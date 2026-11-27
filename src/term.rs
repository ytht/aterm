//! 终端仿真引擎（VT100/xterm 子集，面向 BBS 场景）。
//!
//! 输入：已解码为 `char` 的字符流（字节解码在 net.rs 完成）。
//! 输出：`Term::lines()` 提供的定宽网格 + 光标 + 窗口标题 + 各模式状态。

use std::collections::HashSet;
use std::sync::Arc;
use unicode_width::UnicodeWidthChar;

pub const F_BOLD: u8 = 1;
pub const F_DIM: u8 = 2;
pub const F_ITALIC: u8 = 4;
pub const F_ULINE: u8 = 8;
pub const F_BLINK: u8 = 16;
pub const F_REVERSE: u8 = 32;
pub const F_HIDDEN: u8 = 64;
pub const F_STRIKE: u8 = 128;

/// 单元格颜色：保留 ANSI16 语义（配合调色板可切换），同时支持 256 色与真彩色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// 使用调色板索引 0..16
    Pallet(u8),
    /// xterm 256 色索引
    X256(u8),
    Rgb(u8, u8, u8),
}

impl Default for Color {
    fn default() -> Self {
        Color::Pallet(7) // silver，与 fterm PALETTE0 默认前景一致
    }
}

impl Color {
    /// 若为标准 16 色调色板色，返回其索引；否则 None。
    pub fn pallet_idx(&self) -> Option<u8> {
        match *self {
            Color::Pallet(i) => Some(i),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attr {
    pub fg: Color,
    pub bg: Color,
    pub flags: u8,
}

impl Default for Attr {
    fn default() -> Self {
        Attr {
            fg: Color::default(),
            bg: Color::Pallet(0),
            flags: 0,
        }
    }
}

/// 屏幕单元。`cont` 表示该格是宽字符右半（不单独绘制）；`wide` 表示左半。
#[derive(Debug, Clone, Copy)]
pub struct Cell {
    pub ch: char,
    pub attr: Attr,
    pub wide: bool,
    pub cont: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            attr: Attr::default(),
            wide: false,
            cont: false,
        }
    }
}

pub type Line = Vec<Cell>;

/// 解析器状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    EscInt,
    EscCharset, // 等待 ESC ( X 的 X
    Csi,
    Osc,
    Soc, // OSC 内已见 ESC，等待 \
    Dcs,
    DcsEsc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Underline,
    Bar,
}

/// 光标形状（DECSCUSR: `CSI Ps SP q`）
#[derive(Debug, Clone)]
pub struct Term {
    pub cols: u16,
    pub rows: u16,

    pub lines: Vec<Line>,
    /// 备用屏幕内容，进入 smcup 时与主屏互换
    alt_lines: Option<Vec<Line>>,
    pub cx: usize,
    pub cy: usize,
    pub saved_cx: usize,
    pub saved_cy: usize,
    pub saved_attr: Attr,
    pub on_alt: bool,

    pub margin_top: usize,
    pub margin_bot: usize,
    pub margin_left: usize,
    pub margin_right: usize,

    pub pen: Attr,
    pub cursor_visible: bool,
    pub cursor_style: CursorStyle,
    /// DECCKM：光标键发 ESC O 而非 ESC [
    pub app_cursor: bool,
    /// DECKPAM：数字键盘发 ESC O
    pub app_keypad: bool,
    pub auto_wrap: bool,
    /// 等待下一格换行（右侧溢出时置位）
    pub wrap_pending: bool,
    pub insert_mode: bool,
    pub origin_mode: bool,
    pub bracket_paste: bool,
    pub title: String,
    pub bell: bool,
    pub reverse_video: bool,
    /// DECSCNM，1=白底黑字
    pub screen_reverse: bool,

    pub mouse_mode: MouseMode,
    pub dec_hands: bool,
    /// 收到 URL 提示时的记录（fterm 的 PicRules 简化版：仅记录 OSC 8 超链接）
    pub osc8_uri: Option<String>,

    scrollback: Vec<Arc<Line>>,
    scrollback_limit: usize,

    st: State,
    params: Vec<i32>,
    cur_param: i64,
    has_param: bool,
    priv_char: Option<char>,
    inter: Vec<char>,
    osc_buf: String,
    utf8_esc_seen: bool,

    // 保存的状态（用于 DECRC/DECSC 与 alt screen 切换）
    saved_wrap_pending: bool,
    saved_margin_top: usize,
    saved_margin_bot: usize,
    saved_margin_left: usize,
    saved_margin_right: usize,

    pub modes_dec: HashSet<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    Off,
    /// DECSET 1000 按上下键报告
    PressRelease,
    /// 1002 拖动
    Drag,
    /// 1003 任意移动
    AnyMotion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 需要向远端发送的字节（已由 UI 层按当前编码处理，此处为控制序列）
    Send(Vec<u8>),
    Bell,
    Title,
}

impl Term {
    pub fn new(cols: u16, rows: u16) -> Term {
        let cols = cols.max(2);
        let rows = rows.max(2);
        let mut t = Term {
            cols: cols as u16,
            rows: rows as u16,
            lines: Vec::new(),
            alt_lines: None,
            cx: 0,
            cy: 0,
            saved_cx: 0,
            saved_cy: 0,
            saved_attr: Attr::default(),
            on_alt: false,
            margin_top: 0,
            margin_bot: (rows as usize) - 1,
            margin_left: 0,
            margin_right: (rows as usize) * 0 + (cols as usize) - 1,
            pen: Attr::default(),
            cursor_visible: true,
            cursor_style: CursorStyle::Block,
            app_cursor: false,
            app_keypad: false,
            auto_wrap: true,
            wrap_pending: false,
            insert_mode: false,
            origin_mode: false,
            bracket_paste: false,
            title: String::new(),
            bell: false,
            reverse_video: false,
            screen_reverse: false,
            mouse_mode: MouseMode::Off,
            dec_hands: false,
            osc8_uri: None,
            scrollback: Vec::new(),
            scrollback_limit: 2000,
            st: State::Ground,
            params: Vec::new(),
            cur_param: 0,
            has_param: false,
            priv_char: None,
            inter: Vec::new(),
            osc_buf: String::new(),
            utf8_esc_seen: false,
            saved_wrap_pending: false,
            saved_margin_top: 0,
            saved_margin_bot: (rows as usize) - 1,
            saved_margin_left: 0,
            saved_margin_right: (cols as usize) - 1,
            modes_dec: HashSet::new(),
        };
        t.reset(true);
        t
    }

    pub fn set_scrollback_limit(&mut self, n: usize) {
        self.scrollback_limit = n;
        if self.scrollback.len() > n {
            let cut = self.scrollback.len() - n;
            self.scrollback.drain(0..cut);
        }
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn scrollback_line(&self, i: usize) -> Option<&Line> {
        self.scrollback.get(i).map(|a| a.as_ref())
    }

    pub fn clear_scrollback(&mut self) {
        self.scrollback.clear();
    }

    /// 硬复位（RST / 重连）。soft=false 时同时清空回滚缓冲。
    pub fn reset(&mut self, hard: bool) {
        self.pen = Attr::default();
        self.cx = 0;
        self.cy = 0;
        self.margin_top = 0;
        self.margin_bot = self.rows as usize - 1;
        self.margin_left = 0;
        self.margin_right = self.cols as usize - 1;
        self.auto_wrap = true;
        self.insert_mode = false;
        self.origin_mode = false;
        self.app_cursor = false;
        self.app_keypad = false;
        self.bracket_paste = false;
        self.mouse_mode = MouseMode::Off;
        self.cursor_visible = true;
        self.cursor_style = CursorStyle::Block;
        self.on_alt = false;
        self.alt_lines = None;
        self.st = State::Ground;
        self.params.clear();
        self.wrap_pending = false;
        self.screen_reverse = false;
        self.lines = vec![self.blank_line(); self.rows as usize];
        if hard {
            self.scrollback.clear();
            self.title.clear();
            self.modes_dec.clear();
        }
    }

    fn blank_line(&self) -> Line {
        vec![
            Cell {
                ch: ' ',
                attr: Attr {
                    fg: self.pen.fg,
                    bg: self.pen.bg,
                    flags: 0,
                },
                wide: false,
                cont: false,
            };
            self.cols as usize
        ]
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(20) as usize;
        let rows = rows.max(6) as usize;
        if cols == self.cols as usize && rows == self.rows as usize {
            return;
        }
        for l in self.lines.iter_mut() {
            l.resize(cols, Cell::default());
        }
        while self.lines.len() < rows {
            self.lines.push(vec![Cell::default(); cols]);
        }
        while self.lines.len() > rows {
            self.lines.remove(0);
        }
        if let Some(al) = self.alt_lines.as_mut() {
            for l in al.iter_mut() {
                l.resize(cols, Cell::default());
            }
            while al.len() < rows {
                al.push(vec![Cell::default(); cols]);
            }
            while al.len() > rows {
                al.remove(0);
            }
        }
        self.cols = cols as u16;
        self.rows = rows as u16;
        self.margin_right = cols - 1;
        self.margin_bot = rows - 1;
        self.cx = self.cx.min(cols - 1);
        self.cy = self.cy.min(rows - 1);
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut ev = Vec::new();
        for &b in bytes {
            self.step(b, &mut ev);
        }
        ev
    }

    /// 顶层入口：喂入已经解码好的字符（BBS 的 GBK/UTF8 解码在 net.rs 完成）。
    /// ASCII（含全部控制字符）走状态机，非 ASCII 仅在 Ground 状态直接打印。
    pub fn feed_chars(&mut self, text: &str) -> Vec<Event> {
        let mut ev = Vec::new();
        for ch in text.chars() {
            if ch < '\u{80}' {
                self.step(ch as u8, &mut ev);
            } else if self.st == State::Ground {
                self.print_char(ch, &mut ev);
            }
        }
        ev
    }

    fn step(&mut self, b: u8, ev: &mut Vec<Event>) {
        // 引擎内部按字节处理转义序列；可打印字节直接按 char 输出（Latin-1 兜底，
        // 上层若已解码则使用 feed_chars 路径）。
        match self.st {
            State::Ground => self.step_ctl(b, ev),
            State::Esc => self.esc(b, ev),
            State::EscInt => self.esc_int(b),
            State::EscCharset => self.esc_charset(b),
            State::Csi => self.csi(b, ev),
            State::Osc => self.osc(b, ev),
            State::Soc => self.soc(b),
            State::Dcs => self.dcs(b),
            State::DcsEsc => self.dcs_esc(b),
        }
    }

    fn step_ctl(&mut self, b: u8, ev: &mut Vec<Event>) {
        match b {
            0x07 => {
                self.bell = true;
                ev.push(Event::Bell);
            }
            0x08 => {
                self.backspace();
            }
            0x09 => self.tab_forward(),
            0x0a | 0x0b | 0x0c => self.line_feed(),
            0x0d => {
                self.cx = 0;
                self.wrap_pending = false;
            }
            0x0e => {} // SO: 简化，不支持 G1 切换
            0x0f => {}
            0x1b => {
                self.st = State::Esc;
            }
            0x18 | 0x1a => {
                self.st = State::Ground;
            }
            b if b == 0x7f => {
                self.backspace();
            }
            _ if b >= 0x20 => {
                let ch = b as char;
                self.print_char(ch, ev);
            }
            _ => {}
        }
    }

    fn print_char(&mut self, ch: char, _ev: &mut Vec<Event>) {
        // CJK 宽度：中文/制表符/块元素=2，ASCII=1（与 fterm 的 CP936 全角一致）
        let w = UnicodeWidthChar::width_cjk(ch).unwrap_or(1).max(1);
        if self.wrap_pending && self.auto_wrap {
            self.cx = 0;
            self.line_feed();
            self.wrap_pending = false;
        }
        if w >= 2 && self.cx + 1 >= self.cols as usize {
            if self.auto_wrap {
                // 放不下整个宽字符：换行后写
                self.cx = 0;
                self.line_feed();
            } else {
                // 不自动换行时按单格截断写入，光标只推进一格，
                // 避免后续绝对定位内容整体错位（BBS 行尾竖条根因）
                let row = self.cy.min(self.lines.len() - 1);
                let col = self.cx.min(self.cols as usize - 1);
                let attr = self.effective_attr();
                self.lines[row][col] = Cell { ch, attr, wide: false, cont: false };
                self.cx = col + 1;
                if self.cx >= self.cols as usize {
                    self.cx = self.cols as usize - 1;
                    self.wrap_pending = self.auto_wrap;
                }
                return;
            }
        }
        if self.insert_mode {
            self.insert_cells(w);
        }
        let row = self.cy.min(self.lines.len() - 1);
        let col = self.cx.min(self.cols as usize - 1);
        let attr = self.effective_attr();
        if w >= 2 {
            self.lines[row][col] = Cell {
                ch,
                attr,
                wide: true,
                cont: false,
            };
            if col + 1 < self.cols as usize {
                self.lines[row][col + 1] = Cell {
                    ch: '\u{0}',
                    attr,
                    wide: false,
                    cont: true,
                };
                self.cx = col + 1;
            }
        } else {
            self.lines[row][col] = Cell {
                ch,
                attr,
                wide: false,
                cont: false,
            };
        }
        self.cx += 1;
        if self.cx >= self.cols as usize {
            self.cx = self.cols as usize - 1;
            self.wrap_pending = self.auto_wrap;
        }
    }

    fn effective_attr(&self) -> Attr {
        if self.screen_reverse {
            let mut a = self.pen;
            (a.fg, a.bg) = (a.bg, a.fg);
            a
        } else if self.reverse_video {
            let mut a = self.pen;
            a.flags |= F_REVERSE;
            a
        } else {
            self.pen
        }
    }

    fn backspace(&mut self) {
        self.cx = self.cx.saturating_sub(1);
        self.wrap_pending = false;
    }

    fn line_feed(&mut self) {
        if self.cy == self.margin_bot {
            self.scroll_up(1);
        } else if self.cy < (self.rows as usize) - 1 {
            self.cy += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cy == self.margin_top {
            self.scroll_down(1);
        } else if self.cy > 0 {
            self.cy -= 1;
        }
    }

    fn tab_forward(&mut self) {
        let start = self.cx + 1;
        let mut nx = start;
        while nx < self.cols as usize - 1 && !self.tabstop(nx) {
            nx += 1;
        }
        self.cx = nx.min(self.cols as usize - 1);
        self.wrap_pending = false;
    }

    fn tab_back(&mut self) {
        if self.cx == 0 {
            return;
        }
        let mut nx = self.cx - 1;
        while nx > 0 && !self.tabstop(nx) {
            nx -= 1;
        }
        self.cx = nx;
        self.wrap_pending = false;
    }

    fn tabstop(&self, i: usize) -> bool {
        i % 8 == 0
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n {
            if self.margin_top == 0 && !self.on_alt {
                let gone = self.lines.remove(0);
                self.scrollback.push(Arc::new(gone));
                if self.scrollback.len() > self.scrollback_limit {
                    self.scrollback.remove(0);
                }
                self.lines.insert(self.margin_bot, self.blank_line());
            } else {
                self.lines.remove(self.margin_top);
                self.lines.insert(self.margin_bot, self.blank_line());
            }
        }
    }

    // ------------------------------------------------ 文档坐标（回滚 + 屏幕）

    /// 可读取的总行数 = 回滚行数 + 当前屏幕行数。
    pub fn doc_len(&self) -> usize {
        self.scrollback.len() + self.lines.len()
    }

    /// 取第 i 个文档行（0 为最早的回滚行）。
    pub fn doc_line(&self, i: usize) -> Arc<Line> {
        if let Some(sb) = self.scrollback.get(i) {
            return sb.clone();
        }
        let j = i.saturating_sub(self.scrollback.len());
        Arc::new(self.lines.get(j).cloned().unwrap_or_else(|| self.blank_line()))
    }

    /// 取文档区间 [from, to) 的纯文本，用于复制。
    pub fn doc_text(&self, from: usize, to: usize) -> String {
        let mut out = String::new();
        for i in from..to.min(self.doc_len()) {
            let l = self.doc_line(i);
            let mut end = l.len();
            while end > 0 && (l[end - 1].ch == ' ' || l[end - 1].ch == '\u{0}') {
                end -= 1;
            }
            for c in &l[..end] {
                if !c.cont {
                    out.push(c.ch);
                }
            }
            out.push('\n');
        }
        out
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n {
            self.lines.remove(self.margin_bot);
            self.lines.insert(self.margin_top, self.blank_line());
        }
    }

    fn erase_cells(&mut self, from: usize, to: usize) {
        let attr = self.effective_attr();
        let row = self.cy.min(self.lines.len() - 1);
        let from = from.min(self.cols as usize);
        let to = to.min(self.cols as usize);
        for i in from..to {
            self.lines[row][i] = Cell {
                ch: ' ',
                attr,
                wide: false,
                cont: false,
            };
        }
    }

    fn erase_in_line(&mut self, mode: usize) {
        match mode {
            0 => self.erase_cells(self.cx, self.cols as usize),
            1 => self.erase_cells(0, self.cx + 1),
            _ => self.erase_cells(0, self.cols as usize),
        }
    }

    pub fn erase_in_display(&mut self, mode: usize) {
        match mode {
            0 => {
                self.erase_in_line(0);
                for r in (self.cy + 1)..self.lines.len() {
                    self.fill_line_blank(r);
                }
            }
            1 => {
                self.erase_in_line(1);
                for r in 0..self.cy {
                    self.fill_line_blank(r);
                }
            }
            2 | 3 => {
                for r in 0..self.lines.len() {
                    self.fill_line_blank(r);
                }
                if mode == 3 {
                    self.scrollback.clear();
                }
            }
            _ => {}
        }
    }

    fn fill_line_blank(&mut self, r: usize) {
        let attr = self.effective_attr();
        self.lines[r] = vec![
            Cell {
                ch: ' ',
                attr,
                wide: false,
                cont: false,
            };
            self.cols as usize
        ];
    }

    fn insert_cells(&mut self, n: usize) {
        let row = self.cy;
        let attr = self.effective_attr();
        for _ in 0..n {
            if self.lines[row].len() > self.cx {
                self.lines[row].insert(self.cx, Cell { ch: ' ', attr, wide: false, cont: false });
                self.lines[row].pop();
            }
        }
    }

    fn delete_cells(&mut self, n: usize) {
        let row = self.cy;
        let attr = self.effective_attr();
        for _ in 0..n {
            if self.cx < self.lines[row].len() {
                self.lines[row].remove(self.cx);
                self.lines[row].push(Cell { ch: ' ', attr, wide: false, cont: false });
            }
        }
    }

    fn insert_lines(&mut self, n: usize) {
        for _ in 0..n {
            if self.cy <= self.margin_bot {
                self.lines.remove(self.margin_bot);
                self.lines.insert(self.cy, self.blank_line());
            }
        }
    }

    fn delete_lines(&mut self, n: usize) {
        for _ in 0..n {
            if self.cy <= self.margin_bot {
                self.lines.remove(self.cy);
                self.lines.insert(self.margin_bot, self.blank_line());
            }
        }
    }

    fn set_csi_cursor(&mut self, r: usize, c: usize) {
        let top = if self.origin_mode { self.margin_top } else { 0 };
        let bot = if self.origin_mode { self.margin_bot + 1 } else { self.rows as usize };
        self.cy = (top + r).clamp(top, bot - 1);
        self.cx = (c).clamp(0, self.cols as usize - 1);
        self.wrap_pending = false;
    }

    // ---------------- 解析器 ----------------

    fn esc(&mut self, b: u8, ev: &mut Vec<Event>) {
        match b {
            b'[' => {
                self.st = State::Csi;
                self.params.clear();
                self.cur_param = 0;
                self.has_param = false;
                self.priv_char = None;
                self.inter.clear();
            }
            b']' => {
                self.st = State::Osc;
                self.osc_buf.clear();
            }
            b'P' | b'X' | b'^' | b'_' => {
                self.st = State::Dcs;
            }
            b'(' | b')' | b'*' | b'+' => {
                self.st = State::EscCharset;
            }
            b'#' | b'%' => {
                self.st = State::EscInt;
            }
            b'7' => {
                self.saved_cx = self.cx;
                self.saved_cy = self.cy;
                self.saved_attr = self.pen;
                self.st = State::Ground;
            }
            b'8' => {
                self.cx = self.saved_cx;
                self.cy = self.saved_cy;
                self.pen = self.saved_attr;
                self.st = State::Ground;
            }
            b'D' => {
                self.line_feed();
                self.st = State::Ground;
            }
            b'E' => {
                self.cx = 0;
                self.line_feed();
                self.st = State::Ground;
            }
            b'M' => {
                self.reverse_index();
                self.st = State::Ground;
            }
            b'H' => {
                self.tabstop_set();
                self.st = State::Ground;
            }
            b'c' => {
                self.reset(true);
                self.st = State::Ground;
            }
            b'=' => {
                self.app_keypad = true;
                self.st = State::Ground;
            }
            b'>' => {
                self.app_keypad = false;
                self.st = State::Ground;
            }
            b' ' | b'!' => {
                self.inter.clear();
                self.inter.push(b as char);
                self.st = State::EscInt;
            }
            _ => {
                self.st = State::Ground;
            }
        }
        let _ = ev;
    }

    fn tabstop_set(&mut self) {
        // HP 记法：在当前列设置制表位；此处用 8 的倍数固定表位，无需额外状态
    }

    fn esc_int(&mut self, b: u8) {
        // ESC # 3/4 (双倍高) 、ESC % @ (Latin-1) 等：吞掉后续一个字节
        let _ = b;
        self.st = State::Ground;
    }

    fn esc_charset(&mut self, b: u8) {
        // DEC 特殊图形字符集 (0) 等：本项目按 Unicode 输出，不做字符集映射，仅消费该字节
        let _ = b;
        self.st = State::Ground;
    }

    fn dcs(&mut self, b: u8) {
        if b == 0x1b {
            self.st = State::DcsEsc;
        }
    }

    fn dcs_esc(&mut self, _b: u8) {
        self.st = State::Ground;
    }

    fn osc(&mut self, b: u8, ev: &mut Vec<Event>) {
        if b == 0x07 {
            self.finish_osc();
            self.st = State::Ground;
            ev.push(Event::Title);
        } else if b == 0x1b {
            self.st = State::Soc;
        } else {
            self.osc_buf.push(b as char);
        }
    }

    fn soc(&mut self, b: u8) {
        if b == b'\\' {
            self.finish_osc();
        }
        self.st = State::Ground;
    }

    fn finish_osc(&mut self) {
        let s = std::mem::take(&mut self.osc_buf);
        let mut parts = s.splitn(2, ';');
        let code = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        match code {
            "0" | "1" | "2" => {
                self.title = rest.to_string();
            }
            "8" => {
                // OSC 8 ; params ; URI
                if rest.is_empty() {
                    self.osc8_uri = None;
                } else if let Some(i) = rest.find(';') {
                    self.osc8_uri = Some(rest[i + 1..].to_string());
                }
            }
            _ => {}
        }
    }

    fn csi(&mut self, b: u8, ev: &mut Vec<Event>) {
        match b {
            b'0'..=b'9' => {
                self.cur_param = self.cur_param * 10 + (b - b'0') as i64;
                if self.cur_param > 100_000 {
                    self.cur_param = 100_000;
                }
                self.has_param = true;
            }
            b';' => {
                self.push_param();
            }
            b':' => {
                self.push_param();
                // 子参数（SGR 38:5:n 之类），用逗号区分以便后续合并
                self.params.push(-9999);
            }
            b'<' | b'=' | b'>' | b'?' | b'!' => {
                if !self.has_param && self.params.is_empty() {
                    self.priv_char = Some(b as char);
                } else {
                    self.inter.push(b as char);
                }
            }
            b' '..=b'/' if b != b' ' => {
                self.inter.push(b as char);
            }
            b' ' => {
                self.inter.push(' ');
            }
            _ => {
                self.push_param();
                self.dispatch_csi(b as char, ev);
                self.st = State::Ground;
            }
        }
    }

    fn push_param(&mut self) {
        if self.has_param {
            self.params.push(self.cur_param as i32);
            self.cur_param = 0;
            self.has_param = false;
        } else {
            // 空参数：保留占位，交由 param(i) 取默认值
            self.params.push(-1);
        }
    }

    fn param(&self, i: usize, def: i32) -> i32 {
        match self.params.get(i) {
            Some(&v) if v >= 0 => v,
            Some(&-9999) => def,
            Some(_) => def,
            None => def,
        }
    }

    fn dispatch_csi(&mut self, f: char, ev: &mut Vec<Event>) {
        let privq = self.priv_char == Some('?');
        match f {
            'A' => self.move_cy(-(self.param(0, 1) as isize)),
            'B' | 'e' => self.move_cy(self.param(0, 1) as isize),
            'C' | 'a' => self.move_cx(self.param(0, 1) as isize),
            'D' => self.move_cx(-(self.param(0, 1) as isize)),
            'E' => {
                self.cx = 0;
                self.move_cy(self.param(0, 1) as isize);
            }
            'F' => {
                self.cx = 0;
                self.move_cy(-(self.param(0, 1) as isize));
            }
            'G' | '`' => {
                self.cx = (self.param(0, 1).max(1) as usize) - 1;
                self.cx = self.cx.min(self.cols as usize - 1);
                self.wrap_pending = false;
            }
            'd' => {
                self.cy = (self.param(0, 1).max(1) as usize) - 1;
                self.cy = self.cy.min(self.rows as usize - 1);
            }
            'H' | 'f' => {
                let r = (self.param(0, 1).max(1) as usize) - 1;
                let c = (self.param(1, 1).max(1) as usize) - 1;
                self.set_csi_cursor(r, c);
            }
            'r' => {
                // DECSTBM 滚动区域
                let t = self.param(0, 1).max(1) as usize - 1;
                let b = if self.params.len() > 1 {
                    (self.param(1, self.rows as i32).max(1) as usize) - 1
                } else {
                    self.rows as usize - 1
                };
                if t < b && b < self.rows as usize {
                    self.margin_top = t;
                    self.margin_bot = b;
                    self.cx = 0;
                    self.cy = if self.origin_mode { t } else { 0 };
                }
            }
            'm' => self.sgr(),
            'J' => self.erase_in_display(self.param(0, 0) as usize),
            'K' => self.erase_in_line(self.param(0, 0) as usize),
            'L' => self.insert_lines(self.param(0, 1) as usize),
            'M' => self.delete_lines(self.param(0, 1) as usize),
            '@' => self.insert_cells(self.param(0, 1) as usize),
            'P' => self.delete_cells(self.param(0, 1) as usize),
            'X' => {
                let n = self.param(0, 1) as usize;
                let to = (self.cx + n).min(self.cols as usize);
                self.erase_cells(self.cx, to);
            }
            'h' | 'l' => {
                let on = f == 'h';
                if privq {
                    for i in 0..self.params.len() {
                        if let Some(p) = self.params.get(i) {
                            if *p >= 0 {
                                self.decset(*p as u16, on);
                            }
                        }
                    }
                } else {
                    for i in 0..self.params.len() {
                        if let Some(&p) = self.params.get(i) {
                            if p >= 0 {
                                self.set_ansi_mode(p as u16, on);
                            }
                        }
                    }
                }
            }
            's' => {
                self.saved_cx = self.cx;
                self.saved_cy = self.cy;
            }
            'u' => {
                self.cx = self.saved_cx;
                self.cy = self.saved_cy;
            }
            't' => {
                // window manipulation，仅支持 #22 保存/恢复标题色
            }
            'S' => self.scroll_up(self.param(0, 1) as usize),
            'T' => self.scroll_down(self.param(0, 1) as usize),
            'g' => {}
            'c' => {
                // DA1 应答由上层回复
                ev.push(Event::Send(format!("\x1b[?6c").into_bytes()));
            }
            'n' => {
                let q = self.param(0, 0);
                if q == 6 {
                    let r = self.cy + 1;
                    let c = self.cx + 1;
                    ev.push(Event::Send(format!("\x1b[{r};{c}R").into_bytes()));
                } else if q == 5 {
                    ev.push(Event::Send(b"\x1b[0n".to_vec()));
                }
            }
            _ => {
                if self.inter.contains(&' ') && (f == 'q' || f == 'p') {
                    // DECSCUSR
                    let s = self.param(0, 0);
                    self.cursor_style = match s {
                        1 | 2 => CursorStyle::Block,
                        3 | 4 => CursorStyle::Underline,
                        5 | 6 => CursorStyle::Bar,
                        _ => CursorStyle::Block,
                    };
                }
            }
        }
    }

    fn move_cx(&mut self, d: isize) {
        let nc = (self.cx as isize + d).clamp(0, self.cols as usize as isize - 1);
        self.cx = nc as usize;
        self.wrap_pending = false;
    }

    fn move_cy(&mut self, d: isize) {
        let nc = (self.cy as isize + d).clamp(0, self.rows as usize as isize - 1);
        self.cy = nc as usize;
        self.wrap_pending = false;
    }

    fn set_ansi_mode(&mut self, m: u16, on: bool) {
        match m {
            4 => self.insert_mode = on,
            12 => {}
            20 => {}
            _ => {}
        }
    }

    fn decset(&mut self, m: u16, on: bool) {
        match m {
            1 => self.app_cursor = on,
            6 => {
                self.origin_mode = on;
                self.cy = if on { self.margin_top } else { 0 };
                self.cx = if on { self.margin_left } else { 0 };
            }
            7 => self.auto_wrap = on,
            12 => {}
            25 => self.cursor_visible = on,
            33 => {}
            4 => {}
            47 | 1047 => {
                self.switch_alt(on, m == 1047);
            }
            1048 => {
                if on {
                    self.saved_cx = self.cx;
                    self.saved_cy = self.cy;
                } else {
                    self.cx = self.saved_cx;
                    self.cy = self.saved_cy;
                }
            }
            1049 => {
                if on {
                    self.saved_cx = self.cx;
                    self.saved_cy = self.cy;
                    self.switch_alt(true, true);
                    self.erase_in_display(2);
                } else {
                    self.switch_alt(false, true);
                    self.cx = self.saved_cx;
                    self.cy = self.saved_cy;
                }
            }
            1000 => {
                self.mouse_mode = if on { MouseMode::PressRelease } else { MouseMode::Off };
            }
            1002 => {
                if on {
                    self.mouse_mode = MouseMode::Drag;
                } else if self.mouse_mode == MouseMode::Drag {
                    self.mouse_mode = MouseMode::Off;
                }
            }
            1003 => {
                self.mouse_mode = if on { MouseMode::AnyMotion } else { MouseMode::Off };
            }
            2004 => self.bracket_paste = on,
            5 => self.dec_hands = on,
            _ => {
                if on {
                    self.modes_dec.insert(m);
                } else {
                    self.modes_dec.remove(&m);
                }
            }
        }
    }

    fn switch_alt(&mut self, on: bool, clear: bool) {
        if on == self.on_alt {
            return;
        }
        if on {
            let blank = vec![self.blank_line(); self.rows as usize];
            self.alt_lines = Some(std::mem::replace(&mut self.lines, blank));
            self.on_alt = true;
            if clear {
                self.erase_in_display(2);
            }
        } else {
            if let Some(old) = self.alt_lines.take() {
                self.lines = old;
            }
            self.on_alt = false;
        }
        self.cx = 0;
        self.cy = 0;
    }

    fn sgr(&mut self) {
        let mut i = 0;
        let ps: Vec<i32> = self.params.clone();
        if ps.is_empty() {
            self.pen = Attr::default();
            return;
        }
        while i < ps.len() {
            let p = ps[i];
            match p {
                -1 | 0 => self.pen = Attr::default(),
                1 => self.pen.flags |= F_BOLD,
                2 => self.pen.flags |= F_DIM,
                3 => self.pen.flags |= F_ITALIC,
                4 => self.pen.flags |= F_ULINE,
                5 | 6 => self.pen.flags |= F_BLINK,
                7 => self.pen.flags |= F_REVERSE,
                8 => self.pen.flags |= F_HIDDEN,
                9 => self.pen.flags |= F_STRIKE,
                21 => self.pen.flags |= F_ULINE,
                22 => self.pen.flags &= !(F_BOLD | F_DIM),
                23 => self.pen.flags &= !F_ITALIC,
                24 => self.pen.flags &= !F_ULINE,
                25 => self.pen.flags &= !F_BLINK,
                27 => self.pen.flags &= !F_REVERSE,
                28 => self.pen.flags &= !F_HIDDEN,
                29 => self.pen.flags &= !F_STRIKE,
                30..=37 => self.pen.fg = Color::Pallet((p - 30) as u8),
                39 => self.pen.fg = Color::default(),
                40..=47 => self.pen.bg = Color::Pallet((p - 40) as u8),
                49 => self.pen.bg = Color::Pallet(0),
                90..=97 => self.pen.fg = Color::Pallet((p - 82) as u8),
                100..=107 => self.pen.bg = Color::Pallet((p - 92) as u8),
                38 | 48 | 58 => {
                    let target = p;
                    if i + 1 < ps.len() {
                        let kind = ps[i + 1];
                        if kind == 5 && i + 2 < ps.len() {
                            let c = Color::X256(ps[i + 2].clamp(0, 255) as u8);
                            self.apply_sgr_color(target, c);
                            i += 2;
                        } else if kind == 2 {
                            let r = ps.get(i + 2).cloned().unwrap_or(0);
                            let g = ps.get(i + 3).cloned().unwrap_or(0);
                            let b = ps.get(i + 4).cloned().unwrap_or(0);
                            self.apply_sgr_color(target, Color::Rgb(r as u8, g as u8, b as u8));
                            i += 4;
                        }
                    }
                }
                53 => {}
                _ => {}
            }
            i += 1;
        }
    }

    fn apply_sgr_color(&mut self, which: i32, c: Color) {
        match which {
            38 => self.pen.fg = c,
            48 => self.pen.bg = c,
            _ => {}
        }
    }

    /// 屏幕最后 n 行的纯文本（用于自动登录提示符匹配）。
    pub fn tail_text(&self, n: usize) -> String {
        let start = self.lines.len().saturating_sub(n);
        let mut s = String::new();
        for l in &self.lines[start..] {
            for c in l.iter().filter(|c| !c.cont) {
                s.push(c.ch);
            }
            s.push('\n');
        }
        s
    }

    /// 整屏纯文本（用于"复制全部"）。
    pub fn screen_text(&self) -> String {
        self.tail_text(self.lines.len())
    }

    pub fn mouse_report(&self, btn: u8, col: usize, row: usize, pressed: bool) -> Option<Vec<u8>> {
        match self.mouse_mode {
            MouseMode::Off => None,
            m => {
                let _ = m;
                if self.modes_dec.contains(&1006) {
                    let code = if pressed { btn } else { btn + 64 };
                    Some(format!("\x1b[<{};{};{}{}", code, col + 1, row + 1, if pressed { "M" } else { "m" }).into_bytes())
                } else {
                    let cb = 32 + btn + if pressed { 0 } else { 3 };
                    Some(vec![0x1b, b'[', b'M', (cb + 32) as u8, (col + 32) as u8, (row + 32) as u8])
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk() -> Term {
        Term::new(80, 24)
    }

    fn row_text(t: &Term, r: usize) -> String {
        t.lines[r]
            .iter()
            .filter(|c| !c.cont)
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn wide_char_at_last_col_no_wrap() {
        let mut t = Term::new(5, 4);
        t.feed(b"\x1b[?7l"); // 关闭自动换行
        t.feed(b"\x1b[1;5H"); // 光标到末列(0基4)
        t.feed_chars("中");
        assert_eq!(t.cx, 4, "末列宽字符截断后光标应停在末列");
        assert!(!t.lines[0][4].wide);
        t.feed(b"\x1b[1;1H");
        t.feed(b"AB");
        assert_eq!(row_text(&t, 0), "AB  中");
    }

    #[test]
    fn wide_char_at_last_col_wrap() {
        let mut t = Term::new(5, 4);
        t.feed(b"\x1b[1;5H");
        t.feed_chars("中");
        assert_eq!(t.cy, 1, "自动换行时宽字符应整体移到下一行");
        assert_eq!(t.cx, 2);
        assert!(t.lines[1][0].wide);
        assert!(t.lines[1][1].cont);
    }

    #[test]
    #[ignore]
    fn replay_rec() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("rec.bin");
        let bytes = std::fs::read(&path).unwrap();
        // 与 net.rs 相同：先剥 IAC，再 GBK 解码
        let mut clean: Vec<u8> = Vec::new();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] != 255 {
                clean.push(bytes[i]);
                i += 1;
                continue;
            }
            if i + 1 >= bytes.len() { break; }
            match bytes[i + 1] {
                255 => { clean.push(255); i += 2; }
                250 => {
                    let mut j = i + 2;
                    while j + 1 < bytes.len() && !(bytes[j] == 255 && bytes[j + 1] == 240) { j += 1; }
                    i = (j + 2).min(bytes.len());
                }
                251 | 252 | 253 | 254 => { i += 3; }
                _ => { i += 2; }
            }
        }
        let (text, _, _) = encoding_rs::GBK.decode(&clean);
        // 全程喂入，每 256 字符转储一次当前屏幕，捕获中间帧
        let mut t = mk();
        let mut out = String::new();
        let mut snap = 0usize;
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            let end = (i + 256).min(chars.len());
            let chunk: String = chars[i..end].iter().collect();
            t.feed_chars(&chunk);
            i = end;
            out.push_str(&format!("===== snap {snap} (upto {i}) =====\n"));
            for r in 0..t.rows as usize {
                let has = t.lines[r].iter().any(|c| c.ch != ' ' && !c.cont);
                if !has { continue; }
                out.push_str(&format!("row{r:02}:"));
                for (ci, cell) in t.lines[r].iter().enumerate() {
                    if cell.ch != ' ' && !cell.cont {
                        out.push_str(&format!(" [{ci}]U+{:04X}", cell.ch as u32));
                    }
                }
                out.push('\n');
            }
            snap += 1;
        }
        std::fs::write(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("replay.txt"), out).unwrap();
    }

    #[test]
    fn plain_text_and_cursor() {
        let mut t = mk();
        t.feed(b"hello");
        assert_eq!(row_text(&t, 0), "hello");
        assert_eq!(t.cx, 5);
    }

    #[test]
    fn crlf_and_wrap() {
        let mut t = Term::new(5, 4);
        t.feed(b"abcdef");
        assert_eq!(row_text(&t, 0), "abcde");
        assert_eq!(row_text(&t, 1), "f");
        t.feed(b"\r\nXY");
        assert_eq!(row_text(&t, 2), "XY");
    }

    #[test]
    fn csi_cursor_and_erase() {
        let mut t = mk();
        t.feed(b"0123456789");
        t.feed(b"\x1b[1;1H\x1b[K");
        assert_eq!(row_text(&t, 0), "");
        t.feed(b"\x1b[1;3H");
        t.feed(b"ZZ");
        assert_eq!(row_text(&t, 0), "  ZZ");
    }

    #[test]
    fn erase_display_standard_semantics() {
        // 标准 ED：无参数/0 清光标到屏尾，2 清全屏。
        // Firebird 站内重绘依赖 H+J 真正清屏，若 J 被当作无操作会残留旧内容。
        let mut t = Term::new(20, 6);
        t.feed(b"ART1\r\nART2\r\nWELCOME\r\nlogin:");
        t.feed(b"\x1b[H\x1b[J"); // 光标归位后清光标以下全部
        assert!(t.screen_text().trim().is_empty(), "H+J 应清屏: {:?}", t.screen_text());
        t.feed(b"NEW");
        t.feed(b"\x1b[2J");
        assert!(t.screen_text().trim().is_empty());
        // 光标在行中时 0J 只清当前行剩余及下方行
        let mut t2 = Term::new(20, 6);
        t2.feed(b"abcdef\r\nsecond");
        t2.feed(b"\x1b[1;3H\x1b[0J");
        assert_eq!(row_text(&t2, 0), "ab");
        assert_eq!(row_text(&t2, 1), "");
    }

    #[test]
    fn sgr_colors_and_attrs() {
        let mut t = mk();
        t.feed(b"\x1b[1;31mR\x1b[0mN");
        assert_eq!(t.lines[0][0].ch, 'R');
        assert_eq!(t.lines[0][0].attr.fg, Color::Pallet(1));
        assert_eq!(t.lines[0][0].attr.flags & F_BOLD, F_BOLD);
        assert_eq!(t.lines[0][1].attr.flags & F_BOLD, 0);
    }

    #[test]
    fn sgr_256_and_truecolor() {
        let mut t = mk();
        t.feed(b"\x1b[38;5;196mA\x1b[38;2;10;20;30mB");
        assert_eq!(t.lines[0][0].attr.fg, Color::X256(196));
        assert_eq!(t.lines[0][1].attr.fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn scroll_region_and_scrollback() {
        let mut t = mk();
        for i in 0..30 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        // 30 行写入 24 行屏幕：line0..6 滚入回滚，line7..29 占屏幕 0..22，
        // 最后一次 \r\n 使光标停在屏幕末行（空行）。
        assert_eq!(t.scrollback_len(), 7);
        assert_eq!(row_text(&t, 22), "line29");
        assert_eq!(row_text(&t, 23), "");
        assert_eq!(t.scrollback_line(0).map(|l| l.iter().filter(|c| !c.cont).map(|c| c.ch).collect::<String>().trim_end().to_string()), Some("line0".to_string()));
    }

    #[test]
    fn alt_screen_preserves_main() {
        let mut t = mk();
        t.feed(b"main screen text");
        t.feed(b"\x1b[?1049h");
        assert!(t.on_alt);
        t.feed(b"alt text");
        t.feed(b"\x1b[?1049l");
        assert!(!t.on_alt);
        assert_eq!(row_text(&t, 0), "main screen text");
    }

    #[test]
    fn osc_title_and_bell() {
        let mut t = mk();
        // 用 ASCII 标题测试 OSC 结束符 BEL 的路径
        let ev = t.feed(b"\x1b]0;SMTH BBS\x07");
        assert_eq!(ev, vec![Event::Title]);
        assert_eq!(t.title, "SMTH BBS");
        // OSC 8 超链接
        let mut t2 = mk();
        let _ = t2.feed(b"\x1b]8;;http://x.y\x07");
        assert_eq!(t2.osc8_uri.as_deref(), Some("http://x.y"));
        let mut t3 = mk();
        let ev3 = t3.feed(b"\x07");
        assert_eq!(ev3, vec![Event::Bell]);
        assert!(t3.bell);
    }

    #[test]
    fn insert_delete_lines() {
        let mut t = mk();
        t.feed(b"AAA\r\nBBB\r\nCCC");
        t.feed(b"\x1b[2;1H");
        t.feed(b"\x1b[1L");
        assert_eq!(row_text(&t, 1), "");
        assert_eq!(row_text(&t, 2), "BBB");
    }

    #[test]
    fn wide_char_occupies_two_cells() {
        let mut t = mk();
        t.feed_chars("中");
        assert!(t.lines[0][0].wide);
        assert!(t.lines[0][1].cont);
        assert_eq!(t.cx, 2);
    }

    #[test]
    fn insert_mode_shifts_text() {
        let mut t = mk();
        t.feed(b"abcd");
        t.feed(b"\x1b[1;1H\x1b[4hXY");
        // 插入模式：XY 插入行首，abcd 右移，80 列足够容纳
        assert_eq!(row_text(&t, 0), "XYabcd");
    }

    #[test]
    fn unknown_csi_does_not_break_stream() {
        let mut t = mk();
        t.feed(b"a\x1b[?1234$y b");
        assert_eq!(row_text(&t, 0), "a b");
    }

    #[test]
    fn da1_and_dsr_responses() {
        let mut t = mk();
        let ev = t.feed(b"\x1b[6n");
        assert!(matches!(&ev[..], [Event::Send(_)]));
        if let Event::Send(b) = &ev[0] {
            assert!(b.starts_with(b"\x1b["));
        }
    }
}

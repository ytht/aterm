//! 图形界面层：网格终端渲染、选区复制、输入法接管、标签页、滚动与右键菜单。

use crate::config::{self, CharEncoding, ColorScheme, Settings, Site};
use crate::dialogs::{self, Dlg};
use crate::net::{self, Cmd, Conn, Phase, SpawnCtx};
use crate::term::{self, Cell, Color, CursorStyle, Line};
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align, Color32, FontData, FontDefinitions, FontFamily, FontId, Panel, Pos2, Rect, Stroke,
    TextFormat, Vec2, pos2, vec2,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const FONT_FAMILY: &str = "fterm";

/// 文档坐标（绝对行号, 列）
pub type DocPos = (usize, usize);

pub struct Tab {
    pub name: String,
    pub site: Site,
    pub conn: Option<Conn>,
    /// 本标签独立字号（属性窗口可实时调整）
    pub font_size: f32,
    /// 本标签字体名（属性窗口可选择系统字体）
    pub font_name: String,
    pub sel_anchor: Option<DocPos>,
    pub sel_head: Option<DocPos>,
    /// 鼠标按下时的格子位置（用于区分单击与划选）
    pub press_at: Option<DocPos>,
    pub dragging: bool,
    pub ime_preedit: String,
    pub menu_at: Option<Pos2>,
}

impl Tab {
    pub fn new(site: Site, conn: Option<Conn>, font_size: f32, font_name: String) -> Tab {
        Tab {
            name: site.name.clone(),
            site,
            conn,
            font_size,
            font_name,
            sel_anchor: None,
            sel_head: None,
            press_at: None,
            dragging: false,
            ime_preedit: String::new(),
            menu_at: None,
        }
    }
}

pub struct App {
    pub cfg: Settings,
    pub cfg_path_shown: String,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub dlg: Dlg,
    pub rt: SpawnCtx,
    pub ctx: egui::Context,
    pub status_line: String,
    pub cell_w: f32,
    pub cell_h: f32,
    pub term_rect: Rect,
    pub blink_on: bool,
    last_blink: f64,
    pub bell_at: f64,
    quit_armed: bool,
    fonts_installed: bool,
    font_bits: u32,
    /// 当前已应用的活动标签字体名（变化时重建字体链）
    font_chain: String,
    /// 系统字体列表 (显示名, 文件路径, ttc 索引)
    pub sys_fonts: Vec<(String, String, u32)>,
    glyph_cache: HashMap<(char, u32, u32, u8, u32), Arc<egui::Galley>>,
    /// 对话框草稿
    pub draft_site: Site,
    pub draft_proxy: Proxy,
    /// 属性窗口对应的标签下标
    pub props_tab: Option<usize>,
    /// 地址簿分组管理草稿
    pub draft_group: String,
    /// 新建分组输入行是否展开（跨帧持久）
    pub show_group_input: bool,
    /// 地址簿中选中的站点下标
    pub selected_site: Option<usize>,
    /// 站点编辑器打开时的原始名称（用于判断更新/新增）
    pub edit_original_name: Option<String>,
    /// 宏匹配缓冲：最近输入的字节，用于匹配多字节触发序列
    macro_buf: Vec<u8>,
    /// 缓冲最后更新时刻（超时则原样发出，避免按键被挂住）
    macro_buf_at: f64,
    /// 触发栏被激活的宏下标：此时下一次按键（含组合键）决定其内容
    pub trigger_armed: Option<usize>,
    /// 关于窗口是否显示
    pub show_about: bool,
    /// SSH 认证弹窗：待连接站点（确认后注入凭据再连接）
    pub ssh_pending: Option<Site>,
    /// SSH 认证弹窗草稿
    pub ssh_user: String,
    pub ssh_pass: String,
    /// 启动后窗口是否已居中（只执行一次）
    centered: bool,
    /// 标题栏图标是否已显式设置成功（未成功则每帧重试）
    pub titlebar_icon_set: bool,
    /// 上一帧窗口是否最大化（最大化时字号自动 20，还原时恢复原字号）
    was_maximized: bool,
    /// 最大化前的字号（还原窗口时恢复）
    pre_max_font_size: f32,
    /// 按键宏全局开关（Ctrl+Shift+M 切换；仅本次运行有效，不写入设置文件）
    macros_global: bool,
}

use crate::config::Proxy;

impl App {
    pub fn new(ctx: egui::Context, rt: SpawnCtx) -> App {
        let cfg = config::load();
        let cfg_path_shown = config::config_path().display().to_string();
        let mut app = App {
            cfg,
            cfg_path_shown,
            tabs: Vec::new(),
            active: 0,
            dlg: Dlg::None,
            rt,
            ctx,
            status_line: String::new(),
            cell_w: 9.0,
            cell_h: 18.0,
            term_rect: Rect::NOTHING,
            blink_on: true,
            last_blink: 0.0,
            bell_at: -1.0,
            quit_armed: false,
            fonts_installed: false,
            font_bits: 0,
            font_chain: String::new(),
            sys_fonts: enum_system_fonts(),
            glyph_cache: HashMap::new(),
            draft_site: Site::default(),
            draft_proxy: Proxy::default(),
            props_tab: None,
            draft_group: String::new(),
            show_group_input: false,
            selected_site: None,
            edit_original_name: None,
            macro_buf: Vec::new(),
            macro_buf_at: 0.0,
            trigger_armed: None,
            show_about: false,
            ssh_pending: None,
            ssh_user: String::new(),
            ssh_pass: String::new(),
            centered: false,
            titlebar_icon_set: false,
            was_maximized: false,
            pre_max_font_size: 0.0,
            macros_global: true,
        };
        app.tabs.push(Tab::new(
            Site {
                name: "欢迎".into(),
                ..Default::default()
            },
            None,
            app.cfg.font_size,
            String::new(),
        ));
        app.status_line =
            "按 F2 打开地址簿选择站点连接".into();
        // 诊断模式：ATERM_SHOT=1 时自动连接首个 Telnet 站点并截两帧存盘
        if std::env::var("ATERM_SHOT").is_ok() {
            // 优先选带代理的站点；ATERM_SHOT=ssh 时选 SSH，否则 Telnet
            let want_ssh = std::env::var("ATERM_SHOT")
                .map(|v| v.eq_ignore_ascii_case("ssh"))
                .unwrap_or(false);
            let want = if want_ssh {
                config::Protocol::Ssh
            } else {
                config::Protocol::Telnet
            };
            let pick = app
                .cfg
                .sites
                .iter()
                .find(|s| s.proto == want && s.proxy.is_some())
                .or_else(|| app.cfg.sites.iter().find(|s| s.proto == want))
                .cloned();
            if let Some(site) = pick {
                app.connect_site(site);
            }
            let ctx2 = app.ctx.clone();
            let conn_for_shot = app.tabs.last().and_then(|t| t.conn.clone());
            let term_for_dump = conn_for_shot.as_ref().map(|c| c.term.clone());
            let status_for_dump = conn_for_shot.as_ref().map(|c| c.status.clone());
            app.rt.spawn(async move {
                let t0 = std::time::Instant::now();
                // 诊断：进站画面后自动 guest 登录，验证站内重绘
                tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                if let Some(c) = &conn_for_shot {
                    c.send(crate::net::Cmd::Bytes(b"guest\r".to_vec()));
                }
                for (at, name) in [(500u64, "shot_early.png"), (2000, "shot_a.png"), (5000, "shot_b.png"), (9000, "shot_c.png")] {
                    let target = std::time::Duration::from_millis(at);
                    let elapsed = t0.elapsed();
                    if target > elapsed {
                        tokio::time::sleep(target - elapsed).await;
                    }
                    let exe_dir = std::env::current_exe().unwrap_or_default().parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
                    let path = exe_dir.join(name);
                    let dump_path = exe_dir.join("dump.txt");
                    let term_clone = term_for_dump.clone();
                    let status_clone = status_for_dump.clone();
                    let do_dump = name == "shot_c.png";
                    let cb = egui::ScreenshotCallback::new(move |img| {
                        let w = img.width() as u32;
                        let h = img.height() as u32;
                        let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                        if let Some(buf) = image::RgbaImage::from_raw(w, h, rgba) {
                            let _ = buf.save(&path);
                        }
                        if do_dump {
                            if let Some(term) = &term_clone {
                                if let Ok(t) = term.lock() {
                                    let mut out = String::new();
                                    if let Some(st) = &status_clone {
                                        if let Ok(s) = st.lock() {
                                            out.push_str(&format!("phase={:?} msg={}\n", s.phase, s.msg));
                                        }
                                    }
                                    out.push_str(&format!("cols={} rows={} doc_len={}\n", t.cols, t.rows, t.doc_len()));
                                    let base = t.doc_len().saturating_sub(t.rows as usize);
                                    for r in 0..t.rows as usize {
                                        let line = t.doc_line(base + r);
                                        out.push_str(&format!("row{r:02}:"));
                                        for (ci, cell) in line.iter().enumerate() {
                                            if cell.ch != ' ' && !cell.cont {
                                                out.push_str(&format!(" [{ci}]U+{:04X}({})", cell.ch as u32, cell.ch));
                                            }
                                        }
                                        out.push('\n');
                                    }
                                    let _ = std::fs::write(&dump_path, out);
                                }
                            }
                        }
                    });
                    ctx2.send_viewport_cmd(egui::ViewportCommand::Screenshot(cb));
                    ctx2.request_repaint();
                }
            });
        }
        app
    }

    // ---------------------------------------------------- 字体与度量

    fn ensure_fonts(&mut self) {
        if self.fonts_installed {
            return;
        }
        self.fonts_installed = true;
        // UI 字体补 CJK，保证菜单中文可见
        let mut fd = FontDefinitions::default();
        for (path, index) in [
            (r"C:\Windows\Fonts\simsun.ttc", 0u32),
            (r"C:\Windows\Fonts\msyh.ttc", 0),
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                let key = format!("ui-{}", index);
                fd.font_data.insert(key.clone(), Arc::new(FontData::from_blob(Arc::new(bytes), index)));
                for fam in [FontFamily::Proportional, FontFamily::Monospace] {
                    let e = fd.families.entry(fam).or_default();
                    if !e.contains(&key) { e.push(key.clone()); }
                }
            }
        }
        self.ctx.set_fonts(fd);
        self.font_chain.clear(); // 强制下次重建终端字体链
    }

    /// 按活动标签的字体名重建终端字体链（主字体 + CJK 回退）
    fn rebuild_font_chain(&mut self) {
        let name = self
            .tabs
            .get(self.active)
            .map(|t| t.font_name.clone())
            .unwrap_or_default();
        if self.font_chain == name {
            return;
        }
        self.font_chain = name.clone();
        let mut fd = FontDefinitions::default();
        let mut chain: Vec<String> = Vec::new();
        // 主字体：用户选择；未选择则默认新宋体
        let primary = if name.is_empty() {
            self.sys_fonts.iter().find(|(n, _, _)| n == "新宋体" || n == "NSimSun").cloned()
                .or_else(|| {
                    // 兜底：直接按文件加载新宋体（simsun.ttc 第 2 面）
                    let p = std::path::Path::new(r"C:\Windows\Fonts\simsun.ttc");
                    if p.exists() {
                        Some(("新宋体".to_string(), p.display().to_string(), 1))
                    } else {
                        None
                    }
                })
        } else {
            self.sys_fonts.iter().find(|(n, _, _)| n == &name).cloned()
        };
        if let Some((_, path, index)) = primary {
            if let Ok(bytes) = std::fs::read(&path) {
                let key = "term-primary".to_string();
                fd.font_data.insert(key.clone(), Arc::new(FontData::from_blob(Arc::new(bytes), index)));
                chain.push(key);
            }
        }
        // CJK 回退
        for (path, index) in [
            (r"C:\Windows\Fonts\simsun.ttc", 0u32),
            (r"C:\Windows\Fonts\msyh.ttc", 0),
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                let key = format!("term-cjk-{}", index);
                fd.font_data.insert(key.clone(), Arc::new(FontData::from_blob(Arc::new(bytes), index)));
                if !chain.contains(&key) { chain.push(key); }
            }
        }
        if chain.is_empty() {
            return;
        }
        fd.families.insert(FontFamily::Name(FONT_FAMILY.into()), chain);
        self.ctx.set_fonts(fd);
        self.font_bits = 0; // 强制重新度量
        self.glyph_cache = HashMap::new();
    }

    pub fn font_id(&self) -> FontId {
        let size = self
            .tabs
            .get(self.active)
            .map(|t| t.font_size)
            .unwrap_or(self.cfg.font_size);
        FontId {
            size,
            family: FontFamily::Name(FONT_FAMILY.into()),
        }
    }

    fn update_metrics(&mut self) {
        self.rebuild_font_chain();
        let size = self
            .tabs
            .get(self.active)
            .map(|t| t.font_size)
            .unwrap_or(self.cfg.font_size);
        let bits = size.to_bits();
        if self.font_bits == bits {
            return;
        }
        self.font_bits = bits;
        let fid = FontId {
            size,
            family: FontFamily::Name(FONT_FAMILY.into()),
        };
        let (w, h) = self.ctx.fonts_mut(|f| {
            let m = f.layout_job(single_job("M", fid.clone(), [255, 255, 255]));
            let c = f.layout_job(single_job("中", fid.clone(), [255, 255, 255]));
            (m.rect.width(), c.rect.height().max(m.rect.height()))
        });
        // 单元格 = ASCII 半角宽；中文/制表符按 2 格（CJK 宽度）铺满
        self.cell_w = w.max(3.0);
        // 显示拉长：早期点阵字体观感偏扁，行高加 50% 后再加 10%（纯显示，不影响编码/字符）
        self.cell_h = (h * 1.65).max(self.cell_w * 1.65).max(8.0);
        self.glyph_cache = HashMap::new();
    }

    // ---------------------------------------------------- 主帧

    fn tick(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // 窗口最大化 → 字号自动 20、立刻生效；还原 → 恢复最大化前字号
        let maximized = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
        if maximized != self.was_maximized {
            if maximized {
                self.pre_max_font_size = self
                    .tabs
                    .get(self.active)
                    .map(|t| t.font_size)
                    .unwrap_or(self.cfg.font_size);
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.font_size = 20.0;
                }
            } else if self.pre_max_font_size > 0.0 {
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.font_size = self.pre_max_font_size;
                }
            }
            self.was_maximized = maximized;
        }
        // 最大化期间新建的连接标签同样用 20 号字（每帧对齐活动标签）
        if maximized
            && let Some(t) = self.tabs.get_mut(self.active)
            && t.font_size != 20.0
        {
            t.font_size = 20.0;
        }
        self.update_metrics();
        self.handle_close(&ctx);

        // 启动时把窗口移到屏幕正中（等首帧拿到窗口/显示器尺寸后执行一次）。
        // 注意：居中底层是 set_outer_position，winit 会顺带清除最大化状态；
        // 因此已最大化时绝不发居中命令，避免把用户的最大化还原掉。
        if !self.centered {
            if maximized {
                self.centered = true;
            } else if let Some(cmd) = egui::ViewportCommand::center_on_screen(&ctx) {
                ctx.send_viewport_cmd(cmd);
                self.centered = true;
            }
        }

        // 显式设置标题栏图标（eframe 自带设置依赖窗口激活时机，可能落空）
        if !self.titlebar_icon_set {
            self.titlebar_icon_set = crate::set_titlebar_icon();
        }

        // 已关闭后端的剪贴板快捷键转换（终端要原始 Ctrl+C/X/V）。
        // 对话框/输入框场景下自行合成剪贴板事件，保证编辑框照常可用。
        if self.dlg != Dlg::None || self.props_tab.is_some() {
            self.synth_clipboard_events(&ctx);
        }

        // 事件快照：面板与对话框先消费，剩余的再交给终端
        dialogs::draw(&ctx, self);

        Panel::top("menubar").show(ui, |ui| self.menu_bar(ui));
        Panel::top("tabbar").show(ui, |ui| self.tab_bar(ui));

        let status = self.status_line.clone();
        let ringing = self.bell_at;
        let now = ctx.input(|i| i.time);
        Panel::bottom("statusbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ringing > 0.0 && now - ringing < 0.6 {
                    ui.colored_label(Color32::from_rgb(255, 210, 80), "● BEL 提示音");
                }
                ui.label(status);
            });
        });

        let mut rect = Rect::NOTHING;
        let term_id = egui::Id::new("term_focus");
        // 标签栏以下、状态栏以上统一纯黑背景（含网格居中后的留白）
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| {
            rect = ui.max_rect();
            // 终端区域作为可聚焦控件：点击终端即取得焦点
            let resp = ui.interact(rect, term_id, egui::Sense::click());
            if resp.clicked() {
                ui.memory_mut(|m| m.request_focus(term_id));
            }
            let idx = self.active;
            if idx >= self.tabs.len() {
                return;
            }
            // 终端网格在窗口内居中显示（原为左上对齐）
            if let Some(c) = &self.tabs[idx].conn
                && let Ok(t) = c.term.lock()
            {
                let gw = t.cols as f32 * self.cell_w;
                let gh = t.rows as f32 * self.cell_h;
                let ox = ((rect.width() - gw) / 2.0).max(0.0);
                let oy = ((rect.height() - gh) / 2.0).max(0.0);
                rect = Rect::from_min_size(
                    pos2(rect.left() + ox, rect.top() + oy),
                    vec2(gw.min(rect.width()), gh.min(rect.height())),
                );
            }
            let mut tab =
                std::mem::replace(&mut self.tabs[idx], Tab::new(Site::default(), None, self.cfg.font_size, String::new()));
            match tab.conn.clone() {
                Some(conn) => {
                    self.pump(&conn, &mut tab);
                    self.draw_screen(ui, &conn, &mut tab, rect);
                }
                None => self.draw_welcome(ui, rect),
            }
            self.tabs[idx] = tab;
        });
        self.term_rect = rect;

        // 属性窗口（实时调整字号/编码）
        if let Some(ti) = self.props_tab {
            self.show_props(ti);
        }
        if self.show_about {
            self.about_window(&ctx);
        }

        // 终端焦点接管：无对话框/属性窗时让终端持有焦点，
        // 并锁定 Tab/方向键/Esc（焦点稳定一帧后过滤器才生效），
        // 使 Tab 等键发给 BBS 而不是被界面焦点导航占用
        let dlg_open = self.dlg != Dlg::None || self.props_tab.is_some();
        ctx.memory_mut(|m| {
            if dlg_open {
                if m.focused() == Some(term_id) {
                    m.surrender_focus(term_id);
                }
                // 触发栏激活时：放弃控件焦点。否则焦点停在触发栏按钮上，
                // 空格/回车会被 egui 当作「点击按钮」吃掉，设定不了空格触发键
                if self.trigger_armed.is_some()
                    && let Some(fid) = m.focused()
                {
                    m.surrender_focus(fid);
                }
            } else {
                if m.focused().is_none() {
                    m.request_focus(term_id);
                }
                if m.focused() == Some(term_id) {
                    m.set_focus_lock_filter(
                        term_id,
                        egui::EventFilter {
                            tab: true,
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            escape: true,
                        },
                    );
                }
            }
        });

        let events = ctx.input(|i| i.events.clone());
        let widget_focus = ctx.memory(|m| m.focused().is_some());
        let term_focused = ctx.memory(|m| m.focused() == Some(term_id));
        // 触发栏激活中：下一次按键（含组合键）决定该栏内容，不发往远端
        if let Some(i) = self.trigger_armed {
            let mut got: Vec<u8> = Vec::new();
            let mut cancel = false;
            // 第一趟：只看按键事件（组合键优先，避免被同帧的文本事件覆盖）
            for ev in &events {
                if let egui::Event::Key { key, pressed: true, modifiers, .. } = ev {
                    // 单独按下修饰键不算内容，等真正的按键
                    if matches!(
                        key,
                        egui::Key::ShiftLeft
                            | egui::Key::ShiftRight
                            | egui::Key::ControlLeft
                            | egui::Key::ControlRight
                            | egui::Key::AltLeft
                            | egui::Key::AltRight
                            | egui::Key::SuperLeft
                            | egui::Key::SuperRight
                    ) {
                        continue;
                    }
                    if *key == egui::Key::Escape && !modifiers.any() {
                        cancel = true;
                        break;
                    }
                    let b = if modifiers.ctrl && !modifiers.alt {
                        if let Some(d) = ctrl_digit(*key) {
                            let mark = if numpad_digit_down(*key) { b'#' } else { b'!' };
                            vec![0x1b, mark, b'0' + d]
                        } else {
                            ctrl_byte(*key).unwrap_or_default()
                        }
                    } else {
                        let mut v = base_seq(*key, modifiers.shift, '[');
                        if v.is_empty() {
                            continue;
                        }
                        if modifiers.alt {
                            let mut w = vec![0x1b];
                            w.extend_from_slice(&v);
                            v = w;
                        }
                        v
                    };
                    if !b.is_empty() {
                        got = b;
                        break; // 只取一次按键
                    }
                }
            }
            // 第二趟：本帧没有可用按键事件时，才用文本（普通字符/中文）
            if !cancel && got.is_empty() {
                for ev in &events {
                    if let egui::Event::Text(s) = ev
                        && !s.is_empty()
                    {
                        got = s.as_bytes().to_vec();
                        break;
                    }
                }
            }
            if cancel {
                self.trigger_armed = None;
            } else if !got.is_empty() {
                let spec = config::bytes_to_keyspec(&got);
                if let Some(m) = self.cfg.macros.get_mut(i) {
                    m.trigger = spec;
                }
                self.trigger_armed = None;
                self.save_cfg();
            }
            if cancel || !got.is_empty() {
                // 消费本帧键盘事件，避免同时落入输入框或远端
                ctx.input_mut(|i| {
                    i.events.retain(|e| {
                        !matches!(
                            e,
                            egui::Event::Key { pressed: true, .. } | egui::Event::Text(_)
                        )
                    })
                });
            }
            ctx.request_repaint_after(Duration::from_millis(60));
            return;
        }
        // 宏只在终端界面激活（无对话框、终端持有焦点）时才参与匹配
        // 指针仍悬停在终端区域时，即使焦点被界面控件拿走（发帖等场景），
        // 也照常处理终端按键/点击，避免 Ctrl+Insert、Shift+Insert、单击 URL 失效
        let pointer_over_term = ctx.input(|i| {
            i.pointer
                .latest_pos()
                .map(|p| rect.contains(p))
                .unwrap_or(false)
        });
        let term_active = self.dlg == Dlg::None
            && self.props_tab.is_none()
            && (!widget_focus || term_focused || pointer_over_term);
        if term_active && ctx.input(|i| i.focused) {
            self.feed(&ctx, &events, rect);
        } else if !self.macro_buf.is_empty() {
            // 离开终端时把挂起的前缀字节补发，避免丢键
            let out = std::mem::take(&mut self.macro_buf);
            if let Some(c) = self.active_conn() {
                c.send(Cmd::Bytes(out));
            }
        }
        self.flush_macro_buf(ctx.input(|i| i.time));
        ctx.request_repaint_after(Duration::from_millis(60));
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        let requested = ctx.input(|i| i.viewport().close_requested());
        if !requested {
            return;
        }
        let alive = self
            .tabs
            .iter()
            .filter_map(|t| t.conn.as_ref())
            .any(|c| c.status().phase.alive());
        if alive && self.cfg.confirm_quit && !self.quit_armed {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quit_armed = true;
            self.status_line = "仍有活动连接：再次点击关闭将退出 ATerm".into();
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("地址簿 F2").clicked() {
                self.dlg = Dlg::AddressBook;
            }
            if ui.button("设置 F4").clicked() {
                self.dlg = Dlg::Settings;
            }
            ui.separator();
            if ui.button("新标签").clicked() {
                let mut s = self.current_site();
                if s.host.is_empty() {
                    s = self.cfg.sites.first().cloned().unwrap_or(s);
                }
                if !s.host.is_empty() {
                    self.connect_site(s);
                } else {
                    self.status_line = "没有可连接的站点，请先新建站点".into();
                }
            }
            if ui.button("重连").clicked() {
                self.reconnect_active();
            }
            if ui.button("断开").clicked() {
                if let Some(c) = self.active_conn() {
                    c.send(Cmd::Close);
                }
            }
            ui.separator();
            if ui.button("关于").clicked() {
                self.show_about = true;
            }
        });
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close_idx: Option<usize> = None;
        let mut props_idx: Option<usize> = None;
        ui.horizontal(|ui| {
            let n = self.tabs.len();
            for i in 0..n {
                let (name, phase, host) = {
                    let t = &self.tabs[i];
                    (
                        t.name.clone(),
                        t.conn
                            .as_ref()
                            .map(|c| c.status().phase)
                            .unwrap_or(Phase::Closed),
                        t.site.host.clone(),
                    )
                };
                let dot = match phase {
                    Phase::Connected => "●",
                    Phase::Connecting | Phase::Negotiating => "○",
                    Phase::Failed => "×",
                    Phase::Closed => "·",
                };
                let resp = ui
                    .selectable_label(i == self.active, format!("{dot} {name}"))
                    .on_hover_text(if host.is_empty() {
                        "本地页：F2 选择站点".to_string()
                    } else {
                        host.clone()
                    });
                if resp.clicked() {
                    self.active = i;
                }
                resp.context_menu(|ui| {
                    if ui.button("关闭标签").clicked() {
                        close_idx = Some(i);
                        ui.close();
                    }
                    if ui.button("属性…").clicked() {
                        props_idx = Some(i);
                        ui.close();
                    }
                });
            }
        });
        if let Some(i) = close_idx {
            self.close_tab(i);
        }
        if let Some(i) = props_idx {
            self.active = i;
            self.props_tab = Some(i);
        }
    }

    fn pump(&mut self, conn: &Conn, tab: &mut Tab) {
        for msg in conn.poll() {
            match msg {
                net::UiMsg::Bell => self.bell_at = self.ctx.input(|i| i.time),
                net::UiMsg::Note(s) => {
                    if !s.is_empty() {
                        self.status_line = s;
                    }
                }
            }
        }
        let st = conn.status();
        // 代理 / 直连 标识
        let via = match conn.site.proxy.and_then(|i| self.cfg.proxies.get(i)) {
            Some(px) if !px.host.trim().is_empty() && px.port > 0 => "代理",
            _ => "直连",
        };
        self.status_line = format!(
            "{} {}:{} · {} · {} · {} · {}",
            tab.name,
            conn.site.host,
            conn.site.port,
            conn.site.proto.label(),
            conn.site.encoding.label(),
            via,
            st.msg,
        );
    }

    fn rows_visible(&self) -> usize {
        if self.term_rect.is_negative() {
            return 24;
        }
        ((self.term_rect.height() / self.cell_h).floor() as usize).clamp(1, 400)
    }

    // ---------------------------------------------------- 欢迎页

    fn draw_welcome(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let scheme = self.cfg.scheme().clone();
        ui.painter().rect_filled(rect, 0.0, rgb(scheme.bg));
        let fg = rgb(scheme.fg);
        let fid = self.font_id();
        let ch = self.cell_h;
        let painter = ui.painter();
        let job = LayoutJob::simple_format(
            "ATerm -- AI写的Term".to_string(),
            TextFormat {
                font_id: fid,
                color: fg,
                ..Default::default()
            },
        );
        let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
        painter.galley(
            pos2(rect.left() + ch, rect.top() + ch * 1.5),
            g,
            fg,
        );
    }

    // ---------------------------------------------------- 终端绘制

    fn draw_screen(&mut self, ui: &mut egui::Ui, conn: &Conn, tab: &mut Tab, rect: Rect) {
        let scheme = self.cfg.scheme().clone();
        let (cw, ch) = (self.cell_w, self.cell_h);
        let mut painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, rgb(scheme.bg));
        // 裁剪到终端区域，超出视口的列/行不绘制
        painter.set_clip_rect(rect);

        // 终端网格固定为站点尺寸（BBS 按 80×24 定位），不随窗口缩放
        let (term_cols, term_rows) = {
            let Ok(t) = conn.term.lock() else { return };
            (t.cols as usize, t.rows as usize)
        };

        let (total, cx, cy, visible, style) = {
            let Ok(t) = conn.term.lock() else { return };
            (t.doc_len(), t.cx, t.cy, t.cursor_visible, t.cursor_style)
        };

        // BBS 不做历史回溯：只显示当前屏幕 term_rows 行、顶置
        let start = total.saturating_sub(term_rows);
        let vis = total.saturating_sub(start).min(term_rows);

        let lines: Vec<Arc<Line>> = {
            let Ok(t) = conn.term.lock() else { return };
            (0..vis).map(|i| t.doc_line(start + i)).collect()
        };
        let sel = norm_sel(tab.sel_anchor, tab.sel_head);

        for ri in 0..vis {
            let line = &lines[ri];
            let y = rect.top() + ri as f32 * ch;
            let doc_row = start + ri;
            let mut ci = 0usize;
            // 按终端实际列数绘制（超出视口的由 clip_rect 裁剪）
            while ci < line.len().min(term_cols) {
                let cell = &line[ci];
                if cell.cont {
                    ci += 1;
                    continue;
                }
                let wcells = if cell.wide { 2 } else { 1 };
                let in_sel = sel
                    .map(|((r0, c0), (r1, c1))| {
                        (doc_row, ci) <= (r1, c1) && (doc_row, ci + wcells - 1) >= (r0, c0)
                    })
                    .unwrap_or(false);
                self.draw_cell(
                    &painter,
                    cell,
                    pos2(rect.left() + ci as f32 * cw, y),
                    &scheme,
                    in_sel,
                    wcells,
                );
                ci += wcells;
            }
        }

        // 光标 + IME
        if visible {
            let now = self.ctx.input(|i| i.time);
            if self.cfg.cursor_blink && now - self.last_blink > 0.53 {
                self.blink_on = !self.blink_on;
                self.last_blink = now;
            }
            let cx = cx.min(term_cols.saturating_sub(1));
            let cy = cy.min(term_rows.saturating_sub(1));
            let cr = Rect::from_min_size(
                pos2(rect.left() + cx as f32 * cw, rect.top() + cy as f32 * ch),
                vec2(cw, ch),
            );
            let col = rgb(scheme.cursor);
            if !self.cfg.cursor_blink || self.blink_on {
                match style {
                    CursorStyle::Block => {
                        painter.rect_filled(cr, 0.0, col.linear_multiply(0.85));
                    }
                    CursorStyle::Underline => {
                        painter.line_segment(
                            [pos2(cr.left(), cr.bottom() - 1.0), pos2(cr.right(), cr.bottom() - 1.0)],
                            Stroke::new(1.5_f32, col),
                        );
                    }
                    CursorStyle::Bar => {
                        painter.line_segment(
                            [pos2(cr.left() + 1.0, cr.top()), pos2(cr.left() + 1.0, cr.bottom())],
                            Stroke::new(2.0_f32, col),
                        );
                    }
                }
            }
            if self.dlg == Dlg::None {
                let ime = egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Terminal,
                    rect: cr,
                    cursor_rect: cr,
                    should_interrupt_composition: false,
                };
                self.ctx.output_mut(|o| o.ime = Some(ime));
            }
            if !tab.ime_preedit.is_empty() {
                let mut job = LayoutJob::default();
                job.append(
                    &tab.ime_preedit,
                    0.0,
                    TextFormat {
                        font_id: self.font_id(),
                        color: Color32::from_rgb(255, 255, 130),
                        ..Default::default()
                    },
                );
                job.wrap = no_wrap();
                let g = self.ctx.fonts_mut(|f| f.layout_job(job));
                painter.galley(cr.min, g, Color32::from_rgb(255, 255, 130));
            }
        }

        if let Some(at) = tab.menu_at.take() {
            self.show_menu(at);
        }
    }

    fn draw_cell(
        &mut self,
        painter: &egui::Painter,
        cell: &Cell,
        at: Pos2,
        scheme: &ColorScheme,
        in_sel: bool,
        wcells: usize,
    ) {
        let (cw, ch) = (self.cell_w, self.cell_h);
        let a = cell.attr;
        let mut fg = resolve(a.fg, scheme);
        let mut bg = resolve(a.bg, scheme);
        if a.flags & term::F_REVERSE != 0 {
            std::mem::swap(&mut fg, &mut bg);
        }
        if a.flags & term::F_BOLD != 0 {
            if let Color::Pallet(i) = a.fg
                && i < 8
            {
                fg = scheme.pallet(i + 8);
            }
        }
        if a.flags & term::F_DIM != 0 {
            fg = [
                (fg[0] as u16 * 2 / 3) as u8,
                (fg[1] as u16 * 2 / 3) as u8,
                (fg[2] as u16 * 2 / 3) as u8,
            ];
        }
        if in_sel {
            bg = scheme.selection_rgb();
        }
        let rect = Rect::from_min_size(at, vec2(cw * wcells as f32, ch));
        if bg != scheme.bg || in_sel {
            painter.rect_filled(rect, 0.0, rgb(bg));
        }
        if cell.ch == ' ' || cell.ch == '\u{0}' {
            return;
        }
        // 块元素/三角元素（U+2580–259F）字体字形不铺满整格，程序化满格绘制；
        // 制表符（U+2500–257F）用字体渲染（新宋体制表符全角平直），不用图形代替。
        let cc = cell.ch as u32;
        if (0x2580..=0x259F).contains(&cc) {
            draw_block(&painter, cell.ch, at, cw * wcells as f32, ch, rgb(fg));
            return;
        }
        let deco = a.flags & (term::F_ULINE | term::F_STRIKE | term::F_ITALIC);
        // 早期 term（fterm/cterm/FQTerm）的中文点阵字库把注音 ㄧ 画作竖笔，
        // ANSI 作品据此拿它当竖线用；现代矢量字体是横笔，故渲染时旋转 90°。
        // 仍用字体字形，不改编码、不以图形替代字符。
        let rotate = cell.ch == 'ㄧ';
        let fid = self.font_id();
        let key = (cell.ch, pack(fg), pack(bg), deco, fid.size.to_bits());
        if !self.glyph_cache.contains_key(&key) {
            let mut job = LayoutJob::default();
            job.append(
                &cell.ch.to_string(),
                0.0,
                TextFormat {
                    font_id: fid,
                    color: rgb(fg),
                    background: Color32::TRANSPARENT,
                    italics: deco & term::F_ITALIC != 0,
                    underline: if deco & term::F_ULINE != 0 {
                        Stroke::new(1.0_f32, rgb(fg))
                    } else {
                        Stroke::NONE
                    },
                    strikethrough: if deco & term::F_STRIKE != 0 {
                        Stroke::new(1.0_f32, rgb(fg))
                    } else {
                        Stroke::NONE
                    },
                    valign: Align::Center,
                    ..Default::default()
                },
            );
            job.wrap = no_wrap();
            job.break_on_newline = false;
            job.keep_trailing_whitespace = true;
            let g = self.ctx.fonts_mut(|f| f.layout_job(job));
            self.glyph_cache.insert(key, g);
        }
        let g = self.glyph_cache[&key].clone();
        // 横向拉伸字形填满单元格（早期点阵字体的 ASCII 字形偏窄，
        // 直接居中会留下字间距；按格宽缩放后与 fterm 的点阵排布一致）
        let cell_px = cw * wcells as f32;
        let gw = g.rect.width().max(0.01);
        let gh = g.rect.height().max(0.01);
        // 细窄字符拉满会显粗、不拉又显细：按字符取折中系数，居中
        let thin_factor = match cell.ch {
            'i' | 'j' | 'l' | 'I' => Some(0.5),
            't' | 'f' | '[' | ']' => Some(0.6),
            '|' => Some(0.7),
            '│' | '║' => Some(0.0), // 竖线不横向拉伸，保持真竖线
            _ => None,
        };
        let full_sx = cell_px / gw;
        let sx = match thin_factor {
            Some(k) => 1.0 + (full_sx - 1.0) * k,
            None => full_sx,
        };
        let sy = ch / gh;
        let off_x = if thin_factor.is_some() { (cell_px - gw * sx).max(0.0) * 0.5 } else { 0.0 };
        // 旋转字符：等比缩放到格内，绕格中心转 90°
        let rs = (cell_px / gh).min(ch / gw);
        if rotate || (sx - 1.0).abs() > 0.01 || (sy - 1.0).abs() > 0.01 {
            // 行网格 UV 是 texel 坐标，需按图集尺寸归一化后才能直接提交
            let [tw, th] = self.ctx.fonts(|f| f.font_image_size());
            let inv = vec2(1.0 / tw.max(1) as f32, 1.0 / th.max(1) as f32);
            let ccx = at.x + cell_px * 0.5;
            let ccy = at.y + ch * 0.5;
            let mut mesh = egui::epaint::Mesh::default();
            for pr in &g.rows {
                let base = mesh.vertices.len() as u32;
                for v in &pr.row.visuals.mesh.vertices {
                    let mut nv = *v;
                    if rotate {
                        let dx = (v.pos.x + pr.pos.x - g.rect.center().x) * rs;
                        let dy = (v.pos.y + pr.pos.y - g.rect.center().y) * rs;
                        nv.pos.x = ccx - dy;
                        nv.pos.y = ccy + dx;
                    } else {
                        nv.pos.x = at.x + off_x + (v.pos.x + pr.pos.x - g.rect.min.x) * sx;
                        nv.pos.y = at.y + (v.pos.y + pr.pos.y - g.rect.min.y) * sy;
                    }
                    nv.uv = pos2(v.uv.x * inv.x, v.uv.y * inv.y);
                    // 行网格顶点色是占位白，必须替换为前景色（与渲染器细分逻辑一致）
                    if nv.color == Color32::PLACEHOLDER {
                        nv.color = rgb(fg);
                    }
                    mesh.vertices.push(nv);
                }
                for idx in &pr.row.visuals.mesh.indices {
                    mesh.indices.push(base + idx);
                }
            }
            mesh.texture_id = g.rows.first().map(|r| r.row.visuals.mesh.texture_id).unwrap_or_default();
            painter.add(egui::Shape::mesh(mesh));
        } else {
            let off = (cell_px - gw).max(0.0) * 0.5;
            painter.galley(pos2(at.x + off, at.y), g, rgb(fg));
        }
    }

    fn show_menu(&mut self, at: Pos2) {
        let idx = self.active;
        let has_sel = self.selection_text(idx).is_some();
        let sel_text = self.selection_text(idx);
        let clip = self.read_clipboard();
        let site = self.tabs.get(idx).map(|t| t.site.clone());
        let conn = self.tabs.get(idx).and_then(|t| t.conn.clone());
        let ctx = self.ctx.clone();

        // 0=无 1=复制 2=粘贴 3=清选区 4=重连 5=编辑站点 6=关闭标签
        let mut action = 0u8;
        egui::Area::new(egui::Id::new("termmenu"))
            .fixed_pos(at)
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    if ui
                        .add_enabled(has_sel, egui::Button::new("复制选区"))
                        .clicked()
                    {
                        action = 1;
                        ui.close();
                    }
                    if ui.button("粘贴").clicked() {
                        action = 2;
                        ui.close();
                    }
                    if ui.button("清除选区").clicked() {
                        action = 3;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("断线重连").clicked() {
                        action = 4;
                        ui.close();
                    }
                    if ui.button("站点编辑…").clicked() {
                        action = 5;
                        ui.close();
                    }
                });
            });

        match action {
            1 => {
                if let Some(t) = sel_text {
                    ctx.copy_text(t);
                }
            }
            2 => {
                if let (Some(c), Some(txt), Some(s)) = (conn, clip, site) {
                    paste(&c, &txt, &s);
                }
            }
            3 => {
                if let Some(t) = self.tabs.get_mut(idx) {
                    t.sel_anchor = None;
                    t.sel_head = None;
                }
            }
            4 => self.reconnect_active(),
            5 => {
                if let Some(s) = site {
                    let idx = self.cfg.sites.iter().position(|x| x.name == s.name);
                    dialogs::edit_site(self, s, idx);
                }
            }
            _ => {}
        }
    }

    fn read_clipboard(&self) -> Option<String> {
        arboard::Clipboard::new()
            .ok()?
            .get_text()
            .ok()
            .map(|s| s.replace("\r\n", "\n"))
    }

    /// 对话框场景：把 Ctrl+C/X/V 转成剪贴板事件（终端场景不转，直发控制码）。
    fn synth_clipboard_events(&mut self, ctx: &egui::Context) {
        let mut add: Vec<egui::Event> = Vec::new();
        let mut drop_keys = false;
        ctx.input(|i| {
            for ev in &i.events {
                if let egui::Event::Key { key, pressed: true, modifiers, .. } = ev {
                    if !modifiers.command_only() {
                        continue;
                    }
                    match key {
                        egui::Key::C => {
                            add.push(egui::Event::Copy);
                            drop_keys = true;
                        }
                        egui::Key::X => {
                            add.push(egui::Event::Cut);
                            drop_keys = true;
                        }
                        egui::Key::V => {
                            if let Ok(mut cb) = arboard::Clipboard::new()
                                && let Ok(t) = cb.get_text()
                                && !t.is_empty()
                            {
                                add.push(egui::Event::Paste(t.replace("\r\n", "\n")));
                            }
                            drop_keys = true;
                        }
                        _ => {}
                    }
                }
            }
        });
        if drop_keys {
            ctx.input_mut(|i| {
                i.events.retain(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => !(modifiers
                        .command_only()
                        && matches!(key, egui::Key::C | egui::Key::X | egui::Key::V)),
                    egui::Event::Text(_) => false,
                    _ => true,
                });
                i.events.extend(add);
            });
        }
    }

    /// 关于窗口：版本号与程序信息
    fn about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_about;
        egui::Window::new("关于 ATerm")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label("ATerm - AI 写的 Term");
                ui.label(format!("版本：{}", env!("CARGO_PKG_VERSION")));
                ui.add_space(4.0);
            });
        self.show_about = open;
    }

    /// 属性窗口：显示并实时调整当前标签的字号与编码
    fn show_props(&mut self, ti: usize) {
        let ctx = self.ctx.clone();
        let mut open = true;
        let mut new_enc: Option<CharEncoding> = None;

        let (name, host, port, proto, cur_enc, cur_fs, cur_font) = match self.tabs.get(ti) {
            Some(t) => (
                t.name.clone(),
                t.site.host.clone(),
                t.site.port,
                t.site.proto.label(),
                t.site.encoding,
                t.font_size,
                t.font_name.clone(),
            ),
            None => {
                self.props_tab = None;
                return;
            }
        };
        let mut new_font: Option<String> = None;

        egui::Window::new(format!("属性 — {name}"))
            .id(egui::Id::new("dlg_props"))
            .open(&mut open)
            .collapsible(false)
            .default_pos([260.0, 180.0])
            .show(&ctx, |ui| {
                ui.label(format!("{host}:{port}  ({proto})"));
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("字体大小");
                    let mut fs = cur_fs;
                    if ui
                        .add(
                            egui::DragValue::new(&mut fs)
                                .range(9.0..=48.0)
                                .speed(0.5)
                                .suffix(" px"),
                        )
                        .changed()
                    {
                        if let Some(t) = self.tabs.get_mut(ti) {
                            t.font_size = fs;
                        }
                        // 立即重算度量
                        self.font_bits = 0;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("字体");
                    let cur = if cur_font.is_empty() { "新宋体（默认）".to_string() } else { cur_font.clone() };
                    egui::ComboBox::from_id_salt("prop_font")
                        .selected_text(cur)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(cur_font.is_empty(), "新宋体（默认）").clicked() {
                                new_font = Some(String::new());
                            }
                            for (n, _, _) in &self.sys_fonts {
                                if ui.selectable_label(cur_font == *n, n).clicked() {
                                    new_font = Some(n.clone());
                                }
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("字符编码");
                    for e in [
                        CharEncoding::Gbk,
                        CharEncoding::Big5,
                        CharEncoding::Utf8,
                        CharEncoding::Latin1,
                    ] {
                        if ui.selectable_label(cur_enc == e, e.label()).clicked() {
                            new_enc = Some(e);
                        }
                    }
                });
                ui.separator();
                ui.label("字号与编码修改立即生效于本窗口，不影响其他标签。");
            });

        if let Some(f) = new_font {
            if let Some(t) = self.tabs.get_mut(ti) {
                t.font_name = f;
            }
            self.font_chain.clear();
            self.font_bits = 0;
        }
        if let Some(enc) = new_enc {
            if let Some(t) = self.tabs.get_mut(ti) {
                t.site.encoding = enc;
                if let Some(c) = &t.conn {
                    c.send(Cmd::SetEncoding(enc));
                }
            }
        }
        if !open {
            self.props_tab = None;
        }
    }

    // ---------------------------------------------------- 会话

    pub fn active_conn(&self) -> Option<Conn> {
        self.tabs.get(self.active).and_then(|t| t.conn.clone())
    }

    pub fn current_site(&self) -> Site {
        self.tabs
            .get(self.active)
            .map(|t| t.site.clone())
            .unwrap_or_default()
    }

    pub fn connect_site(&mut self, site: Site) {
        if site.host.trim().is_empty() {
            self.status_line = "站点主机为空，无法连接".into();
            return;
        }
        // SSH 地址不带用户名（host 无 user@ 且自动登录未填）→ 先弹窗询问凭据
        if site.proto == config::Protocol::Ssh
            && site.autologin.user.trim().is_empty()
            && !site.host.contains('@')
        {
            self.ssh_user = String::new();
            self.ssh_pass = String::new();
            self.ssh_pending = Some(site);
            self.dlg = Dlg::SshAuth;
            return;
        }
        self.connect_site_now(site);
    }

    /// 立即建立连接（凭据已具备）。
    pub fn connect_site_now(&mut self, site: Site) {
        let proxy = site.proxy.and_then(|i| self.cfg.proxies.get(i).cloned());
        let conn = net::spawn(&self.rt, site.clone(), proxy, 2000);
        self.tabs.push(Tab::new(site, Some(conn), self.cfg.font_size, String::new()));
        self.active = self.tabs.len() - 1;
    }

    pub fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if let Some(c) = &self.tabs[i].conn {
            c.send(Cmd::Close);
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new(
                Site {
                    name: "欢迎".into(),
                    ..Default::default()
                },
                None,
                self.cfg.font_size,
                String::new(),
            ));
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }

    pub fn reconnect_active(&mut self) {
        let idx = self.active;
        let Some(site) = self.tabs.get(idx).map(|t| t.site.clone()) else {
            return;
        };
        if site.host.is_empty() {
            return;
        }
        if let Some(t) = self.tabs.get_mut(idx) {
            if let Some(c) = &t.conn {
                c.send(Cmd::Close);
            }
            t.conn = None;
            t.sel_anchor = None;
            t.sel_head = None;
            t.ime_preedit.clear();
        }
        let proxy = site.proxy.and_then(|i| self.cfg.proxies.get(i).cloned());
        let conn = net::spawn(&self.rt, site, proxy, 2000);
        if let Some(t) = self.tabs.get_mut(idx) {
            t.conn = Some(conn);
        }
        self.status_line = "已发起重连".into();
    }

    pub fn save_cfg(&mut self) {
        self.status_line = match config::save(&self.cfg) {
            Ok(()) => format!("配置已保存：{}", self.cfg_path_shown),
            Err(e) => format!("保存失败：{e}"),
        };
    }

    /// 启用的宏：(触发字节, 发送分段)。触发为空的条目忽略。
    /// 仅在终端界面激活（无对话框/属性窗）时才返回宏，设置界面里一律不生效。
    fn active_macros(&self) -> Vec<(Vec<u8>, Vec<config::Segment>)> {
        if !self.macros_global
            || self.dlg != Dlg::None
            || self.props_tab.is_some()
            || self.trigger_armed.is_some()
        {
            return Vec::new();
        }
        self.cfg
            .macros
            .iter()
            .filter(|m| m.enabled && !m.trigger.is_empty() && !m.keys.is_empty())
            .filter_map(|m| {
                let t = config::parse_keyspec(&m.trigger);
                if t.is_empty() {
                    return None;
                }
                Some((t, config::parse_segments(&m.keys)))
            })
            .collect()
    }

    /// 键入/输入法文本 → 发送字节：按站点「输入内码转换」先转内码再按站点编码。
    fn input_bytes(&self, site: &Site, text: &str) -> Vec<u8> {
        match site.input_conv.source() {
            Some(from) => config::convert_input(text, from, site.encoding),
            None => {
                let (b, _, _) = config::codec_for(site.encoding).encode(text);
                b.into_owned()
            }
        }
    }

    /// 把输入字节送入宏匹配器：
    /// - 完整命中触发序列 → 改发宏内容（支持 ~N 延时）
    /// - 只是某触发序列的前缀 → 暂存，等后续字节（超时原样发出）
    /// - 不匹配 → 原样发出
    fn send_with_macros(&mut self, conn: &Conn, bytes: &[u8], enc: CharEncoding, now: f64) {
        let macros = self.active_macros();
        self.macro_buf.extend_from_slice(bytes);
        self.macro_buf_at = now;
        let (out, segs) = macro_match(&mut self.macro_buf, &macros);
        if !out.is_empty() {
            conn.send(Cmd::Bytes(out));
        }
        if !segs.is_empty() {
            self.send_segments(conn, &segs, enc);
        }
    }

    /// 发送宏分段：无延时时直接发字节，有延时时交给网络层按序等待。
    fn send_segments(&self, conn: &Conn, segs: &[config::Segment], enc: CharEncoding) {
        let has_delay = segs.iter().any(|s| matches!(s, config::Segment::Delay(_)));
        let conv: Vec<config::Segment> = segs
            .iter()
            .map(|s| match s {
                config::Segment::Bytes(b) => {
                    let txt = config::bytes_to_keyspec(b);
                    config::Segment::Bytes(config::encode_macro(&txt, enc))
                }
                d => d.clone(),
            })
            .collect();
        if has_delay {
            conn.send(Cmd::Segments(conv));
        } else {
            let mut out = Vec::new();
            for s in conv {
                if let config::Segment::Bytes(b) = s {
                    out.extend_from_slice(&b);
                }
            }
            if !out.is_empty() {
                conn.send(Cmd::Bytes(out));
            }
        }
    }

    /// 宏缓冲超时兜底：挂起的字节超过 250ms 未凑成触发序列就原样发出。
    fn flush_macro_buf(&mut self, now: f64) {
        if self.macro_buf.is_empty() || now - self.macro_buf_at < 0.25 {
            return;
        }
        let out = std::mem::take(&mut self.macro_buf);
        if let Some(c) = self.active_conn() {
            c.send(Cmd::Bytes(out));
        }
    }

    pub fn selection_text(&self, idx: usize) -> Option<String> {
        let tab = self.tabs.get(idx)?;
        let (a, b) = norm_sel(tab.sel_anchor, tab.sel_head)?;
        if a == b {
            return None;
        }
        let conn = tab.conn.as_ref()?;
        let t = conn.term.lock().ok()?;
        let raw = t.doc_text(a.0, b.0 + 1);
        let out = crop_rect(&raw, a, b);
        if out.is_empty() { None } else { Some(out) }
    }

    // ---------------------------------------------------- 输入分发

    fn feed(&mut self, ctx: &egui::Context, events: &[egui::Event], rect: Rect) {
        if rect.is_negative() {
            return;
        }
        let idx = self.active;
        let mut tab = std::mem::replace(&mut self.tabs[idx], Tab::new(Site::default(), None, self.cfg.font_size, String::new()));
        let conn = tab.conn.clone();
        let mut used = false;

        for ev in events {
            match ev {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if self.on_key(ctx, &mut tab, conn.as_ref(), *key, *modifiers) {
                        used = true;
                    }
                }
                egui::Event::Text(s) => {
                    // 普通可打印字符统一由 Text 事件发送（实测一次按键会同帧
                    // 产生 Key+Text 两个事件，Key 路径对可打印字符不再发送，
                    // 否则会双发/宏双触发）。Text 自带正确大小写与键盘布局。
                    // 中文等输入法输入走 Ime Commit 分支，不受影响。
                    if let Some(c) = &conn
                        && !s.is_empty()
                    {
                        let b = self.input_bytes(&tab.site, s);
                        let now = ctx.input(|i| i.time);
                        self.send_with_macros(c, &b, tab.site.encoding, now);
                        used = true;
                    }
                }
                egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                    if conn.is_some() {
                        tab.ime_preedit = text.clone();
                        used = true;
                    }
                }
                egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                    if let Some(c) = &conn {
                        tab.ime_preedit.clear();
                        if !text.is_empty() {
                            let b = self.input_bytes(&tab.site, text);
                            let now = ctx.input(|i| i.time);
                            self.send_with_macros(c, &b, tab.site.encoding, now);
                            used = true;
                        }
                    }
                }
                egui::Event::Paste(text) => {
                    // 仅来自 Shift+Insert / 右键菜单 / 专用粘贴键
                    if let Some(c) = &conn {
                        paste(c, text, &tab.site);
                        used = true;
                    }
                }
                egui::Event::Copy | egui::Event::Cut => {
                    // 仅来自 Ctrl+Insert / 专用复制键
                    if let Some(t) = self.sel_text_of(&tab) {
                        ctx.copy_text(t);
                    }
                    used = true;
                }
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    if !rect.contains(*pos) {
                        continue;
                    }
                    use egui::PointerButton as PB;
                    match (button, pressed) {
                        (PB::Primary, true) if !modifiers.ctrl && !modifiers.alt => {
                            // 点击终端即收回焦点：避免焦点残留按钮等控件后，
                            // 空格被当点击、Ctrl/Shift+Insert 等按键被门控挡掉
                            ctx.memory_mut(|m| m.request_focus(egui::Id::new("term_focus")));
                            // 只记录按下位置，不立即建立选区（避免单击选中一个字符）
                            tab.press_at = Some(self.cell_at(*pos, rect, &tab));
                            tab.sel_anchor = None;
                            tab.sel_head = None;
                            tab.dragging = true;
                            used = true;
                        }
                        (PB::Primary, false) => {
                            if tab.dragging {
                                tab.dragging = false;
                                // 未发生拖动 = 单击：不选中任何字符
                                if tab.sel_anchor.is_none() {
                                    tab.sel_head = None;
                                    // 单击落在 URL 上 → 按协议打开
                                    if let Some(at) = tab.press_at
                                        && let Some(c) = &conn
                                        && let Ok(t) = c.term.lock()
                                    {
                                        let line = t.doc_line(at.0);
                                        // 按网格列对齐重建文本：宽字符占 2 列（字符+空格占位），
                                        // 使字符下标 == 网格列，点击列号才能对上 URL 位置。
                                        // （彩色码是单元格属性、不进文本；此前过滤续格导致列错位。）
                                        let mut text = String::new();
                                        for cell in line.iter() {
                                            if cell.cont {
                                                continue;
                                            }
                                            text.push(cell.ch);
                                            if cell.wide {
                                                text.push(' ');
                                            }
                                        }
                                        if let Some((_tok, action)) = url_at(&text, at.1) {
                                            match action {
                                                UrlAction::Browser(u) => {
                                                    match open_external(&u) {
                                                        Ok(()) => {
                                                            self.status_line =
                                                                format!("已用默认浏览器打开：{u}")
                                                        }
                                                        Err(e) => {
                                                            self.status_line =
                                                                format!("打开失败：{e}")
                                                        }
                                                    }
                                                }
                                                UrlAction::Connect { host, port, ssh } => {
                                                    let mut site = Site::default();
                                                    site.name = host.clone();
                                                    site.host = host;
                                                    site.port = port;
                                                    site.proto = if ssh {
                                                        config::Protocol::Ssh
                                                    } else {
                                                        config::Protocol::Telnet
                                                    };
                                                    site.proxy = c.site.proxy;
                                                    self.connect_site(site);
                                                }
                                            }
                                        }
                                    }
                                } else if self.cfg.copy_on_select
                                    && let Some(t) = self.sel_text_of(&tab)
                                {
                                    ctx.copy_text(t);
                                }
                                tab.press_at = None;
                                used = true;
                            }
                        }
                        (PB::Secondary, true) => {
                            tab.menu_at = Some(*pos);
                            used = true;
                        }
                        _ => {}
                    }
                }
                egui::Event::PointerMoved(pos) => {
                    if tab.dragging && rect.contains(*pos) {
                        let cur = self.cell_at(*pos, rect, &tab);
                        // 只有移动到不同于按下位置的格子，才真正开始划选
                        if tab.sel_anchor.is_none() {
                            if Some(cur) != tab.press_at {
                                tab.sel_anchor = tab.press_at;
                                tab.sel_head = Some(cur);
                                used = true;
                            }
                        } else {
                            tab.sel_head = Some(cur);
                            used = true;
                        }
                    }
                }
                egui::Event::MouseWheel { unit, delta, .. } => {
                    // 滚轮不回溯历史：向上/向下每滑动一格，向远端发一次 PgUp/PgDn
                    let over = ctx.input(|i| i.pointer.interact_pos().unwrap_or(rect.min));
                    if rect.contains(over)
                        && let Some(c) = &conn
                    {
                        let notches = match unit {
                            egui::MouseWheelUnit::Line => delta.y.abs().round().max(1.0),
                            egui::MouseWheelUnit::Point => {
                                (delta.y.abs() / 40.0).round().max(1.0)
                            }
                            egui::MouseWheelUnit::Page => delta.y.abs().round().max(1.0),
                        } as usize;
                        if delta.y.abs() > 1e-3 {
                            let seq: &[u8] = if delta.y > 0.0 {
                                b"\x1b[5~"
                            } else {
                                b"\x1b[6~"
                            };
                            for _ in 0..notches {
                                c.send(Cmd::Bytes(seq.to_vec()));
                            }
                            used = true;
                        }
                    }
                }
                _ => {}
            }
        }
        self.tabs[idx] = tab;
        if used {
            ctx.input_mut(|i| i.events.clear());
        }
    }

    fn sel_text_of(&self, tab: &Tab) -> Option<String> {
        let (a, b) = norm_sel(tab.sel_anchor, tab.sel_head)?;
        if a == b {
            return None;
        }
        let conn = tab.conn.as_ref()?;
        let t = conn.term.lock().ok()?;
        let s = crop_rect(&t.doc_text(a.0, b.0 + 1), a, b);
        if s.is_empty() { None } else { Some(s) }
    }

    fn cell_at(&self, pos: Pos2, rect: Rect, tab: &Tab) -> DocPos {
        let vis_row = ((pos.y - rect.top()) / self.cell_h).floor().max(0.0) as usize;
        let col = ((pos.x - rect.left()) / self.cell_w).floor().max(0.0) as usize;
        let (total, term_rows) = tab
            .conn
            .as_ref()
            .and_then(|c| c.term.lock().ok().map(|t| (t.doc_len(), t.rows as usize)))
            .unwrap_or((0, 24));
        let start = total.saturating_sub(term_rows);
        (start + vis_row, col)
    }

    fn on_key(
        &mut self,
        ctx: &egui::Context,
        tab: &mut Tab,
        conn: Option<&Conn>,
        key: egui::Key,
        m: egui::Modifiers,
    ) -> bool {
        use egui::Key as K;
        // 界面级快捷键
        if !m.ctrl && !m.alt && !m.shift {
            match key {
                K::F2 => {
                    self.dlg = Dlg::AddressBook;
                    return true;
                }
                K::F4 => {
                    self.dlg = Dlg::Settings;
                    return true;
                }
                _ => {}
            }
        }
        // Shift+Insert=粘贴：不依赖 Ctrl（此前误放在 ctrl 分支内，
        // 纯 Shift+Insert 永远进不来，导致发帖界面无法粘贴）
        if key == K::Insert && m.shift && !m.ctrl && !m.alt {
            if let Some(c) = conn {
                if let Ok(mut cb) = arboard::Clipboard::new()
                    && let Ok(txt) = cb.get_text()
                {
                    paste(c, &txt.replace("\r\n", "\n"), &tab.site);
                }
            }
            return true;
        }
        if m.ctrl && !m.alt {
            match key {
                K::Insert => {
                    // Ctrl+Insert=复制（Shift+Insert 粘贴已在前面独立处理）；
                    // 裸 Insert 交给远端
                    if !m.shift && !m.alt {
                        if let Some(t) = self.sel_text_of(tab) {
                            ctx.copy_text(t);
                        }
                        return true;
                    }
                }
                K::R => {
                    // 重连改用 Ctrl+Shift+R，避免占用终端的 ^R
                    if m.shift {
                        self.reconnect_active();
                        return true;
                    }
                }
                K::W => {
                    if m.shift {
                        self.close_tab(self.active);
                        return true;
                    }
                }
                K::M => {
                    if m.shift {
                        // 宏全局开关：只改运行时状态，不写设置文件
                        self.macros_global = !self.macros_global;
                        self.status_line = if self.macros_global {
                            "按键宏：已启用（Ctrl+Shift+M 切换）".into()
                        } else {
                            "按键宏：已停用（Ctrl+Shift+M 切换）".into()
                        };
                        return true;
                    }
                }
                _ => {}
            }
        }

        let Some(conn) = conn else {
            return false;
        };

        let app_key = conn
            .term
            .lock()
            .map(|t| t.app_cursor)
            .unwrap_or(false);
        let ss = if app_key { 'O' } else { '[' };

        let out: Vec<u8> = if m.ctrl && !m.alt {
            if let Some(d) = ctrl_digit(key) {
                // Ctrl+数字：仅作宏触发标识（大键盘 ESC ! 数字 / 小键盘 ESC # 数字），
                // 不作控制字符发往远端。命中宏 → 发宏内容；未绑定/宏停用 → 吞掉。
                let mark = if numpad_digit_down(key) { b'#' } else { b'!' };
                let trig = [0x1b, mark, b'0' + d];
                let hit = self
                    .active_macros()
                    .iter()
                    .find(|(t, _)| t.as_slice() == trig)
                    .map(|(_, s)| s.clone());
                if let Some(segs) = hit {
                    self.macro_buf.clear();
                    self.send_segments(conn, &segs, tab.site.encoding);
                }
                return true;
            }
            match ctrl_byte(key) {
                Some(v) => v,
                None => return false,
            }
        } else if !m.alt && printable_pair(key).is_some() {
            // 可打印字符的裸键/Shift 组合：交给同帧的 Text 事件发送，
            // 这里不发（实测 Key+Text 同帧双事件，避免双发/宏双触发）
            return false;
        } else {
            let base = base_seq(key, m.shift, ss);
            if base.is_empty() {
                return false;
            }
            if m.alt {
                let mut v = vec![0x1b];
                v.extend_from_slice(&base);
                v
            } else {
                base
            }
        };
        let now = ctx.input(|i| i.time);
        self.send_with_macros(conn, &out, tab.site.encoding, now);
        true
    }
}

// ---------------------------------------------------- eframe 0.37 适配

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ensure_fonts();
        self.tick(ui);
    }

    fn auto_save_interval(&self) -> Duration {
        Duration::from_secs(300)
    }
}

// ---------------------------------------------------- 辅助

fn no_wrap() -> TextWrapping {
    TextWrapping {
        max_width: f32::INFINITY,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: None,
    }
}

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// 枚举系统已安装字体，返回 (显示名, 文件绝对路径, ttc 索引)。
fn enum_system_fonts() -> Vec<(String, String, u32)> {
    let mut out: Vec<(String, String, u32)> = Vec::new();
    let script = "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
        Add-Type -AssemblyName System.Drawing; \
        (New-Object System.Drawing.Text.InstalledFontCollection).Families | \
        ForEach-Object { $_.Name }";
    // CREATE_NO_WINDOW：避免 GUI 程序启动时闪出控制台窗口
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(output) = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return out;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let fonts_dir = std::path::Path::new(r"C:\Windows\Fonts");
    // 名称→文件 的常见映射（ttc 多面用索引区分）
    let map: &[(&str, &str, u32)] = &[
        ("新宋体", "simsun.ttc", 1),
        ("NSimSun", "simsun.ttc", 1),
        ("宋体", "simsun.ttc", 0),
        ("SimSun", "simsun.ttc", 0),
        ("微软雅黑", "msyh.ttc", 0),
        ("Microsoft YaHei", "msyh.ttc", 0),
        ("黑体", "simhei.ttf", 0),
        ("SimHei", "simhei.ttf", 0),
        ("楷体", "simkai.ttf", 0),
        ("KaiTi", "simkai.ttf", 0),
        ("仿宋", "simfang.ttf", 0),
        ("FangSong", "simfang.ttf", 0),
        ("等线", "Deng.ttf", 0),
        ("DengXian", "Deng.ttf", 0),
        ("Consolas", "consola.ttf", 0),
        ("Cascadia Mono", "CascadiaMono.ttf", 0),
        ("Cascadia Code", "CascadiaCode.ttf", 0),
        ("Courier New", "cour.ttf", 0),
        ("Lucida Console", "lucon.ttf", 0),
        ("Segoe UI", "segoeui.ttf", 0),
        ("Tahoma", "tahoma.ttf", 0),
        ("Arial", "arial.ttf", 0),
        ("Times New Roman", "times.ttf", 0),
        ("MingLiU", "mingliu.ttc", 0),
        ("細明體", "mingliu.ttc", 0),
        ("PMingLiU", "mingliu.ttc", 1),
        ("新細明體", "mingliu.ttc", 1),
    ];
    for line in text.lines() {
        let name = line.trim().to_string();
        if name.is_empty() { continue; }
        if let Some((_, file, idx)) = map.iter().find(|(n, _, _)| *n == name) {
            let p = fonts_dir.join(file);
            if p.exists() {
                out.push((name, p.display().to_string(), *idx));
                continue;
            }
        }
        // 未知映射：尝试同名 ttf
        let p = fonts_dir.join(format!("{}.ttf", name));
        if p.exists() {
            out.push((name, p.display().to_string(), 0));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

pub fn pack(c: [u8; 3]) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
}

/// 块元素/三角元素满格绘制（字体字形不铺满整格，相邻格会留缝）。
fn draw_block(p: &egui::Painter, ch: char, at: Pos2, w: f32, h: f32, fg: Color32) {
    let x0 = at.x;
    let y0 = at.y;
    let cx = at.x + w * 0.5;
    let cy = at.y + h * 0.5;
    let rect = |x: f32, y: f32, rw: f32, rh: f32| {
        p.rect_filled(Rect::from_min_size(pos2(x, y), vec2(rw, rh)), 0.0, fg);
    };
    let tri = |a: Pos2, b: Pos2, d: Pos2| {
        let mut m = egui::epaint::Mesh::default();
        for v in [a, b, d] {
            m.vertices.push(egui::epaint::Vertex { pos: v, uv: egui::epaint::WHITE_UV, color: fg });
        }
        m.indices.extend_from_slice(&[0, 1, 2]);
        p.add(egui::Shape::mesh(m));
    };
    let tl = pos2(x0, y0);
    let tr = pos2(x0 + w, y0);
    let bl = pos2(x0, y0 + h);
    let br = pos2(x0 + w, y0 + h);
    let ml = pos2(x0, cy);
    let mr = pos2(x0 + w, cy);
    let mt = pos2(cx, y0);
    let mb = pos2(cx, y0 + h);
    match ch as u32 {
        0x2580 => rect(x0, y0, w, h * 0.5),
        0x2581 => rect(x0, y0 + h * 7.0 / 8.0, w, h / 8.0),
        0x2582 => rect(x0, y0 + h * 3.0 / 4.0, w, h / 4.0),
        0x2583 => rect(x0, y0 + h * 5.0 / 8.0, w, h * 3.0 / 8.0),
        0x2584 => rect(x0, cy, w, h * 0.5),
        0x2585 => rect(x0, y0 + h * 3.0 / 8.0, w, h * 5.0 / 8.0),
        0x2586 => rect(x0, y0 + h / 4.0, w, h * 3.0 / 4.0),
        0x2587 => rect(x0, y0 + h / 8.0, w, h * 7.0 / 8.0),
        0x2588 => rect(x0, y0, w, h),
        0x2589 => rect(x0, y0, w * 7.0 / 8.0, h),
        0x258A => rect(x0, y0, w * 3.0 / 4.0, h),
        0x258B => rect(x0, y0, w * 5.0 / 8.0, h),
        0x258C => rect(x0, y0, w * 0.5, h),
        0x258D => rect(x0, y0, w * 3.0 / 8.0, h),
        0x258E => rect(x0, y0, w / 4.0, h),
        0x258F => rect(x0, y0, w / 8.0, h),
        0x2590 => rect(cx, y0, w * 0.5, h),
        0x2591 | 0x2592 | 0x2593 => {
            let density = match ch as u32 { 0x2591 => 1, 0x2592 => 2, _ => 3 };
            for i in 0..4u32 {
                for j in 0..4u32 {
                    let on = match density {
                        1 => (i + j) % 4 == 0,
                        2 => (i + j) % 2 == 0,
                        _ => !((i + j) % 4 == 0),
                    };
                    if on {
                        rect(x0 + i as f32 * w / 4.0, y0 + j as f32 * h / 4.0, w / 4.0, h / 4.0);
                    }
                }
            }
        }
        0x2594 => rect(x0, y0, w, h / 8.0),
        0x2595 => rect(x0 + w * 7.0 / 8.0, y0, w / 8.0, h),
        0x2596 => rect(x0, cy, w * 0.5, h * 0.5),          // ▖
        0x2597 => rect(cx, cy, w * 0.5, h * 0.5),          // ▗
        0x2598 => rect(x0, y0, w * 0.5, h * 0.5),          // ▘
        0x2599 => { tri(tl, bl, br); rect(x0, y0, w * 0.5, h * 0.5); } // ▙ 近似
        0x259A => { rect(x0, y0, w * 0.5, h * 0.5); rect(cx, cy, w * 0.5, h * 0.5); } // ▚
        0x259B => { rect(x0, y0, w, h * 0.5); rect(x0, cy, w * 0.5, h * 0.5); } // ▛
        0x259C => { rect(x0, y0, w, h * 0.5); rect(cx, cy, w * 0.5, h * 0.5); } // ▜
        0x259D => rect(cx, y0, w * 0.5, h * 0.5),          // ▝
        0x259E => { rect(cx, y0, w * 0.5, h * 0.5); rect(x0, cy, w * 0.5, h * 0.5); } // ▞
        0x259F => { rect(cx, y0, w * 0.5, h * 0.5); rect(x0, cy, w, h * 0.5); } // ▟
        // 几何三角（BBS 艺术常用）
        0x25E2 => tri(bl, br, tr), // ◢
        0x25E3 => tri(bl, br, tl), // ◣
        0x25E4 => tri(tl, tr, bl), // ◤
        0x25E5 => tri(tl, tr, br), // ◥
        0x25B2 => tri(mt, bl, br), // ▲
        0x25BC => tri(mb, tl, tr), // ▼
        0x25C0 => tri(ml, tr, br), // ◀
        0x25B6 => tri(mr, tl, bl), // ▶
        _ => {
            // 未知块字符退回满格，避免留缝
            rect(x0, y0, w, h);
        }
    }
    let _ = (mt, mb);
}

pub fn resolve(c: Color, s: &ColorScheme) -> [u8; 3] {
    match c {
        Color::Pallet(i) => s.pallet(i),
        Color::X256(i) => xterm256(i),
        Color::Rgb(r, g, b) => [r, g, b],
    }
}

pub fn single_job(text: &str, fid: FontId, color: [u8; 3]) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: fid,
            color: rgb(color),
            ..Default::default()
        },
    );
    job.wrap = no_wrap();
    job.break_on_newline = false;
    job
}

pub fn norm_sel(a: Option<DocPos>, b: Option<DocPos>) -> Option<(DocPos, DocPos)> {
    let (x, y) = (a?, b?);
    Some(if x <= y { (x, y) } else { (y, x) })
}

/// 按起止文档坐标裁剪 doc_text 结果。
/// 注意：列坐标是**网格列**（中文/全角占 2 列），需按显示宽度累加换算，
/// 不能当作字符下标，否则选中 N 个汉字会复制出 2N 个字符。
pub fn crop_rect(txt: &str, a: DocPos, b: DocPos) -> String {
    let rows = b.0 - a.0 + 1;
    let mut out = String::new();
    for (i, line) in txt.split('\n').take(rows).enumerate() {
        let row = a.0 + i;
        let from_col = if row == a.0 { a.1 } else { 0 };
        // 末行取到 b.1 所在字符（含该字符），其余行取整行
        let end_col = if row == b.0 { b.1 } else { usize::MAX };
        let mut col = 0usize;
        let mut seg = String::new();
        for ch in line.chars() {
            let w = unicode_width::UnicodeWidthChar::width_cjk(ch).unwrap_or(1).max(1);
            // 该字符占据 [col, col+w)；与目标区间 [from_col, end_col] 有交集则保留
            if col + w > from_col && col <= end_col {
                seg.push(ch);
            }
            col += w;
            if col > end_col { break; }
        }
        let seg = seg.trim_end_matches(' ');
        out.push_str(seg);
        if i + 1 < rows {
            out.push('\n');
        }
    }
    out
}

/// 宏匹配纯逻辑：给定当前缓冲与宏表，返回「应立即原样发送的字节」和
/// 「命中后要发送的宏分段」，并把仍需等待的前缀留在 buf 中。
///
/// 规则：完整命中（优先最长触发）→ 替换；仅是某触发的前缀 → 暂存；否则原样发出。
pub fn macro_match(
    buf: &mut Vec<u8>,
    macros: &[(Vec<u8>, Vec<config::Segment>)],
) -> (Vec<u8>, Vec<config::Segment>) {
    if buf.is_empty() {
        return (Vec::new(), Vec::new());
    }
    if macros.is_empty() {
        return (std::mem::take(buf), Vec::new());
    }
    // 完整命中：取最长触发序列
    let mut hit: Option<(usize, Vec<config::Segment>)> = None;
    for (t, segs) in macros {
        if buf.ends_with(t.as_slice()) && hit.as_ref().map(|(n, _)| t.len() > *n).unwrap_or(true) {
            hit = Some((t.len(), segs.clone()));
        }
    }
    if let Some((_n, segs)) = hit {
        // 触发键本身被拦截、不再发往远端：只发宏内容（第二栏）。
        // 缓冲里命中前的字节（多键触发的前导部分）也一并丢弃。
        buf.clear();
        return (Vec::new(), segs);
    }
    // 未命中：找出最长的「仍可能构成触发序列」的尾部前缀，保留它
    let mut keep_from = buf.len();
    for (t, _) in macros {
        let max_k = t.len().saturating_sub(1).min(buf.len());
        for k in (1..=max_k).rev() {
            if buf[buf.len() - k..] == t[..k] {
                keep_from = keep_from.min(buf.len() - k);
                break;
            }
        }
    }
    let out: Vec<u8> = buf[..keep_from].to_vec();
    buf.drain(..keep_from);
    (out, Vec::new())
}

pub fn ctrl_byte(key: egui::Key) -> Option<Vec<u8>> {
    use egui::Key as K;
    let c: u8 = match key {
        K::Space => 0x00,
        K::A => 0x01,
        K::B => 0x02,
        K::C => 0x03,
        K::D => 0x04,
        K::E => 0x05,
        K::F => 0x06,
        K::G => 0x07,
        K::H => 0x08,
        K::I => 0x09,
        K::J => 0x0A,
        K::K => 0x0B,
        K::L => 0x0C,
        K::M => 0x0D,
        K::N => 0x0E,
        K::O => 0x0F,
        K::P => 0x10,
        K::Q => 0x11,
        K::R => 0x12,
        K::S => 0x13,
        K::T => 0x14,
        K::U => 0x15,
        K::V => 0x16,
        K::W => 0x17,
        K::X => 0x18,
        K::Y => 0x19,
        K::Z => 0x1A,
        K::OpenBracket => 0x1B,
        K::Backslash => 0x1C,
        K::CloseBracket => 0x1D,
        K::Minus => 0x1F,
        K::Backspace => 0x08,
        _ => return None,
    };
    Some(vec![c])
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(vkey: i32) -> i16;
}

/// 该数字键事件是否来自小键盘：egui 把大/小键盘数字合并成同一 Key，
/// 用系统物理键状态区分（按下瞬间查询，小键盘 VK_NUMPAD* 处于按下态）。
fn numpad_digit_down(key: egui::Key) -> bool {
    let Some(d) = ctrl_digit(key) else {
        return false;
    };
    unsafe { GetAsyncKeyState(0x60 + d as i32) < 0 }
}

/// 数字键 → 数字值（Ctrl+数字宏触发键用）。
fn ctrl_digit(key: egui::Key) -> Option<u8> {
    use egui::Key as K;
    Some(match key {
        K::Num0 => 0,
        K::Num1 => 1,
        K::Num2 => 2,
        K::Num3 => 3,
        K::Num4 => 4,
        K::Num5 => 5,
        K::Num6 => 6,
        K::Num7 => 7,
        K::Num8 => 8,
        K::Num9 => 9,
        _ => return None,
    })
}

/// 可打印字符键 → (未按 Shift 的字符, 按住 Shift 的字符)（US 布局）。
/// egui 的 Key 不区分大小写，终端字符统一由此表生成，不再依赖 Text 事件。
fn printable_pair(key: egui::Key) -> Option<(char, char)> {
    use egui::Key as K;
    let p = match key {
        K::A => ('a', 'A'),
        K::B => ('b', 'B'),
        K::C => ('c', 'C'),
        K::D => ('d', 'D'),
        K::E => ('e', 'E'),
        K::F => ('f', 'F'),
        K::G => ('g', 'G'),
        K::H => ('h', 'H'),
        K::I => ('i', 'I'),
        K::J => ('j', 'J'),
        K::K => ('k', 'K'),
        K::L => ('l', 'L'),
        K::M => ('m', 'M'),
        K::N => ('n', 'N'),
        K::O => ('o', 'O'),
        K::P => ('p', 'P'),
        K::Q => ('q', 'Q'),
        K::R => ('r', 'R'),
        K::S => ('s', 'S'),
        K::T => ('t', 'T'),
        K::U => ('u', 'U'),
        K::V => ('v', 'V'),
        K::W => ('w', 'W'),
        K::X => ('x', 'X'),
        K::Y => ('y', 'Y'),
        K::Z => ('z', 'Z'),
        K::Num0 => ('0', ')'),
        K::Num1 => ('1', '!'),
        K::Num2 => ('2', '@'),
        K::Num3 => ('3', '#'),
        K::Num4 => ('4', '$'),
        K::Num5 => ('5', '%'),
        K::Num6 => ('6', '^'),
        K::Num7 => ('7', '&'),
        K::Num8 => ('8', '*'),
        K::Num9 => ('9', '('),
        K::Space => (' ', ' '),
        K::Minus => ('-', '_'),
        K::Equals => ('=', '+'),
        K::OpenBracket => ('[', '{'),
        K::CloseBracket => (']', '}'),
        K::Backslash => ('\\', '|'),
        K::Semicolon => (';', ':'),
        K::Quote => ('\'', '"'),
        K::Backtick => ('`', '~'),
        K::Comma => (',', '<'),
        K::Period => ('.', '>'),
        K::Slash => ('/', '?'),
        // 这些变体本身已是移位后的字符
        K::Colon => (':', ':'),
        K::Pipe => ('|', '|'),
        K::Questionmark => ('?', '?'),
        K::Exclamationmark => ('!', '!'),
        K::Plus => ('+', '+'),
        K::OpenCurlyBracket => ('{', '{'),
        K::CloseCurlyBracket => ('}', '}'),
        _ => return None,
    };
    Some(p)
}

pub fn base_seq(key: egui::Key, shift: bool, ss: char) -> Vec<u8> {
    use egui::Key as K;
    // 普通可打印字符（字母/数字/符号）：按字符本身发送（含大小写）
    if let Some((lo, up)) = printable_pair(key) {
        let c = if shift { up } else { lo };
        let mut b = [0u8; 4];
        return c.encode_utf8(&mut b).as_bytes().to_vec();
    }
    let direct: &[u8] = match (key, shift) {
        (K::Enter, _) => b"\r",
        (K::Tab, false) => b"\t",
        (K::Tab, true) => b"\x1b[Z",
        (K::Backspace, _) => b"\x7f",
        (K::Escape, _) => b"\x1b",
        (K::Space, _) => b" ",
        (K::PageUp, false) => b"\x1b[5~",
        (K::PageDown, false) => b"\x1b[6~",
        (K::Insert, false) => b"\x1b[2~",
        (K::Delete, _) => b"\x1b[3~",
        // Home/End 用旧式 PC 终端 ~ 序列（Firebird 键表同族）；
        // ESC[H/ESC[F 不被识别，尾字符 H/F 会漏成普通按键（十大/转寄）
        (K::Home, false) => b"\x1b[1~",
        (K::End, false) => b"\x1b[4~",
        _ => &[],
    };
    if !direct.is_empty() {
        return direct.to_vec();
    }
    let letter = match key {
        K::ArrowUp => b'A',
        K::ArrowDown => b'B',
        K::ArrowRight => b'C',
        K::ArrowLeft => b'D',
        K::Home => b'H',
        K::End => b'F',
        _ => return Vec::new(),
    };
    if shift {
        return format!("\x1b[1;2{}", letter as char).into_bytes();
    }
    vec![0x1b, ss as u8, letter]
}

pub fn send_str(conn: &Conn, s: &str, enc: CharEncoding) {
    let (bytes, _, _) = config::codec_for(enc).encode(s);
    conn.send(Cmd::Bytes(bytes.into_owned()));
}

// ---------------------------------------------------- URL 点击

/// 点击 URL 的处理动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlAction {
    /// 用默认浏览器打开
    Browser(String),
    /// 用 ATerm 新建连接
    Connect { host: String, port: u16, ssh: bool },
}

/// 纯 IPv4 地址（无协议/路径/查询）。
pub fn is_bare_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().map(|n| n <= 255).unwrap_or(false)
        })
}

/// 带掩码的 IPv4：四段，每段为 0-255 数字或单个 `*`，至少含一个 `*`。
/// 归一化为相应段补 0 的 IP（202.120.*.* → 202.120.0.0，10.*.*.* → 10.0.0.0）。
pub fn normalize_masked_ip(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 || !parts.contains(&"*") {
        return None;
    }
    let mut out: Vec<String> = Vec::with_capacity(4);
    for p in parts {
        if p == "*" {
            out.push("0".to_string());
        } else if !p.is_empty()
            && p.len() <= 3
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u16>().map(|n| n <= 255).unwrap_or(false)
        {
            out.push(p.to_string());
        } else {
            return None;
        }
    }
    Some(out.join("."))
}

fn host_ok(h: &str) -> bool {
    // 带掩码的 IP（202.120.*.* 等）直接放行，后续归一化为补 0 的 IP
    if normalize_masked_ip(h).is_some() {
        return true;
    }
    // 纯数字点分（形如版本号 1.2.3）必须是合法 IPv4 才算主机，避免误判
    if h.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return is_bare_ipv4(h);
    }
    if h.is_empty()
        || (!h.contains('.') && !h.eq_ignore_ascii_case("localhost"))
        || h.starts_with('.')
        || h.ends_with('.')
        || h.contains("..")
        || !h
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
    {
        return false;
    }
    // 每个点分标签须以字母/数字开头和结尾（bbs.fudan.edu.cn 这类
    // 多级域名放行；-foo / foo- 等非法形态拒绝）
    h.split('.').all(|label| {
        !label.is_empty()
            && label.as_bytes()[0].is_ascii_alphanumeric()
            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
    })
}

/// 不带协议的 URL 候选：host[:port][/path][?query]，host 须含点（或 localhost/纯IP）。
fn bare_url_candidate(tok: &str) -> Option<String> {
    let end = tok.find(['/', '?', '#']).unwrap_or(tok.len());
    let authority = &tok[..end];
    let (h, port_ok) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p.len() <= 5,
        ),
        None => (authority, true),
    };
    if port_ok && host_ok(h) {
        Some(tok.to_string())
    } else {
        None
    }
}

/// 在行文本中找覆盖列 col 的 URL token，返回 (token, 动作)。
/// 规则：
/// - 带协议 http/https → 浏览器；telnet/ssh → ATerm 连接（默认端口 23/22）
/// - 不带协议：纯 IP（无路径/查询）→ 浏览器打开 360 搜索；其余 → 浏览器直接打开
pub fn url_at(line: &str, col: usize) -> Option<(String, UrlAction)> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() {
        return None;
    }
    let is_tok = |c: char| c.is_ascii() && !c.is_ascii_whitespace() && !c.is_ascii_control();
    if !is_tok(chars[col]) {
        return None;
    }
    let mut start = col;
    while start > 0 && is_tok(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col + 1;
    while end < chars.len() && is_tok(chars[end]) {
        end += 1;
    }
    let mut token: String = chars[start..end].iter().collect();
    // 去掉首尾包裹的括号/引号/句读（保留内部合法字符）
    token = token
        .trim_start_matches(['(', '[', '{', '<', '"', '\''])
        .trim_end_matches([')', ']', '}', '>', '"', '\'', '.', ',', ';', '!', '?'])
        .to_string();
    // 尾随冒号：后面是数字时视为端口分隔符保留（bbs.fudan.edu.cn:2323），
    // 否则按句读剥离（"地址是 example.com:"）
    if token.ends_with(':')
        && !token
            .rsplit_once(':')
            .map(|(_, p)| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .unwrap_or(false)
    {
        token.pop();
    }
    if token.is_empty() {
        return None;
    }
    let lower = token.to_ascii_lowercase();
    for (scheme, ssh) in [("telnet://", false), ("ssh://", true)] {
        if let Some(rest) = lower.strip_prefix(scheme) {
            let rest = rest.trim_end_matches('/');
            let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
            let authority = authority.rsplit('@').next().unwrap_or("");
            let (h, port) = match authority.rsplit_once(':') {
                Some((h, p)) => (h, p.parse::<u16>().unwrap_or(if ssh { 22 } else { 23 })),
                None => (authority, if ssh { 22 } else { 23 }),
            };
            if host_ok(h) {
                return Some((
                    token.clone(),
                    UrlAction::Connect { host: h.to_string(), port, ssh },
                ));
            }
            return None;
        }
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some((token.clone(), UrlAction::Browser(token)));
    }
    if lower.contains("://") {
        return None; // 其它协议不处理
    }
    if let Some(bare) = bare_url_candidate(&token) {
        if is_bare_ipv4(&bare) {
            return Some((
                token.clone(),
                UrlAction::Browser(format!("https://www.so.com/s?q={bare}")),
            ));
        }
        // 带掩码的 IP（202.120.*.* 等）：* 段补 0 后按 IP 搜索
        if let Some(ip) = normalize_masked_ip(&bare) {
            return Some((
                token.clone(),
                UrlAction::Browser(format!("https://www.so.com/s?q={ip}")),
            ));
        }
        return Some((token.clone(), UrlAction::Browser(bare)));
    }
    None
}

/// 用系统默认浏览器打开 URL。
/// 不带协议（如 bbs.fudan.edu.cn）时统一补 http:// 再打开，
/// 一律走系统 URL 关联交给默认浏览器，不查询浏览器启动命令。
pub fn open_external(target: &str) -> std::io::Result<()> {
    let url = if target.contains("://") {
        target.to_string()
    } else {
        format!("http://{target}")
    };
    open_via_association(&url)
}

/// 走系统 URL 关联打开（不经过 shell 解析，避免注入）。
fn open_via_association(target: &str) -> std::io::Result<()> {
    std::process::Command::new("rundll32.exe")
        .arg("url.dll,FileProtocolHandler")
        .arg(target)
        .spawn()?;
    Ok(())
}

pub fn paste(conn: &Conn, text: &str, site: &Site) {
    let bytes: Vec<u8> = match site.input_conv.source() {
        Some(from) => config::convert_input(text, from, site.encoding),
        None => {
            let (b, _, _) = config::codec_for(site.encoding).encode(text);
            b.into_owned()
        }
    };
    let bracketed = conn
        .term
        .lock()
        .map(|t| t.bracket_paste)
        .unwrap_or(false);
    let mut out = Vec::with_capacity(bytes.len() + 12);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
    }
    out.extend_from_slice(&bytes);
    if bracketed {
        out.extend_from_slice(b"\x1b[201~");
    }
    conn.send(Cmd::Bytes(out));
}

pub fn xterm256(i: u8) -> [u8; 3] {
    const BASE: [[u8; 3]; 8] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
    ];
    if i < 8 {
        return BASE[i as usize];
    }
    if i < 16 {
        let bright = [
            [127, 127, 127],
            [255, 0, 0],
            [0, 255, 0],
            [255, 255, 0],
            [92, 92, 255],
            [255, 0, 255],
            [0, 255, 255],
            [255, 255, 255],
        ];
        return bright[i as usize - 8];
    }
    if i < 232 {
        let v = i - 16;
        let conv = |x: u8| if x == 0 { 0u8 } else { 55 + x * 40 };
        return [conv(v / 36), conv((v % 36) / 6), conv(v % 6)];
    }
    let g = 8 + (i - 232) * 10;
    [g, g, g]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_single_line() {
        let txt = "hello world\nsecond line\n";
        let s = crop_rect(txt, (0, 6), (0, 10));
        assert_eq!(s, "world");
    }

    #[test]
    fn crop_multi_line() {
        let txt = "abcdef\nghijkl\nmnopqr\n";
        let s = crop_rect(txt, (0, 2), (2, 3));
        assert_eq!(s, "cdef\nghijkl\nmnop");
    }

    #[test]
    fn crop_clamps_end() {
        let txt = "short\n";
        let s = crop_rect(txt, (0, 0), (0, 99));
        assert_eq!(s, "short");
    }

    #[test]
    fn crop_cjk_by_display_width() {
        // 中文占 2 列：列坐标必须按显示宽度换算，不能当字符下标
        let txt = "你们医学院那么多女学生呢\n";
        assert_eq!(crop_rect(txt, (0, 0), (0, 3)), "你们");
        assert_eq!(crop_rect(txt, (0, 0), (0, 5)), "你们医");
        assert_eq!(crop_rect(txt, (0, 4), (0, 9)), "医学院");
        assert_eq!(crop_rect(txt, (0, 4), (0, 5)), "医");
    }

    #[test]
    fn crop_cjk_leading_spaces() {
        // 行首空格也占列宽
        let txt = "  你们医学院那么多女学生呢\n";
        assert_eq!(crop_rect(txt, (0, 2), (0, 5)), "你们");
        assert_eq!(crop_rect(txt, (0, 6), (0, 11)), "医学院");
    }

    #[test]
    fn crop_mixed_ascii_cjk() {
        let txt = "ab中文cd\n";
        // 列: a0 b1 中2-3 文4-5 c6 d7
        assert_eq!(crop_rect(txt, (0, 2), (0, 5)), "中文");
        assert_eq!(crop_rect(txt, (0, 0), (0, 3)), "ab中");
        assert_eq!(crop_rect(txt, (0, 6), (0, 7)), "cd");
    }

    #[test]
    fn ctrl_bytes() {
        assert_eq!(ctrl_byte(egui::Key::C), Some(vec![0x03]));
        assert_eq!(ctrl_byte(egui::Key::OpenBracket), Some(vec![0x1B]));
        assert_eq!(ctrl_byte(egui::Key::Enter), None);
        // 这些键不再被界面复制/粘贴/重连占用，必须能发往远端
        assert_eq!(ctrl_byte(egui::Key::X), Some(vec![0x18]));
        assert_eq!(ctrl_byte(egui::Key::V), Some(vec![0x16]));
        assert_eq!(ctrl_byte(egui::Key::R), Some(vec![0x12]));
        assert_eq!(ctrl_byte(egui::Key::W), Some(vec![0x17]));
    }

    #[test]
    fn ctrl_c_macro_trigger() {
        // ^C / ^X / ^V 可作为宏触发键
        for (spec, byte) in [("^C", 0x03u8), ("^X", 0x18), ("^V", 0x16)] {
            let ms = vec![mac(spec, "ok")];
            let mut buf = vec![byte];
            let (out, segs) = macro_match(&mut buf, &ms);
            assert!(out.is_empty(), "spec={spec}");
            assert_eq!(segs, vec![config::Segment::Bytes(b"ok".to_vec())]);
        }
    }

    fn mac(trigger: &str, keys: &str) -> (Vec<u8>, Vec<config::Segment>) {
        (
            config::parse_keyspec(trigger),
            config::parse_segments(keys),
        )
    }

    #[test]
    fn ctrl_digit_macro_trigger() {
        // Ctrl+1 绑定宏：命中只发第二栏，触发标识被拦截
        let ms = vec![mac("^!1", "u^M")];
        let mut buf = vec![0x1b, b'!', b'1'];
        let (out, segs) = macro_match(&mut buf, &ms);
        assert!(out.is_empty());
        assert_eq!(segs, vec![config::Segment::Bytes(vec![b'u', 0x0D])]);
        assert!(buf.is_empty());
        // 未绑定的 Ctrl+2：匹配器按普通字节原样输出；
        // 按键路径（on_key）在绑定缺失时直接吞掉，不会走到匹配器，标识不泄漏
        let mut buf2 = vec![0x1b, b'!', b'2'];
        let (out2, segs2) = macro_match(&mut buf2, &ms);
        assert_eq!(out2, vec![0x1b, b'!', b'2']);
        assert!(segs2.is_empty());
    }

    #[test]
    fn ctrl_digit_keys() {
        assert_eq!(ctrl_digit(egui::Key::Num0), Some(0));
        assert_eq!(ctrl_digit(egui::Key::Num9), Some(9));
        assert_eq!(ctrl_digit(egui::Key::A), None);
        assert_eq!(ctrl_digit(egui::Key::F1), None);
    }

    #[test]
    fn url_http_https_browser() {
        let line = "访问 https://www.mysmth.net/index 看看";
        // 点击 token 内任意列都应命中
        let col = line.find("https").unwrap() + 3;
        let (tok, act) = url_at(line, col).unwrap();
        assert_eq!(tok, "https://www.mysmth.net/index");
        assert_eq!(act, UrlAction::Browser("https://www.mysmth.net/index".into()));
        let line2 = "http://example.com";
        let (_, act2) = url_at(line2, 2).unwrap();
        assert_eq!(act2, UrlAction::Browser("http://example.com".into()));
    }

    #[test]
    fn url_telnet_ssh_connect() {
        let (_, act) = url_at("telnet://bbs.fudan.edu.cn", 10).unwrap();
        assert_eq!(
            act,
            UrlAction::Connect { host: "bbs.fudan.edu.cn".into(), port: 23, ssh: false }
        );
        let (_, act2) = url_at("ssh://mysmth.net:2222/", 8).unwrap();
        assert_eq!(
            act2,
            UrlAction::Connect { host: "mysmth.net".into(), port: 2222, ssh: true }
        );
        let (_, act3) = url_at("ssh://user@1.2.3.4", 12).unwrap();
        assert_eq!(
            act3,
            UrlAction::Connect { host: "1.2.3.4".into(), port: 22, ssh: true }
        );
    }

    #[test]
    fn url_bare_domain_browser() {
        let (_, act) = url_at("www.mysmth.net", 4).unwrap();
        assert_eq!(act, UrlAction::Browser("www.mysmth.net".into()));
        let (_, act2) = url_at("bbs.fenglin.info:2323", 5).unwrap();
        assert_eq!(act2, UrlAction::Browser("bbs.fenglin.info:2323".into()));
        let (_, act3) = url_at("example.com/path?q=1", 3).unwrap();
        assert_eq!(act3, UrlAction::Browser("example.com/path?q=1".into()));
        // bbs.mysmth.net 这类三级域名应识别（打开时统一补 http://）
        let (_, act4) = url_at("bbs.mysmth.net", 5).unwrap();
        assert_eq!(act4, UrlAction::Browser("bbs.mysmth.net".into()));
        let line = "请访问 bbs.mysmth.net 查看";
        let col = line.find("bbs").unwrap();
        let (_, act5) = url_at(line, col).unwrap();
        assert_eq!(act5, UrlAction::Browser("bbs.mysmth.net".into()));
    }

    #[test]
    fn url_bare_domain_fudan_and_port() {
        // bbs.fudan.edu.cn 这类四级域名应识别（打开时统一补 http://）
        let (_, act) = url_at("bbs.fudan.edu.cn", 5).unwrap();
        assert_eq!(act, UrlAction::Browser("bbs.fudan.edu.cn".into()));
        // 带端口：冒号后是数字，须保留端口，不能当句读剥掉
        let (_, act2) = url_at("bbs.fudan.edu.cn:2323", 5).unwrap();
        assert_eq!(act2, UrlAction::Browser("bbs.fudan.edu.cn:2323".into()));
        // 中文语境里点击域名中部
        let line = "详见 bbs.fudan.edu.cn 进站";
        let col = line.find("fudan").unwrap();
        let (_, act3) = url_at(line, col).unwrap();
        assert_eq!(act3, UrlAction::Browser("bbs.fudan.edu.cn".into()));
        // 尾随冒号但后面不是端口数字 → 按句读剥离
        let (_, act4) = url_at("地址是 example.com:", 9).unwrap();
        assert_eq!(act4, UrlAction::Browser("example.com".into()));
    }

    #[test]
    fn url_bare_ip_search() {
        // 纯 IP（无协议/路径/查询）→ 360 搜索
        let (_, act) = url_at("120.92.211.176", 5).unwrap();
        assert_eq!(
            act,
            UrlAction::Browser("https://www.so.com/s?q=120.92.211.176".into())
        );
        // 带路径/查询的 IP 不算纯 IP → 直接打开
        let (_, act2) = url_at("1.2.3.4/x", 3).unwrap();
        assert_eq!(act2, UrlAction::Browser("1.2.3.4/x".into()));
        let (_, act3) = url_at("1.2.3.4?q=1", 3).unwrap();
        assert_eq!(act3, UrlAction::Browser("1.2.3.4?q=1".into()));
    }

    #[test]
    fn url_non_url_ignored() {
        assert!(url_at("hello world", 2).is_none());
        assert!(url_at("请输入代号:", 3).is_none());
        assert!(url_at("version 1.2.3", 9).is_none()); // 无主机形态
        assert!(url_at("ftp://example.com", 5).is_none()); // 其它协议不处理
        assert!(url_at("   ", 1).is_none());
        assert!(url_at("abc", 99).is_none());
    }

    #[test]
    fn url_wrapped_and_punct() {
        // 括号包裹、句尾标点应剥离
        let line = "(https://example.com).";
        let (_, act) = url_at(line, 5).unwrap();
        assert_eq!(act, UrlAction::Browser("https://example.com".into()));
        let line2 = "详见 www.example.com，谢谢";
        let (_, act2) = url_at(line2, 6).unwrap();
        assert_eq!(act2, UrlAction::Browser("www.example.com".into()));
    }

    #[test]
    fn ipv4_validation() {
        assert!(is_bare_ipv4("1.2.3.4"));
        assert!(is_bare_ipv4("255.255.255.255"));
        assert!(!is_bare_ipv4("256.1.1.1"));
        assert!(!is_bare_ipv4("1.2.3"));
        assert!(!is_bare_ipv4("1.2.3.4.5"));
        assert!(!is_bare_ipv4("a.b.c.d"));
        assert!(!is_bare_ipv4("1.2.3.4:80"));
    }

    #[test]
    fn masked_ip_normalize() {
        assert_eq!(normalize_masked_ip("202.120.*.*").as_deref(), Some("202.120.0.0"));
        assert_eq!(normalize_masked_ip("10.*.*.*").as_deref(), Some("10.0.0.0"));
        assert_eq!(normalize_masked_ip("192.168.1.*").as_deref(), Some("192.168.1.0"));
        assert_eq!(normalize_masked_ip("*.*.*.*").as_deref(), Some("0.0.0.0"));
        // 无掩码 / 非法段 → 不处理
        assert_eq!(normalize_masked_ip("1.2.3.4"), None);
        assert_eq!(normalize_masked_ip("256.1.*.*"), None);
        assert_eq!(normalize_masked_ip("1.2.*"), None);
        assert_eq!(normalize_masked_ip("a.b.*.*"), None);
    }

    #[test]
    fn url_masked_ip_search() {
        // 按网格列对齐：来0 自2 空格4 IP从5开始（中文占2列=字符+占位空格）
        let (_, act) = url_at("来自  202.120.*.*  的朋友", 8).unwrap();
        assert_eq!(
            act,
            UrlAction::Browser("https://www.so.com/s?q=202.120.0.0".into())
        );
        let (_, act2) = url_at("10.*.*.*", 2).unwrap();
        assert_eq!(
            act2,
            UrlAction::Browser("https://www.so.com/s?q=10.0.0.0".into())
        );
        let (_, act3) = url_at("192.168.1.*", 4).unwrap();
        assert_eq!(
            act3,
            UrlAction::Browser("https://www.so.com/s?q=192.168.1.0".into())
        );
    }

    #[test]
    fn url_col_align_with_wide_chars() {
        // 模拟终端按网格列重建的行：中文占 2 列（字符+空格占位），
        // 字符下标 == 网格列。点击 URL 中部应正确命中（此前过滤续格会错位）。
        let line = "水木社区 http://www.mysmth.net 欢迎";
        // 网格列：水0 木2 社4 区6 空格8 h9...
        let col = line.find("mysmth").unwrap();
        let (tok, act) = url_at(line, col).unwrap();
        assert_eq!(tok, "http://www.mysmth.net");
        assert_eq!(act, UrlAction::Browser("http://www.mysmth.net".into()));
        // 纯 IP 前带中文同样对齐
        let line2 = "来源 120.92.211.176 在线";
        let col2 = line2.find("120").unwrap();
        let (_, act2) = url_at(line2, col2).unwrap();
        assert_eq!(
            act2,
            UrlAction::Browser("https://www.so.com/s?q=120.92.211.176".into())
        );
    }

    #[test]
    fn macro_match_enter_double() {
        // 用户示例：触发 ^M → 发送 ^M^M
        let ms = vec![mac("^M", "^M^M")];
        let mut buf = b"\r".to_vec();
        let (out, segs) = macro_match(&mut buf, &ms);
        assert!(out.is_empty());
        assert_eq!(segs, vec![config::Segment::Bytes(b"\r\r".to_vec())]);
        assert!(buf.is_empty());
    }

    #[test]
    fn macro_match_passthrough() {
        // 未命中的普通按键必须原样立即发出，不能被吞
        let ms = vec![mac("^M", "^M^M")];
        let mut buf = b"abc".to_vec();
        let (out, segs) = macro_match(&mut buf, &ms);
        assert_eq!(out, b"abc".to_vec());
        assert!(segs.is_empty());
        assert!(buf.is_empty());
    }

    #[test]
    fn macro_match_prefix_holds() {
        // 多字节触发：ESC 只是前缀，应暂存等待后续字节
        let ms = vec![mac("^[[A", "x")];
        let mut buf = vec![0x1B];
        let (out, segs) = macro_match(&mut buf, &ms);
        assert!(out.is_empty());
        assert!(segs.is_empty());
        assert_eq!(buf, vec![0x1B]);
        // 收到 [ 仍是前缀
        buf.push(b'[');
        let (out, _) = macro_match(&mut buf, &ms);
        assert!(out.is_empty());
        assert_eq!(buf, vec![0x1B, b'[']);
        // 收到 A → 命中
        buf.push(b'A');
        let (out, segs) = macro_match(&mut buf, &ms);
        assert!(out.is_empty());
        assert_eq!(segs, vec![config::Segment::Bytes(b"x".to_vec())]);
        assert!(buf.is_empty());
    }

    #[test]
    fn macro_match_prefix_breaks() {
        // 前缀被打破（ESC [ B 不是 ^[[A）→ 原样发出，不丢字节
        let ms = vec![mac("^[[A", "x")];
        let mut buf = vec![0x1B, b'[', b'B'];
        let (out, segs) = macro_match(&mut buf, &ms);
        assert_eq!(out, vec![0x1B, b'[', b'B']);
        assert!(segs.is_empty());
        assert!(buf.is_empty());
    }

    #[test]
    fn macro_match_trigger_intercepted() {
        // 触发键被拦截：命中时只发宏内容，触发字节与缓冲残留都不再发出
        let ms = vec![mac("^M", "^M^M")];
        let mut buf = b"ab\r".to_vec();
        let (out, segs) = macro_match(&mut buf, &ms);
        assert!(out.is_empty());
        assert_eq!(segs, vec![config::Segment::Bytes(b"\r\r".to_vec())]);
        assert!(buf.is_empty());
    }

    #[test]
    fn macro_match_longest_wins() {
        // 两个宏都能命中时取最长触发，触发序列整体被拦截
        let ms = vec![mac("^M", "X"), mac("a^M", "YY")];
        let mut buf = b"a\r".to_vec();
        let (out, segs) = macro_match(&mut buf, &ms);
        assert!(out.is_empty());
        assert_eq!(segs, vec![config::Segment::Bytes(b"YY".to_vec())]);
    }

    #[test]
    fn macro_match_with_delay() {
        // 宏内容带延时：分段保留
        let ms = vec![mac("^M", "^M~100^M")];
        let mut buf = b"\r".to_vec();
        let (_, segs) = macro_match(&mut buf, &ms);
        assert_eq!(
            segs,
            vec![
                config::Segment::Bytes(b"\r".to_vec()),
                config::Segment::Delay(100),
                config::Segment::Bytes(b"\r".to_vec()),
            ]
        );
    }

    #[test]
    fn macro_match_no_macros() {
        let mut buf = b"z".to_vec();
        let (out, segs) = macro_match(&mut buf, &[]);
        assert_eq!(out, b"z".to_vec());
        assert!(segs.is_empty());
    }

    #[test]
    fn base_sequences() {
        assert_eq!(base_seq(egui::Key::Enter, false, '['), vec![b'\r']);
        assert_eq!(base_seq(egui::Key::ArrowUp, false, 'O'), vec![0x1b, b'O', b'A']);
        assert_eq!(base_seq(egui::Key::ArrowUp, false, '['), vec![0x1b, b'[', b'A']);
        assert_eq!(
            base_seq(egui::Key::ArrowUp, true, '['),
            b"\x1b[1;2A".to_vec()
        );
    }

    #[test]
    fn base_seq_printable_chars() {
        // 普通字符按键直接发字符本身（含大小写），不再依赖 Text 事件
        assert_eq!(base_seq(egui::Key::A, false, '['), b"a".to_vec());
        assert_eq!(base_seq(egui::Key::A, true, '['), b"A".to_vec());
        assert_eq!(base_seq(egui::Key::Num1, false, '['), b"1".to_vec());
        assert_eq!(base_seq(egui::Key::Space, false, '['), b" ".to_vec());
        // 功能键仍走控制序列
        assert_eq!(base_seq(egui::Key::PageUp, false, '['), b"\x1b[5~".to_vec());
        assert_eq!(base_seq(egui::Key::PageDown, false, '['), b"\x1b[6~".to_vec());
        // Home/End 旧式 ~ 序列，不随应用光标模式变 SS3
        assert_eq!(base_seq(egui::Key::Home, false, '['), b"\x1b[1~".to_vec());
        assert_eq!(base_seq(egui::Key::Home, false, 'O'), b"\x1b[1~".to_vec());
        assert_eq!(base_seq(egui::Key::End, false, '['), b"\x1b[4~".to_vec());
        assert_eq!(base_seq(egui::Key::End, false, 'O'), b"\x1b[4~".to_vec());
        assert_eq!(base_seq(egui::Key::Home, true, '['), b"\x1b[1;2H".to_vec());
        assert_eq!(base_seq(egui::Key::End, true, '['), b"\x1b[1;2F".to_vec());
    }

    #[test]
    fn norm_sel_orders() {
        assert_eq!(
            norm_sel(Some((5, 3)), Some((2, 1))),
            Some(((2, 1), (5, 3)))
        );
        assert_eq!(norm_sel(None, Some((1, 1))), None);
    }

    #[test]
    fn xterm_256_ramp() {
        assert_eq!(xterm256(16), [0, 0, 0]);
        assert_eq!(xterm256(231), [255, 255, 255]);
        assert_eq!(xterm256(232), [8, 8, 8]);
        assert_eq!(xterm256(255), [238, 238, 238]);
    }
}

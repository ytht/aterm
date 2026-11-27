//! 配置数据结构与持久化（JSON，便携存放于 exe 同目录 config.json）。
//!
//! 字段命名与语义尽量对齐 fterm 的 fterm.xml / fterm.ini，方便导入旧地址簿。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 连接协议。fterm 用 Is_SSH 区分，这里显式化并预留串口等扩展位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    Telnet,
    Ssh,
}

impl Protocol {
    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Telnet => 23,
            Protocol::Ssh => 22,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Telnet => "Telnet",
            Protocol::Ssh => "SSH",
        }
    }
}

/// 字符编码。BBS 站点常见 GBK(简体)、Big5(繁体)、UTF-8。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CharEncoding {
    Gbk,
    Big5,
    Utf8,
    Latin1,
}

impl CharEncoding {
    pub fn label(self) -> &'static str {
        match self {
            CharEncoding::Gbk => "GBK",
            CharEncoding::Big5 => "Big5",
            CharEncoding::Utf8 => "UTF-8",
            CharEncoding::Latin1 => "ISO-8859-1",
        }
    }
    /// fterm 的 inputconvcode / outputconvcode：0=GB 0x8080 1=Big5 0x8080 2=UTF-8
    pub fn from_fterm_code(code: i64) -> CharEncoding {
        match code {
            1 => CharEncoding::Big5,
            2 => CharEncoding::Utf8,
            3 => CharEncoding::Latin1,
            _ => CharEncoding::Gbk,
        }
    }
}

/// 输入内码转换：键入/粘贴/输入法文本先按源内码解码，再按站点「字符编码」发送。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum InputConv {
    #[default]
    None,
    GbToBig5,
    Big5ToGb,
}

impl InputConv {
    pub fn label(self) -> &'static str {
        match self {
            InputConv::None => "none",
            InputConv::GbToBig5 => "GB->Big5",
            InputConv::Big5ToGb => "Big5->GB",
        }
    }
    /// 源内码（None 表示不转换）。
    pub fn source(self) -> Option<CharEncoding> {
        match self {
            InputConv::None => None,
            InputConv::GbToBig5 => Some(CharEncoding::Gbk),
            InputConv::Big5ToGb => Some(CharEncoding::Big5),
        }
    }
}

/// SOCKS5 代理（对应 fterm 的 [ProxyN] socksproxylevel=5）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proxy {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// 是否使用用户名/密码认证（FTERM 的 sockshasauth）。
    pub auth: bool,
    pub user: String,
    pub pass: String,
}

impl Default for Proxy {
    fn default() -> Self {
        Proxy {
            name: "本地 SOCKS5".into(),
            host: "127.0.0.1".into(),
            port: 7890,
            auth: false,
            user: String::new(),
            pass: String::new(),
        }
    }
}

/// 自动登录脚本：等提示符 → 发账号 → 等提示符 → 发密码 → 可选附加命令。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AutoLogin {
    pub enabled: bool,
    /// 第一个提示符片段，例如 "login:" 或 "用户名"。
    pub user_prompt: String,
    pub user: String,
    /// 第二个提示符片段，例如 "Password:" 或 "密码"。
    pub pass_prompt: String,
    pub pass: String,
    /// 登录后额外发送的一行命令（留空则不发）。
    pub after: String,
    /// 连接后命令：连接建立后几乎立刻发送，早于自动/手动输入账号（留空则不发）。
    #[serde(default)]
    pub post_connect: String,
}

/// 单个站点条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub proto: Protocol,
    /// 分组名，空串表示"未分组"。对齐 fterm.xml 的 分组 层级。
    #[serde(default)]
    pub group: String,
    pub cols: u16,
    pub rows: u16,
    pub encoding: CharEncoding,
    /// 输入内码转换（none / GB->Big5 / Big5->GB），只影响键入/粘贴/输入法文本。
    #[serde(default)]
    pub input_conv: InputConv,
    /// 上报给远端的 TERM 变量，fterm 默认 vt100。
    #[serde(default = "default_termtype")]
    pub termtype: String,
    /// 使用哪个代理，None 表示直连。
    #[serde(default)]
    pub proxy: Option<usize>,
    #[serde(default)]
    pub autologin: AutoLogin,
    /// 防掉线：发送内容（支持 ^M ^[ 等 fterm 记法）与间隔秒数。
    #[serde(default = "default_keepalive_text")]
    pub keepalive_text: String,
    #[serde(default = "default_keepalive_sec")]
    pub keepalive_sec: u32,
    /// 收到 BEL 时是否播放提示音（fterm 的 ringscheme）。
    #[serde(default = "default_true")]
    pub ring_bell: bool,
}

fn default_termtype() -> String {
    "vt100".into()
}
fn default_keepalive_text() -> String {
    "^@".into()
}
fn default_keepalive_sec() -> u32 {
    60
}
fn default_true() -> bool {
    true
}

impl Default for Site {
    fn default() -> Self {
        Site {
            name: "新建站点".into(),
            host: String::new(),
            port: 23,
            proto: Protocol::Telnet,
            group: String::new(),
            cols: 80,
            rows: 24,
            encoding: CharEncoding::Gbk,
            input_conv: InputConv::None,
            termtype: default_termtype(),
            proxy: None,
            autologin: AutoLogin::default(),
            keepalive_text: default_keepalive_text(),
            keepalive_sec: default_keepalive_sec(),
            ring_bell: true,
        }
    }
}

/// 按键宏：检测到 `trigger` 序列时，改为发送 `keys` 序列。
/// 两者都用 fterm 记法（`^M`、`^[[A`、`~100` 延时等）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Macro {
    /// 备注名（可空）
    #[serde(default)]
    pub name: String,
    /// 触发序列：用户按下这些键时命中
    #[serde(default)]
    pub trigger: String,
    /// 命中后实际发送的序列
    #[serde(default)]
    pub keys: String,
    /// 是否启用
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// 16 色 ANSI 调色板 + 前景/背景/光标色，对应 fterm 的 [PALETTE N]。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorScheme {
    pub name: String,
    /// 16 个 RGB。
    pub colors: [[u8; 3]; 16],
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub cursor: [u8; 3],
    pub selection: [u8; 3],
}

impl Default for ColorScheme {
    fn default() -> Self {
        // fterm PALETTE0：标准 VGA 16 色，clSilver 作为默认前景，接近旧版观感。
        ColorScheme {
            name: "FTERM 经典".into(),
            colors: [
                [0, 0, 0],         // 0 black
                [170, 0, 0],       // 1 maroon
                [0, 170, 0],       // 2 green
                [170, 85, 0],      // 3 olive
                [0, 0, 170],       // 4 navy
                [170, 0, 170],     // 5 purple
                [0, 170, 170],     // 6 teal
                [192, 192, 192],   // 7 silver
                [85, 85, 85],      // 8 gray
                [255, 85, 85],     // 9 red
                [85, 255, 85],     // 10 lime
                [255, 255, 85],    // 11 yellow
                [85, 85, 255],     // 12 blue
                [255, 85, 255],    // 13 fuchsia
                [85, 255, 255],    // 14 aqua
                [255, 255, 255],   // 15 white
            ],
            fg: [192, 192, 192],
            bg: [0, 0, 0],
            cursor: [192, 192, 192],
            selection: [68, 102, 153],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub sites: Vec<Site>,
    /// 分组名列表（可含暂无站点的空分组）；"" 代表未分组，不在此列表中。
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub proxies: Vec<Proxy>,
    /// 按键宏列表：按顺序匹配触发序列（兼容旧版 F1..F12 表）。
    #[serde(default, deserialize_with = "de_macros")]
    pub macros: Vec<Macro>,
    #[serde(default)]
    pub schemes: Vec<ColorScheme>,
    #[serde(default)]
    pub scheme: usize,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default = "default_true")]
    pub cursor_blink: bool,
    /// 选中后自动复制到剪贴板（fterm 的 CopyToClipboard）。
    #[serde(default)]
    pub copy_on_select: bool,
    /// 关闭窗口时若有活动连接则询问。
    #[serde(default = "default_true")]
    pub confirm_quit: bool,
}

fn default_font_size() -> f32 {
    16.0
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sites: vec![
                Site {
                    name: "水木社区".into(),
                    host: "newsmth.net".into(),
                    port: 22,
                    proto: Protocol::Ssh,
                    group: "常用BBS".into(),
                    encoding: CharEncoding::Gbk,
                    ..Default::default()
                },
                Site {
                    name: "枫林在线".into(),
                    host: "bbs.fenglin.info".into(),
                    port: 2322,
                    proto: Protocol::Ssh,
                    group: "常用BBS".into(),
                    encoding: CharEncoding::Utf8,
                    ..Default::default()
                },
                Site {
                    name: "日月光华".into(),
                    host: "bbs.fudan.edu.cn".into(),
                    port: 2323,
                    proto: Protocol::Telnet,
                    group: "常用BBS".into(),
                    encoding: CharEncoding::Gbk,
                    ..Default::default()
                },
                Site {
                    name: "lilacbbs".into(),
                    host: "lilacbbs.com".into(),
                    port: 23,
                    proto: Protocol::Telnet,
                    group: "常用BBS".into(),
                    encoding: CharEncoding::Gbk,
                    ..Default::default()
                },
                Site {
                    name: "PTT".into(),
                    host: "140.112.172.2".into(),
                    port: 22,
                    proto: Protocol::Ssh,
                    group: "常用BBS".into(),
                    encoding: CharEncoding::Big5,
                    autologin: AutoLogin {
                        enabled: true,
                        user: "bbs".into(),
                        ..Default::default()
                    },
                    keepalive_text: "^g".into(),
                    ..Default::default()
                },
            ],
            groups: vec!["常用BBS".into()],
            proxies: vec![Proxy::default()],
            macros: default_macros(),
            schemes: vec![ColorScheme::default()],
            scheme: 0,
            font_size: default_font_size(),
            cursor_blink: true,
            copy_on_select: false,
            confirm_quit: true,
        }
    }
}

/// 默认按键宏：触发序列 → 发送序列。默认全部关闭，避免影响正常输入。
fn default_macros() -> Vec<Macro> {
    vec![
        Macro {
            name: "回车连发".into(),
            trigger: "^M".into(),
            keys: "^M^M".into(),
            enabled: false,
        },
        Macro {
            name: "刷新".into(),
            trigger: "^[[15~".into(), // F5
            keys: "^L".into(),
            enabled: false,
        },
        Macro {
            name: "回帖".into(),
            trigger: "^[[17~".into(), // F6
            keys: "u^M".into(),
            enabled: false,
        },
    ]
}

/// 兼容读取：新版是数组；旧版是 {"F1":{name,keys},...} 表，
/// 迁移为「按 Fn 触发」的列表项（Fn 的 xterm 序列）。
fn de_macros<'de, D>(d: D) -> Result<Vec<Macro>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Array(_) => {
            serde_json::from_value(v).map_err(serde::de::Error::custom)
        }
        serde_json::Value::Object(map) => {
            let mut out = Vec::new();
            for (k, item) in map {
                let name = item
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let keys = item
                    .get("keys")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if keys.is_empty() {
                    continue;
                }
                // F1..F4 用 SS3 序列，F5..F12 用 CSI 序列
                let trigger = match k.as_str() {
                    "F1" => "^[[11~",
                    "F2" => "^[[12~",
                    "F3" => "^[[13~",
                    "F4" => "^[[14~",
                    "F5" => "^[[15~",
                    "F6" => "^[[17~",
                    "F7" => "^[[18~",
                    "F8" => "^[[19~",
                    "F9" => "^[[20~",
                    "F10" => "^[[21~",
                    "F11" => "^[[23~",
                    "F12" => "^[[24~",
                    _ => continue,
                };
                out.push(Macro {
                    name,
                    trigger: trigger.into(),
                    keys,
                    enabled: false,
                });
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

impl Settings {
    pub fn scheme(&self) -> ColorScheme {
        self.schemes
            .get(self.scheme)
            .or(self.schemes.first())
            .cloned()
            .unwrap_or_else(fallback_scheme)
    }
}

impl ColorScheme {
    pub fn fg_rgb(&self) -> [u8; 3] {
        self.fg
    }
    pub fn bg_rgb(&self) -> [u8; 3] {
        self.bg
    }
    pub fn cursor_rgb(&self) -> [u8; 3] {
        self.cursor
    }
    pub fn selection_rgb(&self) -> [u8; 3] {
        self.selection
    }
    pub fn pallet(&self, i: u8) -> [u8; 3] {
        self.colors.get(i as usize).copied().unwrap_or([192, 192, 192])
    }
}

/// 兜底配色（scheme() 内部 clone 返回）。
pub fn fallback_scheme() -> ColorScheme {
    ColorScheme {
        name: "fallback".into(),
        colors: [
            [0, 0, 0], [170, 0, 0], [0, 170, 0], [170, 85, 0],
            [0, 0, 170], [170, 0, 170], [0, 170, 170], [192, 192, 192],
            [85, 85, 85], [255, 85, 85], [85, 255, 85], [255, 255, 85],
            [85, 85, 255], [255, 85, 255], [85, 255, 255], [255, 255, 255],
        ],
        fg: [192, 192, 192],
        bg: [0, 0, 0],
        cursor: [192, 192, 192],
        selection: [68, 102, 153],
    }
}

/// 配置文件路径：与 exe 同目录（便携化）。
pub fn config_path() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    exe_dir.join("config.json")
}

/// 旧版配置路径（%APPDATA%/ATerm/config.json），用于一次性迁移。
fn legacy_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|b| b.join("ATerm").join("config.json"))
}

pub fn load() -> Settings {
    let path = config_path();
    // 迁移：新路径不存在但旧路径存在时，复制过来
    if !path.exists() {
        if let Some(old) = legacy_config_path() {
            if old.exists() {
                let _ = std::fs::copy(&old, &path);
            }
        }
    }
    let mut s = match std::fs::read_to_string(&path) {
        Ok(txt) => serde_json::from_str::<Settings>(&txt).unwrap_or_else(|e| {
            eprintln!("配置文件格式错误({e})，已使用默认配置：{}", path.display());
            Settings::default()
        }),
        Err(_) => Settings::default(),
    };
    // 归一化：把站点引用到的分组补进 groups（保留用户创建的空分组）
    for site in &s.sites {
        if !site.group.is_empty() && !s.groups.contains(&site.group) {
            s.groups.push(site.group.clone());
        }
    }
    // 迁移：旧版默认心跳 ^L(Ctrl+L) 会触发 BBS 刷屏，统一改为 ^@(NUL)
    let mut migrated = false;
    for site in s.sites.iter_mut() {
        if site.keepalive_text == "^L" {
            site.keepalive_text = "^@".to_string();
            migrated = true;
        }
    }
    // 清理：旧版遗留的无用字段（scrollback / font_family）检测到时立即保存移除
    let obsolete = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.as_object()
                .map(|o| o.contains_key("scrollback") || o.contains_key("font_family"))
        })
        .unwrap_or(false);
    if migrated || obsolete {
        let _ = save(&s);
    }
    s
}

pub fn save(s: &Settings) -> std::io::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(s).unwrap())
}

/// 解析 fterm 风格的转义记法：`^M`=0x0D、`^[`=0x1B、`^@`=0x00、`^^`=字面 `^`，
/// 以及 `^[A` 这类 CSI 序列（`^[[` = ESC + `[`）。返回待发送字节。
/// `~100` 这类延时标记在此函数中被忽略（只取字节）；需要延时时用 `parse_segments`。
pub fn parse_keyspec(src: &str) -> Vec<u8> {
    parse_segments(src)
        .into_iter()
        .flat_map(|s| match s {
            Segment::Bytes(b) => b,
            Segment::Delay(_) => Vec::new(),
        })
        .collect()
}

/// 宏分段：字节段与延时段交替。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Bytes(Vec<u8>),
    /// 延时毫秒
    Delay(u64),
}

/// 把宏文本解析为「字节段 / 延时段」序列。
/// `~数字` 表示延时该毫秒数（如 `~100`、`~1000`）；`~~` 表示字面 `~`。
pub fn parse_segments(src: &str) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let b = src.as_bytes();
    let mut i = 0usize;
    macro_rules! flush {
        () => {
            if !cur.is_empty() {
                out.push(Segment::Bytes(std::mem::take(&mut cur)));
            }
        };
    }
    while i < b.len() {
        // ~数字 → 延时；~~ → 字面 ~
        if b[i] == b'~' {
            if i + 1 < b.len() && b[i + 1] == b'~' {
                cur.push(b'~');
                i += 2;
                continue;
            }
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 {
                let ms: u64 = src[i + 1..j].parse().unwrap_or(0);
                flush!();
                // 上限 60 秒，避免误输入长时间卡住发送
                out.push(Segment::Delay(ms.min(60_000)));
                i = j;
                continue;
            }
            cur.push(b'~');
            i += 1;
            continue;
        }
        // ^!数字 → 大键盘 Ctrl+数字宏触发键：内部标识 ESC ! 数字（普通输入不会产生）
        if b[i] == b'^' && i + 2 < b.len() && b[i + 1] == b'!' && b[i + 2].is_ascii_digit() {
            cur.extend_from_slice(&[0x1B, b'!', b[i + 2]]);
            i += 3;
            continue;
        }
        // ^#数字 → 小键盘 Ctrl+数字宏触发键：内部标识 ESC # 数字
        if b[i] == b'^' && i + 2 < b.len() && b[i + 1] == b'#' && b[i + 2].is_ascii_digit() {
            cur.extend_from_slice(&[0x1B, b'#', b[i + 2]]);
            i += 3;
            continue;
        }
        if b[i] == b'^' && i + 1 < b.len() {
            let c = b[i + 1];
            let ctrl = match c {
                b'@' => Some(0u8),
                b'[' => Some(0x1B),
                b']' => Some(0x1D),
                b'^' => {
                    // ^^^ 记法：^^ 表示字面脱字符
                    if i + 2 < b.len() && b[i + 2] == b'^' {
                        i += 2;
                        Some(b'^')
                    } else {
                        i += 1;
                        None
                    }
                }
                b'M' | b'm' => Some(0x0D),
                b'J' | b'j' => Some(0x0A),
                b'H' | b'h' => Some(0x08),
                b'I' => Some(0x09),
                _ if c.is_ascii_alphabetic() => Some((c.to_ascii_lowercase() - b'a') + 1),
                _ if c.is_ascii_punctuation() => {
                    let up = c.to_ascii_uppercase();
                    if matches!(up, b'_' | b'?' | b'[' | b']') {
                        Some((up as u32 % 32) as u8)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            match ctrl {
                Some(v) => {
                    cur.push(v);
                    i += 2;
                }
                None => {
                    cur.push(b'^');
                    i += 1;
                }
            }
        } else {
            // 非 ^ 起始：按 UTF-8 原样送出（中文宏内容会转成目标编码前由上层处理）
            let ch = src[i..].chars().next().unwrap();
            let bl = ch.len_utf8();
            cur.extend_from_slice(&src.as_bytes()[i..i + bl]);
            i += bl;
        }
    }
    flush!();
    out
}

/// 把字节序列还原成 fterm 记法（触发栏按键设定后回填显示用）。
pub fn bytes_to_keyspec(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // Ctrl+数字触发标识（ESC ! 数字 = 大键盘，ESC # 数字 = 小键盘）
        if i + 2 < bytes.len()
            && bytes[i] == 0x1B
            && (bytes[i + 1] == b'!' || bytes[i + 1] == b'#')
            && bytes[i + 2].is_ascii_digit()
        {
            let mark = if bytes[i + 1] == b'!' { '!' } else { '#' };
            out.push_str(&format!("^{mark}{}", bytes[i + 2] as char));
            i += 3;
            continue;
        }
        let b = bytes[i];
        i += 1;
        match b {
            0x00 => out.push_str("^@"),
            0x08 => out.push_str("^H"),
            0x09 => out.push_str("^I"),
            0x0A => out.push_str("^J"),
            0x0D => out.push_str("^M"),
            0x1B => out.push_str("^["),
            0x1C => out.push_str("^\\"),
            0x1D => out.push_str("^]"),
            0x1F => out.push_str("^_"),
            0x7F => out.push_str("^?"),
            0x01..=0x1A => out.push_str(&format!("^{}", (b'a' + b - 1) as char)),
            b'^' => out.push_str("^^"),
            b'~' => out.push_str("~~"),
            0x20..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// 把宏文本转成给定编码的字节（BBS 上 GBK 站点的中文宏需要按 GBK 送出）。
/// 先按 fterm 记法解析出原始字节，再逐段转换：ASCII 与控制字节原样保留，
/// 连续的可打印 UTF-8 段按目标编码重编码。
pub fn encode_macro(src: &str, enc: CharEncoding) -> Vec<u8> {
    let raw = parse_keyspec(src);
    if enc == CharEncoding::Utf8 {
        return raw;
    }
    let codec = codec_for(enc);
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0usize;
    while i < raw.len() {
        let b = raw[i];
        if b < 0x20 || b == 0x7f {
            out.push(b);
            i += 1;
            continue;
        }
        if b < 0x80 {
            out.push(b);
            i += 1;
            continue;
        }
        // 可打印 UTF-8 序列起始
        let len = if b >= 0xF0 {
            4
        } else if b >= 0xE0 {
            3
        } else if b >= 0xC0 {
            2
        } else {
            1
        };
        let end = (i + len).min(raw.len());
        if let Ok(s) = std::str::from_utf8(&raw[i..end]) {
            let (cow, _, _) = codec.encode(s);
            out.extend_from_slice(&cow);
        } else {
            out.extend_from_slice(&raw[i..end]);
        }
        i = end;
    }
    out
}

/// 输入内码转换：把输入文本先按 from 内码解码为 Unicode，再按站点编码 to 编码。
/// ASCII 与控制字节原样保留；无法按 from 解码的片段按 Unicode 直通（兼容直接键入中文）。
pub fn convert_input(text: &str, from: CharEncoding, to: CharEncoding) -> Vec<u8> {
    if from == to {
        let (b, _, _) = codec_for(to).encode(text);
        return b.into_owned();
    }
    // 输入是合法 UTF-8（键入/粘贴/输入法的 Unicode 文本）→ 先按转换方向做
    // 简繁逐字映射（目标字符集缺字时避免编码成 HTML 数字引用），再按目标编码送出。
    // GBK/Big5 内码字节几乎必然不是合法 UTF-8，不会误入此分支。
    if std::str::from_utf8(text.as_bytes()).is_ok() {
        let mapped: String = text
            .chars()
            .map(|c| match (from, to) {
                (CharEncoding::Gbk, CharEncoding::Big5) => crate::zhconv::s2t(c),
                (CharEncoding::Big5, CharEncoding::Gbk) => crate::zhconv::t2s(c),
                _ => c,
            })
            .collect();
        let (b, _, _) = codec_for(to).encode(&mapped);
        return b.into_owned();
    }
    // 内码字节串：ASCII 原样，双字节按源内码解码再转目标编码；
    // 解不出的字节按目标编码直通（兜底，不丢输入）。
    let src = codec_for(from);
    let dst = codec_for(to);
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0usize;
    while i < raw.len() {
        let b = raw[i];
        if b < 0x80 {
            out.push(b);
            i += 1;
            continue;
        }
        if i + 1 < raw.len() {
            let (cow, _, had_errors) = src.decode(&raw[i..i + 2]);
            if !had_errors && cow.chars().count() == 1 {
                let c = cow.chars().next().unwrap();
                let c = match (from, to) {
                    (CharEncoding::Gbk, CharEncoding::Big5) => crate::zhconv::s2t(c),
                    (CharEncoding::Big5, CharEncoding::Gbk) => crate::zhconv::t2s(c),
                    _ => c,
                };
                let mut buf = [0u8; 4];
                let (e, _, _) = dst.encode(c.encode_utf8(&mut buf));
                out.extend_from_slice(&e);
                i += 2;
                continue;
            }
        }
        let (e, _, _) = dst.encode(&text[i..i + 1]);
        out.extend_from_slice(&e);
        i += 1;
    }
    out
}

/// 编码到 encoding_rs 的映射。
pub fn codec_for(enc: CharEncoding) -> &'static encoding_rs::Encoding {
    match enc {
        CharEncoding::Gbk => encoding_rs::GBK,
        CharEncoding::Big5 => encoding_rs::BIG5,
        CharEncoding::Utf8 => encoding_rs::UTF_8,
        CharEncoding::Latin1 => encoding_rs::WINDOWS_1252,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyspec_basic() {
        assert_eq!(parse_keyspec("^M"), vec![0x0D]);
        assert_eq!(parse_keyspec("q^M"), vec![b'q', 0x0D]);
        assert_eq!(parse_keyspec("^^x"), vec![b'^', b'x']);
        // ^[[A -> ESC [ A （方向键上）
        assert_eq!(parse_keyspec("^[[A"), vec![0x1B, b'[', b'A']);
        assert_eq!(parse_keyspec(""), Vec::<u8>::new());
    }

    #[test]
    fn keyspec_ctrl_letters() {
        assert_eq!(parse_keyspec("^L"), vec![0x0C]);
        assert_eq!(parse_keyspec("^@u"), vec![0x00, b'u']);
    }

    #[test]
    fn keyspec_after_login_sequence() {
        // 用户示例：^M a b c d ^[[A ^[[B ^[[C ^[[D
        let got = parse_keyspec("^M a b c d ^[[A ^[[B ^[[C ^[[D");
        let want = vec![
            0x0D, b' ', b'a', b' ', b'b', b' ', b'c', b' ', b'd', b' ',
            0x1B, b'[', b'A', b' ',
            0x1B, b'[', b'B', b' ',
            0x1B, b'[', b'C', b' ',
            0x1B, b'[', b'D',
        ];
        assert_eq!(got, want);
        // Tab 与 NUL
        assert_eq!(parse_keyspec("^I^@"), vec![0x09, 0x00]);
        // GBK 下控制字节原样、中文按编码转换
        let b = encode_macro("^M中文", CharEncoding::Gbk);
        assert_eq!(&b[..1], &[0x0D]);
        assert!(b.len() > 1);
    }

    #[test]
    fn segments_delay() {
        // ~100 / ~1000 延时
        let s = parse_segments("^M~100a~1000b");
        assert_eq!(
            s,
            vec![
                Segment::Bytes(vec![0x0D]),
                Segment::Delay(100),
                Segment::Bytes(vec![b'a']),
                Segment::Delay(1000),
                Segment::Bytes(vec![b'b']),
            ]
        );
        // parse_keyspec 忽略延时只取字节
        assert_eq!(parse_keyspec("^M~100a~1000b"), vec![0x0D, b'a', b'b']);
        // 连续延时合并为相邻段
        assert_eq!(
            parse_segments("~50~60"),
            vec![Segment::Delay(50), Segment::Delay(60)]
        );
        // 上限 60 秒
        assert_eq!(parse_segments("~999999"), vec![Segment::Delay(60_000)]);
    }

    #[test]
    fn segments_literal_tilde() {
        // ~~ 表示字面 ~；单独的 ~ 也按字面处理
        assert_eq!(parse_keyspec("~~"), vec![b'~']);
        assert_eq!(parse_keyspec("a~b"), vec![b'a', b'~', b'b']);
        assert_eq!(parse_segments("~~100"), vec![Segment::Bytes(b"~100".to_vec())]);
    }

    #[test]
    fn keyspec_roundtrip() {
        // 录制回填：字节 → 记法 → 字节 应一致
        for src in ["^M", "^M^M", "^[[A", "^I", "^@", "abc", "^^", "~~"] {
            let b = parse_keyspec(src);
            let back = bytes_to_keyspec(&b);
            assert_eq!(parse_keyspec(&back), b, "src={src} back={back}");
        }
        assert_eq!(bytes_to_keyspec(&[0x0D]), "^M");
        assert_eq!(bytes_to_keyspec(&[0x1B, b'[', b'A']), "^[[A");
    }

    #[test]
    fn ctrl_digit_keyspec() {
        // ^!数字 = Ctrl+数字宏触发标识：ESC ! 数字
        assert_eq!(parse_keyspec("^!1"), vec![0x1B, b'!', b'1']);
        assert_eq!(parse_keyspec("^!0"), vec![0x1B, b'!', b'0']);
        assert_eq!(parse_keyspec("^!9^M"), vec![0x1B, b'!', b'9', 0x0D]);
        // 回填记法一致
        assert_eq!(bytes_to_keyspec(&[0x1B, b'!', b'5']), "^!5");
        assert_eq!(bytes_to_keyspec(&parse_keyspec("^!3abc")), "^!3abc");
        // ^! 后非数字：按字面处理
        assert_eq!(parse_keyspec("^!x"), vec![b'^', b'!', b'x']);
    }

    #[test]
    fn numpad_ctrl_keyspec() {
        assert_eq!(parse_keyspec("^#1"), vec![0x1B, b'#', b'1']);
        assert_eq!(parse_keyspec("^#0"), vec![0x1B, b'#', b'0']);
        assert_eq!(bytes_to_keyspec(&[0x1B, b'#', b'7']), "^#7");
        assert_eq!(bytes_to_keyspec(&parse_keyspec("^#3^!2")), "^#3^!2");
    }

    #[test]
    fn input_conv_roundtrip() {
        // Unicode 直通：键入中文按目标编码送出（\u 转义避免源码文件编码歧义）
        let zw = "\u{4E2D}\u{6587}";
        let big5 = convert_input(zw, CharEncoding::Gbk, CharEncoding::Big5);
        let (back, _, err) = encoding_rs::BIG5.decode(&big5);
        assert!(!err && back == zw);
        // GBK/Big5 内码字节（非 UTF-8 裸字节串）按源内码解码再转目标编码。
        // GBK/Big5 双字节序列不构成合法 UTF-8，可安全地按字节串承载（仅测试用）。
        let (gb, _, _) = encoding_rs::GBK.encode(zw);
        let raw = unsafe { std::str::from_utf8_unchecked(&gb) };
        let got = convert_input(raw, CharEncoding::Gbk, CharEncoding::Big5);
        let (txt, _, err2) = encoding_rs::BIG5.decode(&got);
        assert!(!err2 && txt == zw);
        // 反向 Big5→GB 往返
        let raw2 = unsafe { std::str::from_utf8_unchecked(&big5) };
        let got2 = convert_input(raw2, CharEncoding::Big5, CharEncoding::Gbk);
        let (txt2, _, err3) = encoding_rs::GBK.decode(&got2);
        assert!(!err3 && txt2 == zw);
        // ASCII 原样
        assert_eq!(convert_input("abc", CharEncoding::Gbk, CharEncoding::Big5), b"abc");
        // 简体缺字回归：华 不在 Big5 字符集，须映射为 華 而非数字引用
        let hx = "\u{534E}\u{590F}";
        let got4 = convert_input(hx, CharEncoding::Gbk, CharEncoding::Big5);
        let (txt4, _, err4) = encoding_rs::BIG5.decode(&got4);
        assert!(!err4 && txt4 == "\u{83EF}\u{590F}");
        // 反向：華夏 → 华夏
        let got5 = convert_input("\u{83EF}\u{590F}", CharEncoding::Big5, CharEncoding::Gbk);
        let (txt5, _, err5) = encoding_rs::GBK.decode(&got5);
        assert!(!err5 && txt5 == hx);
    }

    #[test]
    fn macros_legacy_migration() {
        // 旧版 F1..F12 表能迁移为触发式列表
        let json = r#"{"macros":{"F6":{"name":"回帖","keys":"u^M"},"F2":{"name":"地址簿","keys":""}}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.macros.len(), 1);
        assert_eq!(s.macros[0].trigger, "^[[17~");
        assert_eq!(s.macros[0].keys, "u^M");
        assert!(!s.macros[0].enabled);
        // 新版数组格式正常读取
        let json2 = r#"{"macros":[{"trigger":"^M","keys":"^M^M","enabled":true}]}"#;
        let s2: Settings = serde_json::from_str(json2).unwrap();
        assert_eq!(s2.macros.len(), 1);
        assert!(s2.macros[0].enabled);
        assert_eq!(s2.macros[0].trigger, "^M");
    }

    #[test]
    fn serde_roundtrip() {
        let s = Settings::default();
        let txt = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&txt).unwrap();
        assert_eq!(back.sites.len(), s.sites.len());
        assert_eq!(back.scheme().colors[7], s.scheme().colors[7]);
    }

    #[test]
    fn default_address_book() {
        let s = Settings::default();
        assert_eq!(s.groups, vec!["常用BBS".to_string()]);
        assert_eq!(s.sites.len(), 5);
        assert!(s.sites.iter().all(|x| x.group == "常用BBS"));
        let ptt = s.sites.iter().find(|x| x.name == "PTT").unwrap();
        assert_eq!(ptt.host, "140.112.172.2");
        assert_eq!(ptt.port, 22);
        assert_eq!(ptt.proto, Protocol::Ssh);
        assert_eq!(ptt.encoding, CharEncoding::Big5);
        assert!(ptt.autologin.enabled);
        assert_eq!(ptt.autologin.user, "bbs");
        assert_eq!(ptt.keepalive_text, "^g");
        assert!(s.sites.iter().filter(|x| x.name != "PTT").all(|x| x.keepalive_text == "^@"));
        let fudan = s.sites.iter().find(|x| x.name == "日月光华").unwrap();
        assert_eq!(fudan.port, 2323);
        assert_eq!(fudan.proto, Protocol::Telnet);
        let lilac = s.sites.iter().find(|x| x.name == "lilacbbs").unwrap();
        assert_eq!(lilac.host, "lilacbbs.com");
        assert_eq!(lilac.port, 23);
    }

    #[test]
    fn legacy_optional_fields() {
        // 老配置缺少新字段时仍应能读入。
        let json = r#"{"sites":[{"name":"a","host":"b","port":23,"proto":"Telnet","cols":80,"rows":24,"encoding":"Gbk"}],"proxies":[]}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.sites[0].termtype, "vt100");
        assert_eq!(s.sites[0].keepalive_sec, 60);
    }
}

//! 传输与会话层：Telnet / SSH + SOCKS5 代理 + GBK/Big5/UTF-8 增量解码 + 心跳 + 自动登录。
//!
//! 每个会话一个 tokio 后台任务：
//!   socket → IAC 协商 / SSH 通道 → 解码成字符 → 写入 Arc<Mutex<Term>>
//!   UI 键盘/粘贴 → Cmd::Bytes → 编码后的字节 → socket
//! UI 线程通过 Conn::poll 收事件，通过 Conn::term 读屏幕。

use crate::config::{self, CharEncoding, Proxy, Site, codec_for, encode_macro};
use crate::term::Term;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// Telnet 选项号。
pub mod opt {
    pub const ECHO: u8 = 1;
    pub const SGA: u8 = 3;
    pub const STATUS: u8 = 5;
    pub const TTYPE: u8 = 24;
    pub const NAWS: u8 = 31;
    pub const NEW_ENVIRON: u8 = 39;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Negotiating,
    Connected,
    Closed,
    Failed,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Connecting => "连接中",
            Phase::Negotiating => "协商中",
            Phase::Connected => "已连接",
            Phase::Closed => "已断开",
            Phase::Failed => "失败",
        }
    }
    pub fn alive(self) -> bool {
        matches!(
            self,
            Phase::Connecting | Phase::Negotiating | Phase::Connected
        )
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    pub phase: Phase,
    pub msg: String,
}

pub enum Cmd {
    /// 键盘/粘贴产生的原始字节（已按站点编码转换）
    Bytes(Vec<u8>),
    /// 带延时的分段发送（宏中的 ~N 延时）
    Segments(Vec<crate::config::Segment>),
    Resize(u16, u16),
    /// 运行时切换解码编码（立即生效于当前连接）
    SetEncoding(CharEncoding),
    Close,
}

pub enum UiMsg {
    Bell,
    Note(String),
}

/// 长驻 tokio runtime 句柄别名。
pub type SpawnCtx = tokio::runtime::Handle;

/// UI 侧持有的会话句柄。可克隆。
#[derive(Clone)]
pub struct Conn {
    pub site: Site,
    pub term: Arc<Mutex<Term>>,
    pub status: Arc<Mutex<Status>>,
    tx: mpsc::UnboundedSender<Cmd>,
    rx: Arc<Mutex<mpsc::UnboundedReceiver<UiMsg>>>,
}

impl Conn {
    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .map(|s| s.clone())
            .unwrap_or(Status {
                phase: Phase::Failed,
                msg: "状态不可用".into(),
            })
    }
    /// 非阻塞取出全部待处理事件。
    pub fn poll(&self) -> Vec<UiMsg> {
        let mut out = Vec::new();
        if let Ok(mut rx) = self.rx.lock() {
            while let Ok(m) = rx.try_recv() {
                out.push(m);
            }
        }
        out
    }
}

/// 建立会话并派生后台任务。
pub fn spawn(
    rt: &SpawnCtx,
    site: Site,
    proxy: Option<Proxy>,
    scrollback: usize,
) -> Conn {
    let term = Arc::new(Mutex::new({
        let mut t = Term::new(site.cols, site.rows);
        t.set_scrollback_limit(scrollback);
        t.title = site.name.clone();
        t
    }));
    let status = Arc::new(Mutex::new(Status {
        phase: Phase::Connecting,
        msg: format!("连接 {}:{} …", site.host, site.port),
    }));
    let (tx, rx_cmd) = mpsc::unbounded_channel::<Cmd>();
    let (mtx, rx) = mpsc::unbounded_channel::<UiMsg>();

    let (term_w, status_w) = (term.clone(), status.clone());
    let (site_w, proxy_w) = (site.clone(), proxy);
    rt.spawn(async move {
        let r = run(
            site_w,
            proxy_w,
            term_w.clone(),
            status_w.clone(),
            rx_cmd,
            mtx.clone(),
        )
        .await;
        let (phase, msg) = match r {
            Ok(()) => (Phase::Closed, "连接已关闭".to_string()),
            Err(e) => {
                eprintln!("[ATerm] 会话结束: {e}");
                (Phase::Failed, format!("连接失败：{e}"))
            }
        };
        if let Ok(mut s) = status_w.lock() {
            s.phase = phase;
            s.msg = msg;
        }
        let _ = mtx.send(UiMsg::Note(String::new()));
    });

    Conn {
        site,
        term,
        status,
        tx,
        rx: Arc::new(Mutex::new(rx)),
    }
}

/// 已建立的双向字节流：直连 TCP 或经 SOCKS5 代理的隧道。
pub enum Stream {
    Tcp(tokio::net::TcpStream),
    Socks(tokio_socks::tcp::Socks5Stream<tokio::net::TcpStream>),
}

impl tokio::io::AsyncRead for Stream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf,
    ) -> std::task::Poll<io::Result<()>> {
        use std::pin::Pin;
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Socks(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for Stream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        use std::pin::Pin;
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Socks(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        use std::pin::Pin;
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_flush(cx),
            Stream::Socks(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        use std::pin::Pin;
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Socks(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// 打开到目标主机的字节流，必要时先经 SOCKS5 代理建隧道。
async fn open_stream(host: &str, port: u16, proxy: &Option<Proxy>) -> io::Result<Stream> {
    if let Some(px) = proxy {
        if !px.host.trim().is_empty() && px.port > 0 {
            use tokio_socks::tcp::Socks5Stream;
            // 域名交给代理解析（SOCKS5 DOMAINNAME），让代理按其规则决定出口与 DNS。
            // 这样代理的分流规则才能按域名生效；本地先解析成 IP 会绕过域名规则。
            let r = if px.auth {
                Socks5Stream::connect_with_password(
                    (px.host.as_str(), px.port),
                    (host, port),
                    px.user.as_str(),
                    px.pass.as_str(),
                )
                .await
            } else {
                Socks5Stream::connect((px.host.as_str(), px.port), (host, port)).await
            };
            return match r {
                Ok(s) => Ok(Stream::Socks(s)),
                Err(e) => Err(io::Error::other(format!(
                    "SOCKS5 代理 {}:{} 连接 {}:{} 失败：{e}",
                    px.host, px.port, host, port
                ))),
            };
        }
    }
    let sa = tokio::net::TcpStream::connect((host, port))
        .await
        .map_err(|e| io::Error::other(format!("无法连接 {host}:{port}：{e}")))?;
    let _ = sa.set_nodelay(true);
    Ok(Stream::Tcp(sa))
}

fn set_status(status: &Arc<Mutex<Status>>, phase: Phase, msg: String) {
    if let Ok(mut s) = status.lock() {
        s.phase = phase;
        s.msg = msg;
    }
}

async fn run(
    mut site: Site,
    proxy: Option<Proxy>,
    term: Arc<Mutex<Term>>,
    status: Arc<Mutex<Status>>,
    mut rx_cmd: mpsc::UnboundedReceiver<Cmd>,
    mtx: mpsc::UnboundedSender<UiMsg>,
) -> io::Result<()> {
    let stream = open_stream(&site.host, site.port, &proxy).await?;

    // 经代理时：隧道虽建立，但代理出口到目标可能不通（表现为完全无数据）。
    // 起一个看门狗，若 12 秒内终端没收到任何字符，给出明确提示便于排查。
    if proxy.as_ref().map(|p| !p.host.trim().is_empty()).unwrap_or(false) {
        let t2 = term.clone();
        let s2 = status.clone();
        let m2 = mtx.clone();
        let hp = format!("{}:{}", site.host, site.port);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(12)).await;
            let silent = t2
                .lock()
                .map(|t| t.screen_text().trim().is_empty())
                .unwrap_or(false);
            let alive = s2
                .lock()
                .map(|s| s.phase != Phase::Failed && s.phase != Phase::Closed)
                .unwrap_or(false);
            if silent && alive {
                if let Ok(mut s) = s2.lock() {
                    s.msg = format!(
                        "代理隧道已建立，但 {hp} 无响应：请检查代理线路/分流规则能否访问该站"
                    );
                }
                let _ = m2.send(UiMsg::Note(format!(
                    "经代理连接 {hp} 已建立隧道但收不到数据，通常是代理出口无法访问该站点"
                )));
            }
        });
    }

    if site.proto == config::Protocol::Ssh {
        return ssh_run(site, stream, term, status, rx_cmd, mtx).await;
    }

    set_status(&status, Phase::Negotiating, "Telnet 协商…".into());
    let mut tn = Telnet::new(site.clone(), stream, term.clone(), mtx.clone());
    tn.boot().await?;
    set_status(
        &status,
        Phase::Connected,
        format!("已连接 {}:{}", site.host, site.port),
    );

    let mut inbuf = vec![0u8; 16384];
    let mut al = AutoLogin::new(&site);
    // 连接后命令：协商完成后几乎立刻发送（早于自动/手动账号输入）。
    // 未设置则不发任何内容（与 fterm 一致）；进站画面的保留由终端引擎的
    // Firebird 兼容清屏/滚动语义保证，无需额外空行。
    let mut pc_segs = if site.autologin.post_connect.is_empty() {
        Vec::new()
    } else {
        encode_segments(&site.autologin.post_connect, site.encoding)
    };
    let mut pc_sent = pc_segs.is_empty();
    let mut pc_delay = tokio::time::interval(Duration::from_millis(150));
    pc_delay.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    pc_delay.reset();
    let mut keep = tokio::time::interval(Duration::from_secs(
        site.keepalive_sec.max(15) as u64,
    ));
    keep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    keep.reset();
    // 诊断：录制原始字节流
    let mut rec: Option<std::fs::File> = std::env::var("ATERM_REC").ok().and_then(|p| {
        std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
    });

    loop {
        tokio::select! {
            r = tn.sock.read(&mut inbuf) => {
                let n = r?;
                if n == 0 {
                    return Ok(());
                }
                if let Some(f) = rec.as_mut() {
                    use std::io::Write;
                    let _ = f.write_all(&inbuf[..n]);
                }
                tn.on_data(&inbuf[..n]).await?;
                al.tick(&term, &mut tn).await;
            }
            c = rx_cmd.recv() => {
                match c {
                    None => return Ok(()),
                    Some(Cmd::Close) => return Ok(()),
                    Some(Cmd::SetEncoding(enc)) => {
                        site.encoding = enc;
                        tn.set_encoding(enc);
                    }
                    Some(Cmd::Resize(cc, rr)) => tn.resize(cc, rr).await?,
                    Some(Cmd::Bytes(b)) => {
                        if !b.is_empty() {
                            al.typing();
                            tn.write_bytes(&b).await?;
                        }
                    }
                    Some(Cmd::Segments(segs)) => {
                        al.typing();
                        for s in segs {
                            match s {
                                crate::config::Segment::Bytes(b) => {
                                    if !b.is_empty() { tn.write_bytes(&b).await?; }
                                }
                                crate::config::Segment::Delay(ms) => {
                                    tokio::time::sleep(Duration::from_millis(ms)).await;
                                }
                            }
                        }
                    }
                }
            }
            _ = pc_delay.tick(), if !pc_sent => {
                // 等 Telnet 协商完成再发，避免与 IAC 交叉
                if tn.phase != TelPhase::Negotiating {
                    pc_sent = true;
                    for s in std::mem::take(&mut pc_segs) {
                        match s {
                            config::Segment::Bytes(b) => {
                                if !b.is_empty() { tn.write_bytes(&b).await?; }
                            }
                            config::Segment::Delay(ms) => {
                                tokio::time::sleep(Duration::from_millis(ms)).await;
                            }
                        }
                    }
                }
            }
            _ = keep.tick(), if site.keepalive_sec > 0 => {
                if al.idle_long_enough() {
                    let b = encode_macro(&site.keepalive_text, site.encoding);
                    if !b.is_empty() {
                        tn.write_bytes(&b).await?;
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------ 自动登录

/// 按屏幕尾部文本匹配提示符，顺序发送账号/密码，最后发送登录后命令（可含延时）。
struct AutoLogin {
    enabled: bool,
    steps: Vec<(String, Vec<u8>)>,
    idx: usize,
    last_input: std::time::Instant,
    /// 登录后命令分段（账号密码发完后执行）
    after: Vec<crate::config::Segment>,
    after_done: bool,
}

fn plain(s: &str, enc: CharEncoding) -> Vec<u8> {
    let (bytes, _, _) = codec_for(enc).encode(s);
    bytes.into_owned()
}

impl AutoLogin {
    fn new(site: &Site) -> AutoLogin {
        let mut steps = Vec::new();
        let mut after = Vec::new();
        if site.autologin.enabled {
            steps.push((
                site.autologin.user_prompt.clone(),
                plain(&site.autologin.user, site.encoding),
            ));
            steps.push((
                site.autologin.pass_prompt.clone(),
                plain(&site.autologin.pass, site.encoding),
            ));
            if !site.autologin.after.is_empty() {
                after = encode_segments(&site.autologin.after, site.encoding);
            }
        }
        AutoLogin {
            enabled: site.autologin.enabled
                && (steps.iter().any(|(_, b)| !b.is_empty()) || !after.is_empty()),
            steps,
            idx: 0,
            last_input: std::time::Instant::now(),
            after_done: after.is_empty(),
            after,
        }
    }

    fn typing(&mut self) {
        self.last_input = std::time::Instant::now();
    }

    fn idle_long_enough(&self) -> bool {
        self.last_input.elapsed() > Duration::from_secs(10)
    }

    async fn tick<W>(&mut self, term: &Arc<Mutex<Term>>, tn: &mut Telnet<W>)
    where
        W: AsyncRead + AsyncWrite + Unpin + Send,
    {
        if !self.enabled {
            return;
        }
        if self.idx >= self.steps.len() {
            // 账号密码已发完 → 执行登录后命令（含 ~N 延时）
            if !self.after_done {
                self.after_done = true;
                for s in std::mem::take(&mut self.after) {
                    match s {
                        crate::config::Segment::Bytes(b) => {
                            if !b.is_empty() {
                                let _ = tn.write_bytes(&b).await;
                            }
                        }
                        crate::config::Segment::Delay(ms) => {
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                        }
                    }
                }
                self.last_input = std::time::Instant::now();
            }
            return;
        }
        // 等协商完成再发凭据，避免与 IAC 交叉
        if self.idx == 0 && tn.phase == TelPhase::Negotiating {
            return;
        }
        let tail = term
            .lock()
            .map(|t| t.tail_text(4).to_ascii_lowercase())
            .unwrap_or_default();
        let (needle, payload) = self.steps[self.idx].clone();
        let hit = needle.is_empty()
            || tail.contains(&needle.to_ascii_lowercase())
            || tn.since_idle() > Duration::from_secs(4);
        if hit {
            let mut p = payload.clone();
            if !p.is_empty() {
                p.push(0x0d);
                let _ = tn.write_bytes(&p).await;
            }
            self.idx += 1;
            self.last_input = std::time::Instant::now();
        }
    }
}

// ------------------------------------------------------------ Telnet

#[derive(PartialEq, Clone, Copy)]
enum TelPhase {
    Negotiating,
    Done,
}

/// 通用 Telnet 会话（socket 泛型便于测试与替换）。
pub struct Telnet<W> {
    sock: W,
    site: Site,
    term: Arc<Mutex<Term>>,
    mtx: mpsc::UnboundedSender<UiMsg>,
    /// 已答复过的 (对端命令, 选项)，防止协商来回抖动
    answered: std::collections::HashSet<(u8, u8)>,
    ttype_sent: bool,
    phase: TelPhase,
    started: std::time::Instant,
    dec: ChunkDecoder,
}

impl<W: AsyncRead + AsyncWrite + Unpin + Send> Telnet<W> {
    fn new(
        site: Site,
        sock: W,
        term: Arc<Mutex<Term>>,
        mtx: mpsc::UnboundedSender<UiMsg>,
    ) -> Telnet<W> {
        let dec = ChunkDecoder::new(site.encoding);
        Telnet {
            sock,
            ttype_sent: false,
            site,
            term,
            mtx,
            answered: Default::default(),
            phase: TelPhase::Negotiating,
            started: std::time::Instant::now(),
            dec,
        }
    }

    fn since_idle(&self) -> Duration {
        self.started.elapsed()
    }

    fn set_encoding(&mut self, enc: CharEncoding) {
        self.site.encoding = enc;
        self.dec = ChunkDecoder::new(enc);
    }

    async fn write_bytes(&mut self, b: &[u8]) -> io::Result<()> {
        self.sock.write_all(b).await?;
        self.sock.flush().await
    }

    async fn iac(&mut self, cmd: u8, o: u8) -> io::Result<()> {
        self.sock.write_all(&[255, cmd, o]).await
    }

    async fn send_sub(&mut self, payload: &[u8]) -> io::Result<()> {
        let mut m = vec![255, 250];
        m.extend_from_slice(payload);
        m.extend_from_slice(&[255, 240]);
        self.sock.write_all(&m).await
    }

    /// 连接初始化：只被动应答服务器的协商请求（handle_iac/handle_sub），
    /// 不主动发 DO NAWS / DO TTYPE。实测主动 DO 会使 Firebird 类 BBS
    /// （日月光华）发出"清屏+空行"异常序列甚至直接断连；被动应答时进站
    /// 画面干净、光标自然停在账号输入处。窗口尺寸在服务器 DO NAWS 时或
    /// 窗口变化时上报。
    async fn boot(&mut self) -> io::Result<()> {
        self.sock.flush().await
    }

    /// 喂入收到的字节：先剥离 IAC 控制序列，余下文本增量解码后送入终端引擎。
    async fn on_data(&mut self, data: &[u8]) -> io::Result<()> {
        let mut clean: Vec<u8> = Vec::with_capacity(data.len());
        let mut i = 0usize;
        while i < data.len() {
            if data[i] != 255 {
                clean.push(data[i]);
                i += 1;
                continue;
            }
            if i + 1 >= data.len() {
                break; // 跨包的 IAC，丢弃这半个（极少见）
            }
            match data[i + 1] {
                255 => {
                    clean.push(255);
                    i += 2;
                }
                250 => {
                    let mut j = i + 2;
                    let mut sub = Vec::new();
                    let mut closed = false;
                    while j + 1 < data.len() {
                        if data[j] == 255 && data[j + 1] == 240 {
                            closed = true;
                            break;
                        }
                        sub.push(data[j]);
                        j += 1;
                    }
                    if closed {
                        self.handle_sub(&sub).await?;
                        i = j + 2;
                    } else {
                        i = data.len();
                    }
                }
                251 | 252 | 253 | 254 => {
                    if i + 2 < data.len() {
                        let cmd = data[i + 1];
                        let o = data[i + 2];
                        self.handle_iac(cmd, o).await?;
                        i += 3;
                    } else {
                        i = data.len();
                    }
                }
                242 => {
                    i += 3;
                } // 带数据的命令，保守跳过
                _ => {
                    i += 2;
                }
            }
        }

        if self.phase == TelPhase::Negotiating {
            self.phase = TelPhase::Done;
        }

        if !clean.is_empty() {
            let text = self.dec.feed(&clean);
            if !text.is_empty() {
                let evs = {
                    let mut t = self.term.lock().map_err(|_| io::Error::other("lock"))?;
                    t.feed_chars(&text)
                };
                for e in evs {
                    match e {
                        crate::term::Event::Bell => {
                            if self.site.ring_bell {
                                let _ = self.mtx.send(UiMsg::Bell);
                            }
                        }
                        crate::term::Event::Send(b) => {
                            self.write_bytes(&b).await?;
                        }
                        crate::term::Event::Title => {}
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_iac(&mut self, cmd: u8, o: u8) -> io::Result<()> {
        let key = (cmd, o);
        if self.answered.contains(&key) {
            return Ok(());
        }
        self.answered.insert(key);
        match cmd {
            253 => {
                // DO：对端要求我们启用某选项
                let yes = matches!(o, opt::SGA | opt::TTYPE | opt::NAWS | opt::NEW_ENVIRON);
                self.iac(if yes { 251 } else { 252 }, o).await?;
                if yes && o == opt::TTYPE {
                    self.send_ttype().await?;
                }
                if yes && o == opt::NAWS {
                    self.send_naws().await?;
                }
            }
            254 => self.iac(252, o).await?,
            251 => {
                // WILL：对端声明启用某选项
                let yes = matches!(o, opt::ECHO | opt::SGA | opt::NEW_ENVIRON);
                self.iac(if yes { 253 } else { 254 }, o).await?;
            }
            252 => self.iac(254, o).await?,
            _ => {}
        }
        self.sock.flush().await
    }

    async fn handle_sub(&mut self, sub: &[u8]) -> io::Result<()> {
        if sub.is_empty() {
            return Ok(());
        }
        match sub[0] {
            opt::TTYPE => {
                // SEND=1 要求我们上报终端类型
                if sub.get(1) == Some(&1) && !self.ttype_sent {
                    self.send_ttype().await?;
                }
            }
            opt::STATUS => {
                self.send_sub(&[opt::SGA, 251]).await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn send_ttype(&mut self) -> io::Result<()> {
        self.ttype_sent = true;
        let mut p = vec![opt::TTYPE, 0]; // 0 = IS
        p.extend_from_slice(self.site.termtype.as_bytes());
        self.send_sub(&p).await
    }

    async fn send_naws(&mut self) -> io::Result<()> {
        let (c, r) = {
            let t = self.term.lock().map_err(|_| io::Error::other("lock"))?;
            (t.cols, t.rows)
        };
        let p = vec![
            opt::NAWS,
            (c >> 8) as u8,
            (c & 0xff) as u8,
            (r >> 8) as u8,
            (r & 0xff) as u8,
        ];
        self.send_sub(&p).await
    }

    async fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        {
            let mut t = self.term.lock().map_err(|_| io::Error::other("lock"))?;
            if t.cols == cols && t.rows == rows {
                return Ok(());
            }
            t.resize(cols, rows);
        }
        self.send_naws().await
    }
}

// ------------------------------------------------------------ 编码

/// 把一段字节按指定编码完整解码。
///
/// BBS 输出以行为单位、且 16KB 缓冲足够容纳整行，因此这里不做跨包续接：
/// 若尾部出现半个多字节字符，退化为 GBK/Big5 的容错解码（encoding_rs 会用
/// U+FFFD 替换，下一行重新同步）。
pub fn decode_gbk(enc: &'static encoding_rs::Encoding, bytes: &[u8]) -> String {
    let (cow, _, _) = enc.decode(bytes);
    cow.into_owned()
}

/// 供测试使用的增量解码器。
pub struct ChunkDecoder {
    dec: encoding_rs::Decoder,
    carry: Vec<u8>,
}

impl ChunkDecoder {
    pub fn new(enc: CharEncoding) -> ChunkDecoder {
        ChunkDecoder {
            dec: codec_for(enc).new_decoder(),
            carry: Vec::new(),
        }
    }

    /// 增量解码：内部保留半个多字节字符，返回已可安全输出的文本。
    pub fn feed(&mut self, bytes: &[u8]) -> String {
        let mut src = std::mem::take(&mut self.carry);
        src.extend_from_slice(bytes);
        let mut buf = vec![0u8; src.len() * 3 + 16];
        let mut pos = 0usize;
        let mut used = 0usize;
        loop {
            let (res, r, w, _had_err) =
                self.dec.decode_to_utf8(&src[pos..], &mut buf[used..], false);
            pos += r;
            used += w;
            match res {
                encoding_rs::CoderResult::InputEmpty => break,
                encoding_rs::CoderResult::OutputFull => {
                    if r == 0 {
                        break;
                    }
                    buf.resize(buf.len() * 2, 0);
                }
            }
            if pos >= src.len() {
                break;
            }
        }
        if pos < src.len() {
            self.carry = src[pos..].to_vec();
        }
        buf.truncate(used);
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// 流结束时强制收尾。
    pub fn finish(&mut self) -> String {
        let src = std::mem::take(&mut self.carry);
        let mut buf = vec![0u8; src.len() * 3 + 16];
        let (_res, _r, w, _e) = self.dec.decode_to_utf8(&src, &mut buf, true);
        buf.truncate(w);
        String::from_utf8_lossy(&buf).into_owned()
    }
}

// ------------------------------------------------------------ SSH

struct SshHandler {
    term: Arc<Mutex<Term>>,
}

impl russh::client::Handler for SshHandler {
    type Error = russh::Error;

    /// TOFU：首见主机密钥直接接受。
    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        if !banner.is_empty()
            && let Ok(mut t) = self.term.lock()
        {
            let _ = t.feed_chars(&format!("{banner}\r\n"));
        }
        Ok(())
    }
}

async fn ssh_run(
    mut site: Site,
    stream: Stream,
    term: Arc<Mutex<Term>>,
    status: Arc<Mutex<Status>>,
    mut rx_cmd: mpsc::UnboundedReceiver<Cmd>,
    mtx: mpsc::UnboundedSender<UiMsg>,
) -> io::Result<()> {
    set_status(&status, Phase::Negotiating, "SSH 密钥交换…".into());
    let config = Arc::new(russh::client::Config {
        inactivity_timeout: Some(Duration::from_secs(120)),
        ..Default::default()
    });
    let handler = SshHandler { term: term.clone() };
    let mut handle = russh::client::connect_stream(config, stream, handler)
        .await
        .map_err(|e| io::Error::other(format!("SSH 握手失败：{e}")))?;

    // 认证：用户名/密码取自站点的自动登录配置（SSH 必须提供）
    let mut user = site.autologin.user.clone();
    let pass = site.autologin.pass.clone();
    // 允许 host 写成 user@host 形式
    if user.is_empty()
        && let Some((u, _)) = site.host.split_once('@')
    {
        user = u.to_string();
    }
    if user.is_empty() {
        return Err(io::Error::other(
            "SSH 站点缺少用户名：请在站点设置的自动登录里填写，或把主机写成 user@host",
        ));
    }
    set_status(&status, Phase::Negotiating, format!("SSH 认证 {user} …"));

    let ok = match handle.authenticate_password(user.clone(), pass.clone()).await {
        Ok(a) => a.success(),
        Err(e) => return Err(io::Error::other(format!("SSH 认证出错：{e}"))),
    };
    let mut authed = ok;
    if !authed {
        // 退路：keyboard-interactive（常见于 BBS 的"请输入密码"式提示）
        authed = kb_interactive(&mut handle, &user, &pass).await?;
    }
    if !authed {
        return Err(io::Error::other("SSH 认证失败：用户名或密码不正确"));
    }

    let (cols, rows) = {
        let t = term.lock().map_err(|_| io::Error::other("lock"))?;
        (t.cols, t.rows)
    };
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| io::Error::other(format!("SSH 打开通道失败：{e}")))?;
    let _keep = handle; // 会话需保活至函数结束
    let _ = channel
        .request_pty(
            false,
            site.termtype.as_str(),
            cols as u32,
            rows as u32,
            (cols * 8) as u32,
            (rows * 16) as u32,
            &[],
        )
        .await;
    let _ = channel.request_shell(false).await;
    let (mut rd, wr) = channel.split();
    set_status(
        &status,
        Phase::Connected,
        format!("已连接 SSH {}:{}", site.host, site.port),
    );
    // 连接后命令：shell 就绪即发，早于任何账号输入
    if !site.autologin.post_connect.is_empty() {
        for s in encode_segments(&site.autologin.post_connect, site.encoding) {
            match s {
                config::Segment::Bytes(b) => {
                    if !b.is_empty() {
                        let _ = wr.data_bytes(b).await;
                    }
                }
                config::Segment::Delay(ms) => {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
            }
        }
    }

    let mut dec = ChunkDecoder::new(site.encoding);
    let mut keep = tokio::time::interval(Duration::from_secs(
        site.keepalive_sec.max(15) as u64,
    ));
    keep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    keep.reset();
    let mut last_user = std::time::Instant::now();
    // 登录后命令：等 shell 就绪后发送一次（支持 ~N 延时）
    let mut after_segs = if site.autologin.after.is_empty() {
        Vec::new()
    } else {
        encode_segments(&site.autologin.after, site.encoding)
    };
    let mut after_sent = after_segs.is_empty();
    let mut after_delay = tokio::time::interval(Duration::from_millis(1500));
    after_delay.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    after_delay.reset();

    loop {
        tokio::select! {
            maybe = rd.wait() => {
                match maybe {
                    None => return Ok(()),
                    Some(russh::ChannelMsg::Data { ref data }) => {
                        let text = dec.feed(data);
                        push_text(&term, &text, &site, &mtx);
                    }
                    Some(russh::ChannelMsg::ExtendedData { ref data, .. }) => {
                        let text = dec.feed(data);
                        push_text(&term, &text, &site, &mtx);
                    }
                    Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                        let _ = mtx.send(UiMsg::Note(format!("远端进程退出（{exit_status}）")));
                        return Ok(());
                    }
                    Some(_) => {}
                }
            }
            c = rx_cmd.recv() => {
                match c {
                    None => return Ok(()),
                    Some(Cmd::Close) => { let _ = wr.close().await; return Ok(()); }
                    Some(Cmd::SetEncoding(enc)) => {
                        site.encoding = enc;
                        dec = ChunkDecoder::new(enc);
                    }
                    Some(Cmd::Resize(cc, rr)) => {
                        {
                            let mut t = term.lock().map_err(|_| io::Error::other("lock"))?;
                            if t.cols == cc && t.rows == rr { continue; }
                            t.resize(cc, rr);
                        }
                        let _ = wr.window_change(cc as u32, rr as u32, (cc * 8) as u32, (rr * 16) as u32).await;
                    }
                    Some(Cmd::Bytes(b)) => {
                        if b.is_empty() { continue; }
                        last_user = std::time::Instant::now();
                        if wr.data_bytes(b).await.is_err() { return Ok(()); }
                    }
                    Some(Cmd::Segments(segs)) => {
                        last_user = std::time::Instant::now();
                        for s in segs {
                            match s {
                                config::Segment::Bytes(b) => {
                                    if !b.is_empty() && wr.data_bytes(b).await.is_err() { return Ok(()); }
                                }
                                config::Segment::Delay(ms) => {
                                    tokio::time::sleep(Duration::from_millis(ms)).await;
                                }
                            }
                        }
                    }
                }
            }
            _ = after_delay.tick(), if !after_sent => {
                after_sent = true;
                for s in std::mem::take(&mut after_segs) {
                    match s {
                        config::Segment::Bytes(b) => {
                            if !b.is_empty() {
                                last_user = std::time::Instant::now();
                                let _ = wr.data_bytes(b).await;
                            }
                        }
                        config::Segment::Delay(ms) => {
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                        }
                    }
                }
            }
            _ = keep.tick(), if site.keepalive_sec > 0 => {
                if last_user.elapsed() > Duration::from_secs(10) {
                    let b = encode_macro(&site.keepalive_text, site.encoding);
                    if !b.is_empty() { let _ = wr.data_bytes(b).await; }
                }
            }
        }
    }
}

/// 把宏文本解析为分段，并把各字节段按站点编码转换（延时原样保留）。
fn encode_segments(src: &str, enc: CharEncoding) -> Vec<config::Segment> {
    config::parse_segments(src)
        .into_iter()
        .map(|s| match s {
            config::Segment::Bytes(b) => {
                // 复用 encode_macro 的逐段编码逻辑：先还原记法再编码
                let txt = config::bytes_to_keyspec(&b);
                config::Segment::Bytes(encode_macro(&txt, enc))
            }
            d => d,
        })
        .collect()
}

fn push_text(
    term: &Arc<Mutex<Term>>,
    text: &str,
    site: &Site,
    mtx: &mpsc::UnboundedSender<UiMsg>,
) {
    if text.is_empty() {
        return;
    }
    let mut t = match term.lock() {
        Ok(t) => t,
        Err(_) => return,
    };
    let evs = t.feed_chars(text);
    drop(t);
    for e in evs {
        if let crate::term::Event::Bell = e
            && site.ring_bell
        {
            let _ = mtx.send(UiMsg::Bell);
        }
    }
}

/// keyboard-interactive：把服务器提出的每个"密码"类问题都用站点密码回答。
async fn kb_interactive(
    handle: &mut russh::client::Handle<SshHandler>,
    user: &str,
    pass: &str,
) -> io::Result<bool> {
    let mut resp = match handle
        .authenticate_keyboard_interactive_start(user, Option::<String>::None)
        .await
    {
        Ok(r) => r,
        Err(_) => return Ok(false),
    };
    for _ in 0..8 {
        match resp {
            russh::client::KeyboardInteractiveAuthResponse::Success => return Ok(true),
            russh::client::KeyboardInteractiveAuthResponse::Failure { .. } => {
                return Ok(false);
            }
            russh::client::KeyboardInteractiveAuthResponse::InfoRequest { ref prompts, .. } => {
                let answers: Vec<String> = prompts
                    .iter()
                    .map(|p: &russh::client::Prompt| {
                        let l = p.prompt.to_lowercase();
                        if p.echo || l.contains("password") || l.contains("密码") {
                            pass.to_string()
                        } else {
                            String::new()
                        }
                    })
                    .collect();
                resp = match handle.authenticate_keyboard_interactive_respond(answers).await {
                    Ok(r) => r,
                    Err(e) => return Err(io::Error::other(format!("键盘交互失败：{e}"))),
                };
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_labels_and_alive() {
        assert_eq!(Phase::Connected.label(), "已连接");
        assert!(Phase::Connecting.alive());
        assert!(!Phase::Closed.alive());
        assert!(!Phase::Failed.alive());
    }

    #[test]
    fn decode_gbk_full_buffer() {
        let (gbk, _, _) = codec_for(CharEncoding::Gbk).encode("中文测试");
        assert_eq!(decode_gbk(codec_for(CharEncoding::Gbk), &gbk), "中文测试");
    }

    #[test]
    fn chunk_decoder_reassembles_split_char() {
        let (gbk, _, _) = codec_for(CharEncoding::Gbk).encode("中文AB");
        let mut d = ChunkDecoder::new(CharEncoding::Gbk);
        let a = d.feed(&gbk[..3]); // 切在半个汉字之后
        let b = d.feed(&gbk[3..]);
        assert_eq!(format!("{a}{b}"), "中文AB");
    }

    #[test]
    fn chunk_decoder_utf8_split() {
        let raw = "水木".as_bytes();
        let mut d = ChunkDecoder::new(CharEncoding::Utf8);
        let a = d.feed(&raw[..2]);
        let b = d.feed(&raw[2..]);
        assert_eq!(format!("{a}{b}"), "水木");
    }
}

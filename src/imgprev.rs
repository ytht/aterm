//! 悬停 http/https 图片链接时的预览：后台下载 → 解码 → egui 纹理。
//! 只处理 http/https，带大小与超时上限；失败静默（窗口内显示失败原因）。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 单张图片下载上限（字节）。
const MAX_BYTES: usize = 8 * 1024 * 1024;
/// 预览纹理最长边（超出按比例缩小）。
const MAX_SIDE: u32 = 1920;

pub enum ImgState {
    Loading,
    Ready(egui::TextureHandle),
    Failed(String),
}

pub type Slot = Arc<Mutex<Option<ImgState>>>;

/// 常见图片扩展名。
const IMG_EXTS: [&str; 9] =
    [".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".ico", ".tif", ".tiff"];

/// 是否识别为图片链接：必须带 http/https 协议，且 URL 里出现常见图片文件名。
/// 文件名可以在路径里（/p/i.png），也可以在查询串里（att.php?xxx/yyy.jpg，
/// BBS 附件常见形态）；扩展名后必须紧跟 URL 分隔符或结尾，避免 .png.html 误判。
/// 无协议（含纯 IP、裸域名）一律不算图片。
pub fn image_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
    else {
        return false;
    };
    // 只看主机之后的部分，避免域名里的字母凑成扩展名
    let Some(i) = rest.find('/') else { return false };
    let tail = &rest[i..];
    for ext in IMG_EXTS {
        let mut from = 0usize;
        while let Some(p) = tail[from..].find(ext) {
            let end = from + p + ext.len();
            let next_ok = tail
                .as_bytes()
                .get(end)
                .map(|b| matches!(b, b'/' | b'?' | b'#' | b'&' | b'=' | b';'))
                .unwrap_or(true);
            if next_ok {
                return true;
            }
            from = from + p + 1;
        }
    }
    false
}



/// 后台抓取并解码，结果写入共享槽（纹理在 egui 线程创建）。
pub fn spawn_fetch(rt: &crate::net::SpawnCtx, ctx: egui::Context, url: String, slot: Slot) {
    rt.spawn(async move {
        let res = tokio::task::spawn_blocking(move || fetch_rgba(&url)).await;
        let state = match res {
            Ok(Ok((w, h, rgba))) => {
                let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                ImgState::Ready(ctx.load_texture("url_img_preview", img, egui::TextureOptions::LINEAR))
            }
            Ok(Err(e)) => ImgState::Failed(e),
            Err(_) => ImgState::Failed("解码任务失败".into()),
        };
        if let Ok(mut g) = slot.lock() {
            *g = Some(state);
        }
        ctx.request_repaint();
    });
}

/// 下载公开资源（供 IP 数据库等复用）；超过 limit 字节则失败。
pub fn download(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let (_, bytes) = http_get(url, limit)?;
    if bytes.len() > limit {
        return Err("内容超过大小上限".into());
    }
    Ok(bytes)
}

/// 下载并解码为 RGBA（含缩放）。
fn fetch_rgba(url: &str) -> Result<(u32, u32, Vec<u8>), String> {
    let (ctype, bytes) = http_get(url, MAX_BYTES)?;
    let ct = ctype.to_ascii_lowercase();
    if !ct.is_empty() && !ct.starts_with("image/") && !ct.contains("octet-stream") {
        return Err(format!("不是图片：{ctype}"));
    }
    let img = image::load_from_memory(&bytes).map_err(|_| "不是可显示的图片".to_string())?;
    let img = if img.width().max(img.height()) > MAX_SIDE {
        let scale = MAX_SIDE as f32 / img.width().max(img.height()) as f32;
        let w = ((img.width() as f32 * scale) as u32).max(1);
        let h = ((img.height() as f32 * scale) as u32).max(1);
        img.resize_exact(w, h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((w, h, rgba.into_raw()))
}

/// 拆分 http/https URL 为 (host, port, 请求路径)。
fn split_url(url: &str) -> Option<(String, u16, String, bool)> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return None;
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()?),
        None => (authority.to_string(), if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port, path.to_string(), tls))
}

/// 明文 HTTP GET（https 交给系统 WinHTTP，复用系统证书校验）。
/// 返回 (Content-Type, 正文)；非图片扩展名的链接只探前 256KB。
fn http_get(url: &str, limit: usize) -> Result<(String, Vec<u8>), String> {
    let (host, port, path, tls) = split_url(url).ok_or("URL 格式不支持")?;
    if tls {
        return winhttp_get(url, limit);
    }
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("域名解析失败：{e}"))?
        .next()
        .ok_or("域名解析失败")?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(8))
        .map_err(|e| format!("连接失败：{e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    s.set_write_timeout(Some(Duration::from_secs(10))).ok();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: ATerm\r\nAccept: image/*\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).map_err(|e| format!("请求失败：{e}"))?;
    let mut raw = Vec::new();
    s.take(limit as u64 + 64 * 1024)
        .read_to_end(&mut raw)
        .map_err(|e| format!("读取失败：{e}"))?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("响应格式异常")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(format!("服务器返回 {status}"));
    }
    let ctype = header_value(&head, "content-type");
    let body = &raw[split + 4..];
    let chunked = head.to_ascii_lowercase().contains("transfer-encoding: chunked");
    let out = if chunked { dechunk(body)? } else { body.to_vec() };
    if out.len() > MAX_BYTES {
        return Err("图片超过 8MB，已放弃".into());
    }
    Ok((ctype, out))
}

/// 取响应头字段值（大小写不敏感）。
fn header_value(head: &str, name: &str) -> String {
    head.lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case(name) {
                Some(v.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn dechunk(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < body.len() {
        let Some(end) = body[i..].windows(2).position(|w| w == b"\r\n") else {
            return Err("分块响应损坏".into());
        };
        let line = String::from_utf8_lossy(&body[i..i + end]);
        let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|_| "分块长度非法")?;
        i += end + 2;
        if size == 0 {
            break;
        }
        if i + size > body.len() {
            return Err("分块响应截断".into());
        }
        out.extend_from_slice(&body[i..i + size]);
        i += size + 2;
        if out.len() > MAX_BYTES {
            return Err("图片超过 8MB，已放弃".into());
        }
    }
    Ok(out)
}

/// 用系统 WinHTTP 抓取 https（证书校验由系统完成）。
fn winhttp_get(url: &str, limit: usize) -> Result<(String, Vec<u8>), String> {
    #[link(name = "winhttp")]
    unsafe extern "system" {
        fn WinHttpOpen(
            agent: *const u16,
            access_type: u32,
            proxy: *const u16,
            proxy_bypass: *const u16,
            flags: u32,
        ) -> *mut std::ffi::c_void;
        fn WinHttpSetTimeouts(h: *mut std::ffi::c_void, resolve: i32, connect: i32, send: i32, recv: i32) -> i32;
        fn WinHttpConnect(h: *mut std::ffi::c_void, server: *const u16, port: u16, reserved: u32) -> *mut std::ffi::c_void;
        fn WinHttpOpenRequest(
            h: *mut std::ffi::c_void,
            verb: *const u16,
            object: *const u16,
            version: *const u16,
            referrer: *const u16,
            accept: *const *const u16,
            flags: u32,
        ) -> *mut std::ffi::c_void;
        fn WinHttpSendRequest(
            h: *mut std::ffi::c_void,
            headers: *const u16,
            headers_len: u32,
            optional: *mut std::ffi::c_void,
            optional_len: u32,
            total_len: u32,
            context: usize,
        ) -> i32;
        fn WinHttpReceiveResponse(h: *mut std::ffi::c_void, reserved: *mut std::ffi::c_void) -> i32;
        fn WinHttpQueryDataAvailable(h: *mut std::ffi::c_void, avail: *mut u32) -> i32;
        fn WinHttpReadData(h: *mut std::ffi::c_void, buf: *mut u8, to_read: u32, read: *mut u32) -> i32;
        fn WinHttpQueryHeaders(
            h: *mut std::ffi::c_void,
            info_level: u32,
            name: *const u16,
            buffer: *mut u16,
            buffer_len: *mut u32,
            index: *mut u32,
        ) -> i32;
        fn WinHttpCloseHandle(h: *mut std::ffi::c_void) -> i32;
    }
    const WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
    const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
    const WINHTTP_QUERY_CONTENT_TYPE: u32 = 1;
    const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;


    let (host, port, path, _) = split_url(url).ok_or("URL 格式不支持")?;
    let w = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let agent = w("ATerm");
    let server = w(&host);
    let object = w(&path);
    let verb = w("GET");

    unsafe {
        let session = WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, std::ptr::null(), std::ptr::null(), 0);
        if session.is_null() {
            return Err("无法初始化系统 HTTP 组件".into());
        }
        WinHttpSetTimeouts(session, 8000, 8000, 10000, 10000);
        let conn = WinHttpConnect(session, server.as_ptr(), port, 0);
        if conn.is_null() {
            WinHttpCloseHandle(session);
            return Err("连接失败".into());
        }
        let req = WinHttpOpenRequest(conn, verb.as_ptr(), object.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null(), WINHTTP_FLAG_SECURE);
        if req.is_null() {
            WinHttpCloseHandle(conn);
            WinHttpCloseHandle(session);
            return Err("无法创建请求".into());
        }
        let ok = WinHttpSendRequest(req, std::ptr::null(), 0, std::ptr::null_mut(), 0, 0, 0) != 0
            && WinHttpReceiveResponse(req, std::ptr::null_mut()) != 0;
        let mut out = Vec::new();
        let mut err = String::new();
        let mut ctype = String::new();
        if !ok {
            err = "HTTPS 请求失败（证书或网络问题）".into();
        } else {
            let mut buf = [0u16; 256];
            let mut len = (buf.len() * 2) as u32;
            let mut idx = 0u32;
            if WinHttpQueryHeaders(req, WINHTTP_QUERY_CONTENT_TYPE, std::ptr::null(), buf.as_mut_ptr(), &mut len, &mut idx) != 0 {
                let n = (len as usize / 2).min(buf.len());
                ctype = String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_string();
            }
            let _ = WINHTTP_QUERY_FLAG_NUMBER;
            loop {
                let mut avail = 0u32;
                if WinHttpQueryDataAvailable(req, &mut avail) == 0 || avail == 0 {
                    break;
                }
                let want = avail.min(64 * 1024);
                let mut buf = vec![0u8; want as usize];
                let mut got = 0u32;
                if WinHttpReadData(req, buf.as_mut_ptr(), want, &mut got) == 0 {
                    err = "读取失败".into();
                    break;
                }
                if got == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..got as usize]);
                if out.len() > limit {
                    err = format!("内容超过 {}MB 上限", limit / 1024 / 1024);
                    break;
                }
            }
        }
        WinHttpCloseHandle(req);
        WinHttpCloseHandle(conn);
        WinHttpCloseHandle(session);
        if !err.is_empty() {
            return Err(err);
        }
        Ok((ctype, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_url_detection() {
        // 必须带 http/https 协议 + 常见图片文件名
        assert!(image_url("http://a.com/x.png"));
        assert!(image_url("https://a.com/x.JPG?v=2"));
        assert!(image_url("https://a.com/p/i.jpeg#frag"));
        // 无协议不算图片（纯 IP、裸域名都不弹窗）
        assert!(!image_url("a.com/x.png"));
        assert!(!image_url("8.8.8.8"));
        assert!(!image_url("1.2.3.*"));
        // 文件名在查询串里（BBS 附件）同样识别
        assert!(image_url(
            "https://www.newsmth.net/att.php?816YJ6VK14jwKa0QF9G/aAgAAS+QAAEkBAABBn5UR.jpg"
        ));
        assert!(image_url("https://a.com/att.php?f=x.png&w=100"));
        // 无图片文件名不算图片
        assert!(!image_url("https://a.com/img?id=9"));
        assert!(!image_url("https://a.com/"));
        assert!(!image_url("https://a.com/x.png.html"));
        // 其它协议不处理
        assert!(!image_url("ftp://a.com/x.png"));
        assert!(!image_url("telnet://a.com"));
    }

    #[test]
    fn url_split() {
        assert_eq!(
            split_url("https://a.com/p/i.png?x=1"),
            Some(("a.com".into(), 443, "/p/i.png?x=1".into(), true))
        );
        assert_eq!(
            split_url("http://a.com:8080"),
            Some(("a.com".into(), 8080, "/".into(), false))
        );
        assert!(split_url("ftp://a.com").is_none());
    }

    #[test]
    fn dechunk_basic() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap(), b"abcde");
        assert!(dechunk(b"zz\r\n").is_err());
    }
}

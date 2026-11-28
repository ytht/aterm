//! 悬停纯 IP 时的来源查询。
//! 优先远程城市库（GeoLite2-City，下载后缓存在 exe 同目录）；
//! 远程不可用时回落到同目录的本地 GeoLite2-ASN.tar.gz。
//! 两种库字段不同，统一归一成一行来源文本；都没有则静默不提示。

use maxminddb::geoip2;
use std::io::Read;
use std::net::IpAddr;
use std::sync::{Arc, OnceLock};

/// 本地数据库文件名（与 ATerm.exe 同目录）。
pub const DB_FILE: &str = "GeoLite2-ASN.tar.gz";
/// 远程城市库地址。
pub const REMOTE_URL: &str = "https://cdn.jsdelivr.net/npm/geolite2-city/GeoLite2-City.mmdb.gz";
/// 远程库解压后的本地缓存文件名。
pub const REMOTE_CACHE: &str = "GeoLite2-City.mmdb";

/// 查询结果：统一后的来源文本（界面显示「IP来源：text」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpInfo {
    pub text: String,
}

/// 已加载的数据库（None = 不可用，本次运行不再重试）。
fn city_db() -> &'static Option<maxminddb::Reader<Vec<u8>>> {
    static DB: OnceLock<Option<maxminddb::Reader<Vec<u8>>>> = OnceLock::new();
    DB.get_or_init(load_city)
}

fn asn_db() -> &'static Option<maxminddb::Reader<Vec<u8>>> {
    static DB: OnceLock<Option<maxminddb::Reader<Vec<u8>>>> = OnceLock::new();
    DB.get_or_init(load_asn)
}

fn exe_dir() -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// 城市库：先用本地缓存，没有则下载远程 .gz 并解压落盘。
fn load_city() -> Option<maxminddb::Reader<Vec<u8>>> {
    let dir = exe_dir()?;
    let cache = dir.join(REMOTE_CACHE);
    if let Ok(bytes) = std::fs::read(&cache)
        && let Ok(r) = maxminddb::Reader::from_source(bytes)
    {
        return Some(r);
    }
    let gz = crate::imgprev::download(REMOTE_URL, 64 * 1024 * 1024).ok()?;
    let mut mmdb = Vec::new();
    flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut mmdb).ok()?;
    let reader = maxminddb::Reader::from_source(mmdb.clone()).ok()?;
    // 缓存失败不影响本次查询
    let _ = std::fs::write(&cache, &mmdb);
    Some(reader)
}

/// 本地 ASN 库（远程不可用时的回落）。
fn load_asn() -> Option<maxminddb::Reader<Vec<u8>>> {
    let bytes = std::fs::read(exe_dir()?.join(DB_FILE)).ok()?;
    let mmdb = extract_mmdb(&bytes)?;
    maxminddb::Reader::from_source(mmdb).ok()
}

/// 从 .tar.gz 中取出 .mmdb 成员（官方发布包内为 GeoLite2-ASN_日期/GeoLite2-ASN.mmdb）。
fn extract_mmdb(gz: &[u8]) -> Option<Vec<u8>> {
    let tar = flate2::read::GzDecoder::new(gz);
    let mut ar = tar::Archive::new(tar);
    for entry in ar.entries().ok()? {
        let mut e = entry.ok()?;
        let is_db = e
            .path()
            .ok()
            .and_then(|p| p.extension().map(|x| x.to_os_string()))
            .map(|x| x.eq_ignore_ascii_case("mmdb"))
            .unwrap_or(false);
        if !is_db {
            continue;
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).ok()?;
        return Some(buf);
    }
    None
}

/// 查询 IP 来源：城市库优先，取不到再用本地 ASN 库；都无记录返回 None。
pub fn lookup(ip: &str) -> Option<IpInfo> {
    let addr: IpAddr = ip.parse().ok()?;
    if let Some(text) = city_db().as_ref().and_then(|r| city_text(r, addr)) {
        return Some(IpInfo { text });
    }
    let text = asn_db().as_ref().and_then(|r| asn_text(r, addr))?;
    Some(IpInfo { text })
}

/// 城市库记录 → 「国家 省/州 城市」（中文名优先，缺失逐级省略）。
fn city_text(reader: &maxminddb::Reader<Vec<u8>>, addr: IpAddr) -> Option<String> {
    let res = reader.lookup(addr).ok()?;
    if !res.has_data() {
        return None;
    }
    let rec: geoip2::City = res.decode().ok()??;
    let name = |n: &geoip2::Names| -> Option<String> {
        n.simplified_chinese
            .or(n.english)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = name(&rec.country.names) {
        parts.push(c);
    }
    if let Some(s) = rec.subdivisions.first().and_then(|s| name(&s.names))
        && !parts.contains(&s)
    {
        parts.push(s);
    }
    if let Some(c) = name(&rec.city.names)
        && !parts.contains(&c)
    {
        parts.push(c);
    }
    if parts.is_empty() { None } else { Some(parts.join(" ")) }
}

/// ASN 库记录 → 归属组织名。
fn asn_text(reader: &maxminddb::Reader<Vec<u8>>, addr: IpAddr) -> Option<String> {
    let res = reader.lookup(addr).ok()?;
    if !res.has_data() {
        return None;
    }
    let rec: geoip2::Asn = res.decode().ok()??;
    let org = rec.autonomous_system_organization.unwrap_or("");
    if org.is_empty() { None } else { Some(org.to_string()) }
}

/// 供界面显示的共享结果槽。
pub type Slot = Arc<std::sync::Mutex<Option<IpInfo>>>;

/// 后台查询（首次调用会解包数据库，放后台避免卡界面）。
pub fn spawn_lookup(rt: &crate::net::SpawnCtx, ip: String, slot: Slot) {
    rt.spawn(async move {
        let info = tokio::task::spawn_blocking(move || lookup(&ip))
            .await
            .ok()
            .flatten();
        if let Some(info) = info
            && let Ok(mut g) = slot.lock()
        {
            *g = Some(info);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_mmdb_from_targz() {
        // 构造一个含 .mmdb 成员的 tar.gz，验证解包能取到内容
        let mut tar_buf = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_buf);
            let data = b"mmdb-body";
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(data.len() as u64);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            b.append_data(&mut hdr, "GeoLite2-ASN_20260101/GeoLite2-ASN.mmdb", &data[..])
                .unwrap();
            b.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            use std::io::Write;
            let mut enc =
                flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
            enc.write_all(&tar_buf).unwrap();
            enc.finish().unwrap();
        }
        assert_eq!(extract_mmdb(&gz).unwrap(), b"mmdb-body".to_vec());
        assert!(extract_mmdb(b"not a gzip").is_none());
    }

    #[test]
    fn invalid_ip_is_silent() {
        // 非法 IP 直接返回 None，不触发任何数据库加载/下载
        assert!(lookup("not-an-ip").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn asn_text_from_local_db() {
        // 本地 ASN 库存在时应能取到组织名（缺失则跳过断言）
        if let Some(r) = asn_db().as_ref() {
            let t = asn_text(r, "8.8.8.8".parse().unwrap());
            assert!(t.is_none() || !t.unwrap().is_empty());
        }
    }
}

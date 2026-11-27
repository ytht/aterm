//! 把 aterm_favicon.ico 嵌入 exe 的 Windows 资源（RT_ICON / RT_GROUP_ICON），
//! 让资源管理器、任务栏显示程序图标。
//! 不依赖 rc.exe/windres：直接手工生成已编译的 .res 文件交给 MSVC 链接器。

use std::io::Write;

fn main() {
    println!("cargo:rerun-if-changed=aterm_favicon.ico");
    let ico = std::fs::read("aterm_favicon.ico").expect("读取 aterm_favicon.ico 失败");
    assert!(ico.len() >= 6 && ico[0..2] == [0, 0] && ico[2..4] == [1, 0], "不是 ICO 文件");
    let n = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    assert!(n > 0 && ico.len() >= 6 + n * 16, "ICO 目录损坏");

    let mut res: Vec<u8> = Vec::new();
    // .res 文件必须以一条空记录开头（DataSize=0、HeaderSize=32，
    // 类型/名称为序号标记 0xFFFF+0，与 rc.exe 输出一致），否则 link 报错
    res.extend_from_slice(&0u32.to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes());
    res.extend_from_slice(&0xFFFFu16.to_le_bytes());
    res.extend_from_slice(&0u16.to_le_bytes());
    res.extend_from_slice(&0xFFFFu16.to_le_bytes());
    res.extend_from_slice(&0u16.to_le_bytes());
    res.extend_from_slice(&[0u8; 16]);
    // GROUP_ICON 数据 = ICO 文件头 + 目录项（偏移字段换成资源 ID）
    let mut group: Vec<u8> = Vec::new();
    group.extend_from_slice(&ico[0..6]);
    for i in 0..n {
        let e = &ico[6 + i * 16..22 + i * 16];
        let size = u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize;
        let off = u32::from_le_bytes(e[12..16].try_into().unwrap()) as usize;
        assert!(off + size <= ico.len(), "ICO 图像数据越界");
        let id = (i + 1) as u16; // RT_ICON 资源 ID 从 1 起
        write_resource(&mut res, 3, id, &ico[off..off + size]);
        // GRPICONDIRENTRY：前 12 字节同 ICONDIRENTRY，但 planes 须为 1
        //（ICO 文件里常是 0，直接照抄会导致 CVTRES 报损坏）
        group.extend_from_slice(&e[0..4]);
        group.extend_from_slice(&1u16.to_le_bytes());
        group.extend_from_slice(&e[6..12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    write_resource(&mut res, 14, 1, &group); // RT_GROUP_ICON，ID 1 作为程序图标

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR 未设置");
    let path = std::path::Path::new(&out_dir).join("aterm_icon.res");
    let mut f = std::fs::File::create(&path).expect("创建 .res 失败");
    f.write_all(&res).expect("写入 .res 失败");
    // MSVC link.exe 可直接接受 .res 作为输入
    println!("cargo:rustc-link-arg-bins={}", path.display());
}

/// 追加一条已编译资源记录（类型/名称均为序号，头部 32 字节，数据按 4 字节对齐）。
fn write_resource(out: &mut Vec<u8>, rtype: u16, id: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // DataSize
    out.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    out.extend_from_slice(&0xFFFFu16.to_le_bytes()); // TYPE 序号标记
    out.extend_from_slice(&rtype.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes()); // NAME 序号标记
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    // MemoryFlags 与 rc.exe 一致：RT_ICON=MOVEABLE|PURE，RT_GROUP_ICON 另加 DISCARDABLE
    let flags: u16 = if rtype == 14 { 0x1030 } else { 0x1010 };
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&0x0409u16.to_le_bytes()); // Language: en-US（与 rc.exe 一致）
    out.extend_from_slice(&0u32.to_le_bytes()); // Version
    out.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

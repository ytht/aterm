//! ATerm —— 面向 BBS 的 Telnet/SSH 终端客户端（Rust + egui，无 Qt 依赖）。
#![windows_subsystem = "windows"]

mod config;
mod dialogs;
mod geoip;
mod gui;
mod imgprev;
mod net;
mod term;
mod zhconv;

use eframe::NativeOptions;
use gui::App;

/// 图标在编译期直接嵌入程序，拷贝到任何机器都可用，不依赖外部 ico 文件。
const ICON_BYTES: &[u8] = include_bytes!("../aterm_favicon.ico");

/// 解码内嵌图标为指定尺寸的 RGBA（供窗口/标题栏使用）。
fn icon_rgba(size: u32) -> Option<egui::IconData> {
    let img = image::load_from_memory(ICON_BYTES).ok()?;
    let rgba = img.resize_exact(size, size, image::imageops::FilterType::Lanczos3).to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(egui::IconData { rgba: rgba.into_raw(), width: w, height: h })
}

/// 把 RGBA 编码成 PNG 字节（CreateIconFromResourceEx 可直接识别 PNG 图标）。
fn rgba_to_png(icon: &egui::IconData) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(icon.width, icon.height, icon.rgba.clone())?;
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).ok()?;
    Some(out)
}

/// 主动给窗口设置标题栏图标（小图标=标题栏/Alt+Tab，大图标=任务栏）。
/// eframe 自带的设置依赖「窗口恰好激活」的时机，启动时可能落空；
/// 这里在首帧用找到的窗口句柄显式设置一次，确保标题栏左上角显示自定义图标。
fn set_titlebar_icon() -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(
            cb: unsafe extern "system" fn(*mut std::ffi::c_void, isize) -> i32,
            lparam: isize,
        ) -> i32;
        fn GetWindowThreadProcessId(hwnd: *mut std::ffi::c_void, pid: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: *mut std::ffi::c_void) -> i32;
        fn SendMessageW(hwnd: *mut std::ffi::c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn CreateIconFromResourceEx(
            data: *mut u8,
            len: u32,
            icon: i32,
            ver: u32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> *mut std::ffi::c_void;
        fn GetSystemMetrics(index: i32) -> i32;
    }
    const WM_SETICON: u32 = 0x0080;
    const ICON_SMALL: usize = 0;
    const ICON_BIG: usize = 1;
    const SM_CXICON: i32 = 11;
    const SM_CXSMICON: i32 = 49;

    // 枚举顶层窗口，找到本进程的可见窗口（即主窗口）
    unsafe extern "system" fn enum_cb(hwnd: *mut std::ffi::c_void, lparam: isize) -> i32 {
        let mut pid: u32 = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == std::process::id() && IsWindowVisible(hwnd) != 0 {
                *(lparam as *mut *mut std::ffi::c_void) = hwnd;
                return 0; // 找到即停止枚举
            }
        }
        1
    }
    let mut found: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: 回调只写调用方提供的局部变量；句柄判空后才使用
    unsafe { EnumWindows(enum_cb, &mut found as *mut _ as isize) };
    let hwnd = found;
    if hwnd.is_null() {
        return false;
    }
    let mut ok = true;
    for (kind, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
        let size = unsafe { GetSystemMetrics(metric) }.max(16) as u32;
        let Some(icon) = icon_rgba(size) else { ok = false; continue };
        let Some(mut png) = rgba_to_png(&icon) else { ok = false; continue };
        let hicon = unsafe {
            CreateIconFromResourceEx(
                png.as_mut_ptr(),
                png.len() as u32,
                1,          // 图标（非光标）
                0x00030000, // PNG 图标版本号
                size as i32,
                size as i32,
                0, // LR_DEFAULTCOLOR
            )
        };
        if hicon.is_null() {
            ok = false;
            continue;
        }
        unsafe { SendMessageW(hwnd, WM_SETICON, kind, hicon as isize) };
    }
    ok
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 长驻 tokio runtime：会话后台任务全部挂在这个 Handle 上
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;

    let opts = NativeOptions {
        renderer: eframe::Renderer::Glow,
        // 关闭「Ctrl+C/X/V 自动转成剪贴板事件」：终端里这些是 BBS 控制键，
        // 必须拿到原始按键事件（否则宏触发栏也永远收不到 ^C/^X/^V）。
        // 复制改用 Ctrl+Insert、粘贴改用 Shift+Insert，由程序自行处理；
        // 输入框的复制粘贴由 egui 内部按键逻辑照常支持。
        clipboard_shortcuts: false,
        viewport: {
            let mut vb = egui::ViewportBuilder::default()
                .with_title("ATerm")
                .with_inner_size([1120.0, 860.0])
                .with_min_inner_size([520.0, 320.0]);
            if let Some(icon) = icon_rgba(32) {
                vb = vb.with_icon(icon);
            }
            vb
        },
        ..Default::default()
    };

    let handle = rt.handle().clone();
    let app_creator = move |cc: &eframe::CreationContext<'_>| {
        let app = App::new(cc.egui_ctx.clone(), handle.clone());
        Ok(Box::new(app) as Box<dyn eframe::App>)
    };

    let res = eframe::run_native("ATerm", opts, Box::new(app_creator));
    // 窗口关闭后再释放 runtime，避免后台任务被强杀
    drop(rt);
    if let Err(e) = res {
        eprintln!("ATerm 退出：{e}");
    }
    Ok(())
}

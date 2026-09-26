//! Windows integration: dark title bar to match the dark UI.

#[cfg(windows)]
pub fn dark_title_bar() {
    use std::ffi::c_void;
    type Hwnd = *mut c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(cb: unsafe extern "system" fn(Hwnd, isize) -> i32, lparam: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: Hwnd) -> i32;
    }
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(hwnd: Hwnd, attr: u32, value: *const c_void, size: u32) -> i32;
    }
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_CAPTION_COLOR: u32 = 35;

    unsafe extern "system" fn each(hwnd: Hwnd, _: isize) -> i32 {
        let mut pid = 0u32;
        // SAFETY: plain Win32 calls on a window handle provided by EnumWindows.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == std::process::id() && IsWindowVisible(hwnd) != 0 {
                let on: i32 = 1;
                DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const i32 as *const c_void, 4);
                // Caption colour = panel background (#15171b as 0x00BBGGRR); Windows 11 only.
                let color: u32 = 0x001b_1715;
                DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &color as *const u32 as *const c_void, 4);
            }
        }
        1
    }
    // SAFETY: callback only touches windows of this process.
    unsafe {
        EnumWindows(each, 0);
    }
}

#[cfg(not(windows))]
pub fn dark_title_bar() {}

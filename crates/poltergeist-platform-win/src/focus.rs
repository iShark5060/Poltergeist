#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

#[cfg(windows)]
pub type WindowHandle = isize;
#[cfg(not(windows))]
pub type WindowHandle = i64;

pub fn current_foreground() -> Option<WindowHandle> {
    #[cfg(windows)]
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            None
        } else {
            Some(hwnd.0 as WindowHandle)
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub fn set_foreground(hwnd: WindowHandle) -> bool {
    #[cfg(windows)]
    unsafe {
        SetForegroundWindow(HWND(hwnd as *mut core::ffi::c_void)).as_bool()
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        false
    }
}

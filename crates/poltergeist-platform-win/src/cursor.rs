//! Helpers around the mouse cursor position.
//!
//! Used by the snippet popup so it can spawn directly at the user's
//! cursor when triggered by the global hotkey, mirroring the Python
//! version's behaviour. On non-Windows platforms a stub returns `None`.

#[cfg(windows)]
use windows::Win32::Foundation::POINT;
#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

/// Current screen-space cursor position in *physical* pixels, or
/// `None` if the OS call failed (or we aren't on Windows).
pub fn position() -> Option<(i32, i32)> {
    #[cfg(windows)]
    unsafe {
        let mut p = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut p as *mut POINT).is_ok() {
            Some((p.x, p.y))
        } else {
            None
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// `true` if any of the primary mouse buttons (left, right, middle) is
/// currently held down. We use `GetAsyncKeyState` (system-wide state)
/// rather than Slint's window-local mouse tracking because the snippet
/// popup needs to dismiss itself when the user clicks anywhere on the
/// desktop, including outside any of our windows.
pub fn primary_buttons_down() -> bool {
    #[cfg(windows)]
    unsafe {
        let any = |vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| -> bool {
            // High-bit set => physically down right now. We deliberately
            // ignore the low "toggled since last call" bit so we don't
            // race with whatever else on the system polls these keys.
            (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0
        };
        any(VK_LBUTTON) || any(VK_RBUTTON) || any(VK_MBUTTON)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
use windows::Win32::Foundation::POINT;
#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

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

pub fn primary_buttons_down() -> bool {
    #[cfg(windows)]
    unsafe {
        let any = |vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| -> bool {
            (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0
        };
        any(VK_LBUTTON) || any(VK_RBUTTON) || any(VK_MBUTTON)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

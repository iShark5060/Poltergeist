#[cfg(windows)]
use windows::Win32::Foundation::HANDLE;

#[cfg(windows)]
pub struct SingleInstanceGuard {
    handle: HANDLE,
}

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        if !self.handle.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
pub enum AcquireResult {
    Acquired(SingleInstanceGuard),
    AlreadyRunning,
}

#[cfg(windows)]
pub fn try_acquire(is_admin: bool) -> AcquireResult {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let name = mutex_name(is_admin);
    let wide: Vec<u16> = name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    unsafe {
        let handle = match CreateMutexW(None, false, PCWSTR(wide.as_ptr())) {
            Ok(h) => h,
            Err(_) => {
                return AcquireResult::Acquired(SingleInstanceGuard {
                    handle: HANDLE(std::ptr::null_mut()),
                })
            }
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            use windows::Win32::Foundation::CloseHandle;
            let _ = CloseHandle(handle);
            return AcquireResult::AlreadyRunning;
        }
        AcquireResult::Acquired(SingleInstanceGuard { handle })
    }
}

#[cfg(windows)]
pub fn show_already_running_dialog(is_admin: bool) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};

    let title = if is_admin {
        "Poltergeist [ADMIN]"
    } else {
        "Poltergeist"
    };
    let body = "Poltergeist is already running.\nLook for the icon in the system tray.";

    let title_w: Vec<u16> = title
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let body_w: Vec<u16> = body
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(body_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(windows)]
fn mutex_name(is_admin: bool) -> &'static str {
    if is_admin {
        "Global\\PoltergeistSnippetManager.Admin"
    } else {
        "Global\\PoltergeistSnippetManager"
    }
}

#[cfg(not(windows))]
pub struct SingleInstanceGuard;

#[cfg(not(windows))]
pub enum AcquireResult {
    Acquired(SingleInstanceGuard),
    AlreadyRunning,
}

#[cfg(not(windows))]
pub fn try_acquire(_is_admin: bool) -> AcquireResult {
    AcquireResult::Acquired(SingleInstanceGuard)
}

#[cfg(not(windows))]
pub fn show_already_running_dialog(_is_admin: bool) {}

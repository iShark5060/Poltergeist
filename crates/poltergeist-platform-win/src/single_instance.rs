//! Cross-edition single-instance enforcement.
//!
//! Mirrors the Python parent's named-mutex approach in `main.py` so that
//! both implementations cooperate: launching the Rust admin edition while
//! the Python admin edition is already running (or vice versa) gets the
//! same "already running" early exit instead of two trays fighting over
//! the same hotkeys.
//!
//! We intentionally use the `Global\` namespace mutex names from Python:
//!   - `Global\PoltergeistSnippetManager`        (user edition)
//!   - `Global\PoltergeistSnippetManager.Admin`  (admin edition)
//!
//! The two editions are *meant* to coexist on the same desktop because
//! UAC-elevated apps cannot send input to non-elevated windows, so they
//! get distinct mutexes. Within an edition, only one process is allowed.

#[cfg(windows)]
use windows::Win32::Foundation::HANDLE;

/// RAII guard that owns the single-instance mutex handle. Dropping this
/// closes the handle, which releases the named mutex back to the OS;
/// keep it alive for the lifetime of the process by binding it to a
/// `let _guard = ...` in `main`.
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

/// Outcome of attempting to acquire the single-instance mutex.
#[cfg(windows)]
pub enum AcquireResult {
    /// First (and only) instance — caller should keep `_guard` alive.
    Acquired(SingleInstanceGuard),
    /// Another process already holds the mutex; caller must exit cleanly.
    AlreadyRunning,
}

/// Whether the current process is the admin edition. Both Python and
/// Rust should agree on this (Python uses `services.edition.is_admin()`,
/// which checks the executable name suffix); the Rust app passes its
/// own `Edition` enum in here so the namespacing matches.
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
        // `bInitialOwner = false` matches Python: we do not need to be
        // the locking thread; the mere existence of the named mutex is
        // what other processes test for via `ERROR_ALREADY_EXISTS`.
        let handle = match CreateMutexW(None, false, PCWSTR(wide.as_ptr())) {
            Ok(h) => h,
            // CreateMutexW failing for any reason that isn't
            // ERROR_ALREADY_EXISTS means we cannot enforce
            // single-instance — fail open and let the app start
            // anyway, same as the Python side's `except Exception`.
            Err(_) => {
                return AcquireResult::Acquired(SingleInstanceGuard {
                    handle: HANDLE(std::ptr::null_mut()),
                })
            }
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // Close our duplicate handle so we don't leak it before
            // showing the message box and exiting.
            use windows::Win32::Foundation::CloseHandle;
            let _ = CloseHandle(handle);
            return AcquireResult::AlreadyRunning;
        }
        AcquireResult::Acquired(SingleInstanceGuard { handle })
    }
}

/// Show the same blocking modal Python does when a duplicate launch is
/// attempted. We use `MessageBoxW` directly so this works *before* Slint
/// (or any GUI framework) is initialised — important because we want
/// the duplicate process to die before it touches global hotkeys, the
/// tray icon, or the window registration.
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

// ---- Non-Windows stubs --------------------------------------------------
//
// The crate is `cfg(windows)` in practice, but `cargo check` on other
// hosts still compiles the module list. Provide inert stand-ins so the
// public API remains stable cross-platform.

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

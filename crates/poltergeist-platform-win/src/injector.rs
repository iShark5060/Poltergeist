use anyhow::Context;
use arboard::Clipboard;
use poltergeist_core::tokens::{
    expand_for_clipboard, expand_for_clipboard_segments, expand_for_typing, has_wait_or_key_tokens,
    DatabaseLookup, Segment, SnippetLookup,
};
use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use crate::focus::{set_foreground, WindowHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionMode {
    Clipboard,
    ClipboardShiftInsert,
    Typing,
    TypingCompat,
}

#[derive(Clone)]
pub struct InjectParams<'a> {
    pub snippet_text: &'a str,
    pub mode: InjectionMode,
    pub default_date_format: &'a str,
    pub target_hwnd: Option<WindowHandle>,
    pub paste_delay_ms: u64,
    pub restore_delay_ms: u64,
    pub context: Option<&'a HashMap<String, String>>,
    pub databases: Option<&'a dyn DatabaseLookup>,
    pub snippet_lookup: Option<&'a SnippetLookup<'a>>,
    pub expanded_override: Option<&'a str>,
}

pub fn inject(params: InjectParams<'_>) -> anyhow::Result<()> {
    let mut clipboard = Clipboard::new().context("failed to access clipboard")?;
    let original_clipboard = clipboard.get_text().unwrap_or_default();

    let mut text = params.snippet_text.to_string();
    if let Some(override_text) = params.expanded_override {
        text = override_text.to_string();
    }

    if let Some(hwnd) = params.target_hwnd {
        let _ = set_foreground(hwnd);
        thread::sleep(Duration::from_millis(30));
    }

    match params.mode {
        InjectionMode::Typing => {
            let segments = if params.expanded_override.is_some() {
                vec![Segment::Text(text)]
            } else {
                expand_for_typing(
                    &text,
                    params.default_date_format,
                    &original_clipboard,
                    params.context,
                    params.databases,
                    params.snippet_lookup,
                )
            };
            apply_typing_segments(&segments)?;
            return Ok(());
        }
        InjectionMode::TypingCompat => {
            let segments = if params.expanded_override.is_some() {
                vec![Segment::Text(text)]
            } else {
                expand_for_typing(
                    &text,
                    params.default_date_format,
                    &original_clipboard,
                    params.context,
                    params.databases,
                    params.snippet_lookup,
                )
            };
            apply_typing_compat_segments(&segments)?;
            return Ok(());
        }
        InjectionMode::Clipboard | InjectionMode::ClipboardShiftInsert => {}
    }

    let paste_hotkey = if params.mode == InjectionMode::ClipboardShiftInsert {
        "shift+insert"
    } else {
        "ctrl+v"
    };

    if params.expanded_override.is_some() {
        clipboard
            .set_text(text)
            .context("failed to set clipboard for injection")?;
        thread::sleep(Duration::from_millis(params.paste_delay_ms));
        send_hotkey(paste_hotkey)?;
        thread::sleep(Duration::from_millis(params.restore_delay_ms));
        let _ = clipboard.set_text(original_clipboard);
        return Ok(());
    }

    let segments = if has_wait_or_key_tokens(&text) {
        expand_for_clipboard_segments(
            &text,
            params.default_date_format,
            &original_clipboard,
            params.context,
            params.databases,
            params.snippet_lookup,
        )
    } else {
        vec![Segment::Text(expand_for_clipboard(
            &text,
            params.default_date_format,
            &original_clipboard,
            params.context,
            params.databases,
            params.snippet_lookup,
        ))]
    };

    for segment in segments {
        match segment {
            Segment::Wait(ms) => thread::sleep(Duration::from_millis(ms)),
            Segment::Hotkey(combo) => {
                send_hotkey(&combo)?;
                thread::sleep(Duration::from_millis(params.paste_delay_ms.max(120)));
            }
            Segment::Text(chunk) => {
                if chunk.is_empty() {
                    continue;
                }
                clipboard
                    .set_text(chunk)
                    .context("failed to set clipboard chunk")?;
                thread::sleep(Duration::from_millis(params.paste_delay_ms));
                send_hotkey(paste_hotkey)?;
                thread::sleep(Duration::from_millis(params.paste_delay_ms.max(200)));
            }
            Segment::Key(key) => send_hotkey(&key)?,
        }
    }
    thread::sleep(Duration::from_millis(params.restore_delay_ms));
    let _ = clipboard.set_text(original_clipboard);
    Ok(())
}

fn apply_typing_segments(segments: &[Segment]) -> anyhow::Result<()> {
    for segment in segments {
        match segment {
            Segment::Text(text) => send_text(text)?,
            Segment::Key(key) | Segment::Hotkey(key) => send_hotkey(key)?,
            Segment::Wait(ms) => thread::sleep(Duration::from_millis(*ms)),
        }
    }
    Ok(())
}

fn apply_typing_compat_segments(segments: &[Segment]) -> anyhow::Result<()> {
    for segment in segments {
        match segment {
            Segment::Text(text) => send_text_compat(text, 3)?,
            Segment::Key(key) | Segment::Hotkey(key) => send_hotkey(key)?,
            Segment::Wait(ms) => thread::sleep(Duration::from_millis(*ms)),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn send_text(text: &str) -> anyhow::Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE,
    };
    let mut inputs = Vec::new();
    for utf16 in text.encode_utf16() {
        let down = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: Default::default(),
                    wScan: utf16,
                    dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_UNICODE.0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let up = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: Default::default(),
                    wScan: utf16,
                    dwFlags: KEYBD_EVENT_FLAGS((KEYEVENTF_UNICODE | KEYEVENTF_KEYUP).0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        inputs.push(down);
        inputs.push(up);
    }
    unsafe {
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
    Ok(())
}

#[cfg(windows)]
fn send_text_compat(text: &str, per_char_delay_ms: u64) -> anyhow::Result<()> {
    let caps_was_on = caps_lock_on();
    if caps_was_on {
        send_vk(windows::Win32::UI::Input::KeyboardAndMouse::VK_CAPITAL)?;
    }
    for ch in text.chars() {
        if ch == '\r' {
            continue;
        }
        if ch == '\n' {
            send_hotkey("enter")?;
        } else if ch == '\t' {
            send_hotkey("tab")?;
        } else if !send_char_via_vk(ch)? {
            send_text(&ch.to_string())?;
        }
        if per_char_delay_ms > 0 {
            thread::sleep(Duration::from_millis(per_char_delay_ms));
        }
    }
    if caps_was_on {
        send_vk(windows::Win32::UI::Input::KeyboardAndMouse::VK_CAPITAL)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn send_text(_text: &str) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn send_text_compat(_text: &str, _per_char_delay_ms: u64) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn vk_for_main_key(key: &str) -> Option<windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VIRTUAL_KEY, VK_BACK, VK_CAPITAL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME,
        VK_INSERT, VK_LEFT, VK_LWIN, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB,
        VK_UP,
    };
    match key {
        "enter" | "return" => Some(VK_RETURN),
        "tab" => Some(VK_TAB),
        "esc" | "escape" => Some(VK_ESCAPE),
        "backspace" => Some(VK_BACK),
        "delete" => Some(VK_DELETE),
        "space" => Some(VK_SPACE),
        "insert" | "ins" => Some(VK_INSERT),
        "home" => Some(VK_HOME),
        "end" => Some(VK_END),
        "up" => Some(VK_UP),
        "down" => Some(VK_DOWN),
        "left" => Some(VK_LEFT),
        "right" => Some(VK_RIGHT),
        "page up" => Some(VK_PRIOR),
        "page down" => Some(VK_NEXT),
        "caps lock" => Some(VK_CAPITAL),
        "windows" | "win" => Some(VK_LWIN),
        k if k.len() == 1 => {
            let b = k.as_bytes()[0];
            if b.is_ascii_alphabetic() {
                Some(VIRTUAL_KEY(b.to_ascii_uppercase() as u16))
            } else if b.is_ascii_digit() {
                Some(VIRTUAL_KEY(b as u16))
            } else {
                None
            }
        }
        k if k.starts_with('f') && k.len() <= 3 => {
            let n: u16 = k[1..].parse().ok()?;
            if (1..=24).contains(&n) {
                Some(VIRTUAL_KEY(VK_F1.0 + n - 1))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(windows)]
fn send_vk(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> anyhow::Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP};
    let inputs = [
        key_input(vk, KEYBD_EVENT_FLAGS(0)),
        key_input(vk, KEYEVENTF_KEYUP),
    ];
    unsafe {
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
    Ok(())
}

#[cfg(windows)]
fn caps_lock_on() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL};
    unsafe { (GetKeyState(VK_CAPITAL.0 as i32) as i16 & 0x0001) != 0 }
}

#[cfg(windows)]
fn send_char_via_vk(ch: char) -> anyhow::Result<bool> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MapVirtualKeyW, SendInput, VkKeyScanW, INPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC,
        VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
    };

    let mut utf16 = [0u16; 2];
    let encoded = ch.encode_utf16(&mut utf16);
    if encoded.len() != 1 {
        return Ok(false);
    }
    let scan_result = unsafe { VkKeyScanW(encoded[0]) };
    if scan_result == -1 {
        return Ok(false);
    }
    let vk_u8 = (scan_result & 0xFF) as u8;
    let modifier_state = ((scan_result >> 8) & 0xFF) as u8;
    if modifier_state == 0xFF {
        return Ok(false);
    }

    let needs_shift = (modifier_state & 0x01) != 0;
    let needs_ctrl = (modifier_state & 0x02) != 0;
    let needs_alt = (modifier_state & 0x04) != 0;
    let vk = VIRTUAL_KEY(vk_u8 as u16);
    let _scan = unsafe { MapVirtualKeyW(vk_u8 as u32, MAPVK_VK_TO_VSC) };

    let mut inputs = Vec::new();
    if needs_shift {
        inputs.push(key_input(VK_SHIFT, KEYBD_EVENT_FLAGS(0)));
    }
    if needs_ctrl {
        inputs.push(key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)));
    }
    if needs_alt {
        inputs.push(key_input(VK_MENU, KEYBD_EVENT_FLAGS(0)));
    }
    inputs.push(key_input(vk, KEYBD_EVENT_FLAGS(0)));
    inputs.push(key_input(vk, KEYEVENTF_KEYUP));
    if needs_alt {
        inputs.push(key_input(VK_MENU, KEYEVENTF_KEYUP));
    }
    if needs_ctrl {
        inputs.push(key_input(VK_CONTROL, KEYEVENTF_KEYUP));
    }
    if needs_shift {
        inputs.push(key_input(VK_SHIFT, KEYEVENTF_KEYUP));
    }

    unsafe {
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
    Ok(true)
}

#[cfg(windows)]
fn send_hotkey(combo: &str) -> anyhow::Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN,
        VK_MENU, VK_SHIFT,
    };

    let mut modifiers = Vec::new();
    let parts = combo
        .split('+')
        .map(|p| p.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let key = parts.last().cloned().unwrap_or_default();
    for part in &parts[..parts.len().saturating_sub(1)] {
        match part.as_str() {
            "ctrl" | "control" => modifiers.push(VK_CONTROL),
            "shift" => modifiers.push(VK_SHIFT),
            "alt" => modifiers.push(VK_MENU),
            "windows" | "win" => modifiers.push(VK_LWIN),
            _ => {}
        }
    }
    let key_vk = vk_for_main_key(key.as_str()).unwrap_or(VIRTUAL_KEY(0));

    let mut inputs = Vec::new();
    for modifier in &modifiers {
        inputs.push(key_input(*modifier, KEYBD_EVENT_FLAGS(0)));
    }
    if key_vk.0 != 0 {
        inputs.push(key_input(key_vk, KEYBD_EVENT_FLAGS(0)));
        inputs.push(key_input(key_vk, KEYEVENTF_KEYUP));
    }
    for modifier in modifiers.into_iter().rev() {
        inputs.push(key_input(modifier, KEYEVENTF_KEYUP));
    }
    if !inputs.is_empty() {
        unsafe {
            let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn key_input(
    vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT};
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(not(windows))]
fn send_hotkey(_combo: &str) -> anyhow::Result<()> {
    Ok(())
}

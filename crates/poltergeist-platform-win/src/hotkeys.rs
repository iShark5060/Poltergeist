use anyhow::Context;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

pub type Binding = (String, String);

pub struct HotkeyManager {
    manager: GlobalHotKeyManager,
    active: HashMap<String, (String, HotKey)>,
    desired: Vec<Binding>,
    paused: bool,
}

impl HotkeyManager {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            manager: GlobalHotKeyManager::new().context("failed to init global hotkey manager")?,
            active: HashMap::new(),
            desired: Vec::new(),
            paused: false,
        })
    }

    pub fn install(
        &mut self,
        bindings: impl IntoIterator<Item = Binding>,
    ) -> HashMap<String, String> {
        let mut normalized = Vec::new();
        let mut seen = HashSet::new();
        for (name, hk) in bindings {
            if name.trim().is_empty() || hk.trim().is_empty() || !seen.insert(name.clone()) {
                continue;
            }
            normalized.push((name, normalize_hotkey(&hk)));
        }
        self.desired = normalized;
        self.sync()
    }

    pub fn set_paused(&mut self, paused: bool) -> HashMap<String, String> {
        self.paused = paused;
        self.sync()
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn poll_events(&self) -> Vec<u32> {
        let mut out = Vec::new();
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state == HotKeyState::Pressed {
                out.push(event.id);
            }
        }
        out
    }

    pub fn binding_name_for_id(&self, id: u32) -> Option<String> {
        self.active
            .iter()
            .find_map(|(name, (_, key))| (key.id() == id).then(|| name.clone()))
    }

    fn sync(&mut self) -> HashMap<String, String> {
        let mut skipped = HashMap::new();
        let desired_by_name = self
            .desired
            .iter()
            .cloned()
            .collect::<HashMap<String, String>>();

        let stale = self
            .active
            .iter()
            .filter_map(|(name, (hk, key))| {
                if self.paused || desired_by_name.get(name) != Some(hk) {
                    Some((name.clone(), *key))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for (name, key) in stale {
            let _ = self.manager.unregister(key);
            self.active.remove(&name);
        }

        if self.paused {
            return skipped;
        }

        let mut taken = self
            .active
            .iter()
            .map(|(name, (hk, _))| (hk.clone(), name.clone()))
            .collect::<HashMap<_, _>>();

        for (name, hotkey) in &self.desired {
            if self.active.contains_key(name) {
                continue;
            }
            if let Some(owner) = taken.get(hotkey) {
                skipped.insert(
                    name.clone(),
                    format!("hotkey '{hotkey}' is already bound to '{owner}'"),
                );
                continue;
            }
            let Ok(key) = parse_hotkey(hotkey) else {
                skipped.insert(name.clone(), format!("invalid hotkey '{hotkey}'"));
                continue;
            };
            if let Err(err) = self.manager.register(key) {
                skipped.insert(
                    name.clone(),
                    format!("could not register '{hotkey}': {err}"),
                );
                continue;
            }
            self.active.insert(name.clone(), (hotkey.clone(), key));
            taken.insert(hotkey.clone(), name.clone());
        }
        skipped
    }
}

fn normalize_hotkey(hotkey: &str) -> String {
    hotkey.trim().to_ascii_lowercase()
}

fn parse_hotkey(input: &str) -> anyhow::Result<HotKey> {
    let mut modifiers = Modifiers::empty();
    let parts = input.split('+').map(str::trim).collect::<Vec<_>>();
    let key_part = parts
        .last()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("empty hotkey"))?;
    for part in &parts[..parts.len().saturating_sub(1)] {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "alt" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "win" | "windows" | "meta" => modifiers |= Modifiers::SUPER,
            other => anyhow::bail!("unknown modifier '{other}'"),
        }
    }
    let code = match key_part.to_ascii_uppercase().as_str() {
        "SPACE" => Code::Space,
        "TAB" => Code::Tab,
        "ENTER" | "RETURN" => Code::Enter,
        "ESC" | "ESCAPE" => Code::Escape,
        raw if raw.len() == 1 => Code::from_str(raw)?,
        raw if raw.starts_with('F') => Code::from_str(raw)?,
        raw => Code::from_str(raw)?,
    };
    Ok(HotKey::new(Some(modifiers), code))
}

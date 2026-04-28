use anyhow::Context;
use poltergeist_core::contracts::merge_into_default;
use poltergeist_core::models::PoltergeistConfig;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub const CONFIG_FILENAME: &str = "poltergeist.json";
pub const DEFAULTS_FILENAME: &str = "poltergeist-defaults.json";

pub fn config_path(base_dir: &Path) -> PathBuf {
    base_dir.join(CONFIG_FILENAME)
}

pub fn defaults_path(base_dir: &Path) -> PathBuf {
    base_dir.join(DEFAULTS_FILENAME)
}

pub fn load(base_dir: &Path) -> PoltergeistConfig {
    let primary = read_json(config_path(base_dir));
    if primary.is_some() {
        return merge_into_default(primary);
    }
    let defaults = read_json(defaults_path(base_dir));
    merge_into_default(defaults)
}

pub fn is_first_run(base_dir: &Path) -> bool {
    !config_path(base_dir).exists()
}

pub fn save(base_dir: &Path, cfg: &PoltergeistConfig) -> anyhow::Result<()> {
    fs::create_dir_all(base_dir).context("failed to create config base dir")?;
    let target = config_path(base_dir);
    let tmp = target.with_extension("json.tmp");
    let payload = serde_json::to_vec_pretty(cfg).context("serialize config failed")?;
    fs::write(&tmp, payload).context("failed to write temp config file")?;
    fs::rename(&tmp, &target).context("failed to atomically replace config file")?;
    Ok(())
}

fn read_json(path: PathBuf) -> Option<Value> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

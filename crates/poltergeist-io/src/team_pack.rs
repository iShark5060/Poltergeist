use anyhow::Context;
use chrono::{SecondsFormat, Utc};
use poltergeist_core::models::Node;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const MANIFEST_BASENAME: &str = "manifest.json";
pub const TREE_BASENAME: &str = "team.poltergeist.json";
pub const DATABASES_BASENAME: &str = "databases";
pub const CACHE_DIRNAME: &str = "team_cache";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareStatus {
    Unconfigured,
    Reachable,
    Cached,
    Unreachable,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamManifest {
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub databases: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamTreeFile {
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub tree: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct TeamPack {
    pub tree: Vec<Node>,
    pub manifest: TeamManifest,
    pub source: ShareStatus,
}

impl Default for TeamPack {
    fn default() -> Self {
        Self {
            tree: Vec::new(),
            manifest: TeamManifest::default(),
            source: ShareStatus::Unconfigured,
        }
    }
}

pub fn cache_dir(base_dir: &Path) -> PathBuf {
    base_dir.join(CACHE_DIRNAME)
}

pub fn share_root(share_path: &str) -> Option<PathBuf> {
    let trimmed = share_path.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

pub fn probe_status(share_path: &str, base_dir: &Path) -> ShareStatus {
    let Some(root) = share_root(share_path) else {
        return ShareStatus::Unconfigured;
    };
    if read_manifest(&root).is_ok() {
        return ShareStatus::Reachable;
    }
    if cache_dir(base_dir).exists() {
        ShareStatus::Cached
    } else {
        ShareStatus::Unreachable
    }
}

pub fn read_pack_sync(share_path: &str, base_dir: &Path) -> TeamPack {
    let Some(root) = share_root(share_path) else {
        return load_from_cache(base_dir, ShareStatus::Unconfigured);
    };

    let manifest = read_manifest(&root);
    let tree = read_tree(&root);
    if let (Ok(manifest), Ok(tree)) = (manifest, tree) {
        let _ = refresh_local_cache(base_dir, &root, &manifest, &tree);
        return TeamPack {
            tree,
            manifest,
            source: ShareStatus::Reachable,
        };
    }

    load_from_cache(base_dir, ShareStatus::Unreachable)
}

pub fn publish_to_share(
    share_path: &str,
    base_dir: &Path,
    tree: &[Node],
    bump_from: Option<i64>,
) -> anyhow::Result<TeamManifest> {
    let root =
        share_root(share_path).ok_or_else(|| anyhow::anyhow!("No team share path configured"))?;
    if !root.exists() {
        anyhow::bail!("Share path does not exist: {}", root.display());
    }

    let base = match bump_from {
        Some(v) => v,
        None => read_manifest(&root).map(|m| m.version).unwrap_or(0),
    };

    let mut manifest = TeamManifest {
        version: base + 1,
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        databases: Vec::new(),
    };

    let db_dir = root.join(DATABASES_BASENAME);
    if db_dir.is_dir() {
        let mut dbs = fs::read_dir(&db_dir)
            .ok()
            .into_iter()
            .flat_map(|it| it.flatten())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter_map(|p| {
                p.file_name()
                    .and_then(|f| f.to_str())
                    .map(ToOwned::to_owned)
            })
            .collect::<Vec<_>>();
        dbs.sort();
        manifest.databases = dbs;
    }

    atomic_write_json(
        root.join(TREE_BASENAME),
        &TeamTreeFile {
            version: 1,
            tree: tree.to_vec(),
        },
    )?;
    atomic_write_json(root.join(MANIFEST_BASENAME), &manifest)?;
    let _ = refresh_local_cache(base_dir, &root, &manifest, tree);
    Ok(manifest)
}

fn load_from_cache(base_dir: &Path, fallback_source: ShareStatus) -> TeamPack {
    let cache = cache_dir(base_dir);
    let manifest_path = cache.join(MANIFEST_BASENAME);
    let tree_path = cache.join(TREE_BASENAME);
    let manifest = fs::read_to_string(manifest_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<TeamManifest>(&raw).ok())
        .unwrap_or_default();
    let tree = fs::read_to_string(tree_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<TeamTreeFile>(&raw).ok())
        .map(|v| v.tree)
        .unwrap_or_default();
    let source = if tree.is_empty() && manifest.version == 0 {
        fallback_source
    } else {
        ShareStatus::Cached
    };
    TeamPack {
        tree,
        manifest,
        source,
    }
}

fn read_manifest(root: &Path) -> anyhow::Result<TeamManifest> {
    let path = root.join(MANIFEST_BASENAME);
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str::<TeamManifest>(&raw).context("failed to parse manifest")
}

fn read_tree(root: &Path) -> anyhow::Result<Vec<Node>> {
    let path = root.join(TREE_BASENAME);
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let parsed = serde_json::from_str::<TeamTreeFile>(&raw).context("failed to parse team tree")?;
    Ok(parsed.tree)
}

fn refresh_local_cache(
    base_dir: &Path,
    root: &Path,
    manifest: &TeamManifest,
    tree: &[Node],
) -> anyhow::Result<()> {
    let target = cache_dir(base_dir);
    fs::create_dir_all(&target).context("failed to create team cache dir")?;
    atomic_write_json(target.join(MANIFEST_BASENAME), manifest)?;
    atomic_write_json(
        target.join(TREE_BASENAME),
        &TeamTreeFile {
            version: 1,
            tree: tree.to_vec(),
        },
    )?;

    let remote_db_dir = root.join(DATABASES_BASENAME);
    if remote_db_dir.is_dir() {
        let local_db_dir = target.join(DATABASES_BASENAME);
        fs::create_dir_all(&local_db_dir)?;
        let remote_files = fs::read_dir(&remote_db_dir)
            .ok()
            .into_iter()
            .flat_map(|it| it.flatten())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect::<Vec<_>>();

        let mut keep_names = std::collections::HashSet::new();
        for src in remote_files {
            if let Some(name) = src.file_name().and_then(|s| s.to_str()) {
                keep_names.insert(name.to_ascii_lowercase());
            }
            let dst = local_db_dir.join(src.file_name().unwrap_or_default());
            let _ = fs::copy(src, dst);
        }

        if let Ok(entries) = fs::read_dir(&local_db_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !keep_names.contains(&name) {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
    Ok(())
}

fn atomic_write_json<T: Serialize>(target: PathBuf, payload: &T) -> anyhow::Result<()> {
    let tmp = target.with_extension(format!(
        "{}.tmp",
        target
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
    ));
    let body = serde_json::to_vec_pretty(payload).context("serialize json failed")?;
    fs::write(&tmp, body).with_context(|| format!("write failed for {}", tmp.display()))?;
    fs::rename(&tmp, &target).with_context(|| format!("rename failed for {}", target.display()))?;
    Ok(())
}

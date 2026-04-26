use anyhow::Context;
use chrono::{SecondsFormat, Utc};
use poltergeist_core::models::Node;
use reqwest::blocking::Client;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// True when `share_path` is an `http://` or `https://` folder base (manifest and tree are
/// fetched as `{base}/manifest.json` and `{base}/team.poltergeist.json`).
pub fn is_http_share(share_path: &str) -> bool {
    let t = share_path.trim();
    t.starts_with("http://") || t.starts_with("https://")
}

pub fn share_root(share_path: &str) -> Option<PathBuf> {
    let trimmed = share_path.trim();
    if trimmed.is_empty() || is_http_share(trimmed) {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

pub fn probe_status(share_path: &str, base_dir: &Path) -> ShareStatus {
    let trimmed = share_path.trim();
    if trimmed.is_empty() {
        return ShareStatus::Unconfigured;
    }
    if is_http_share(trimmed) {
        let cache_ok = cache_dir(base_dir).join(MANIFEST_BASENAME).exists();
        let reachable = match http_client() {
            Ok(c) => join_urls(trimmed, MANIFEST_BASENAME)
                .ok()
                .and_then(|u| http_fetch_text(&c, &u).ok())
                .is_some(),
            Err(_) => false,
        };
        return if reachable {
            ShareStatus::Reachable
        } else if cache_ok {
            ShareStatus::Cached
        } else {
            ShareStatus::Unreachable
        };
    }

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
    let trimmed = share_path.trim();
    if trimmed.is_empty() {
        return load_from_cache(base_dir, ShareStatus::Unconfigured);
    }
    if is_http_share(trimmed) {
        return read_pack_from_http(trimmed, base_dir);
    }

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
    if is_http_share(share_path.trim()) {
        anyhow::bail!(
            "Publishing to an HTTP(S) URL is not supported. Use a UNC or local folder, \
             or upload manifest.json, team.poltergeist.json, and the databases folder via your cloud provider."
        );
    }
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

        let mut keep_names = HashSet::new();
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

fn http_client() -> anyhow::Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(45))
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("Poltergeist/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(16))
        .build()
        .context("failed to build HTTP client for team share")
}

fn join_urls(base: &str, rel: &str) -> anyhow::Result<String> {
    let base = Url::parse(base.trim()).context("invalid team share base URL")?;
    let joined = base
        .join(rel.trim_start_matches('/'))
        .context("invalid path for team share URL")?;
    Ok(joined.into())
}

fn http_fetch_text(client: &Client, url: &str) -> anyhow::Result<String> {
    let resp = client
        .get(url)
        .send()
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("GET {url} -> HTTP {}", resp.status());
    }
    resp.text().with_context(|| format!("read body {url}"))
}

fn read_pack_from_http(base_url: &str, base_dir: &Path) -> TeamPack {
    let Ok(client) = http_client() else {
        return load_from_cache(base_dir, ShareStatus::Unreachable);
    };
    let Ok(m_url) = join_urls(base_url, MANIFEST_BASENAME) else {
        return load_from_cache(base_dir, ShareStatus::Unreachable);
    };
    let Ok(t_url) = join_urls(base_url, TREE_BASENAME) else {
        return load_from_cache(base_dir, ShareStatus::Unreachable);
    };
    let (m_raw, t_raw) = match (
        http_fetch_text(&client, &m_url),
        http_fetch_text(&client, &t_url),
    ) {
        (Ok(m), Ok(t)) => (m, t),
        (Err(e), _) | (_, Err(e)) => {
            tracing::warn!(error = %e, "team share HTTP fetch failed");
            return load_from_cache(base_dir, ShareStatus::Unreachable);
        }
    };
    let manifest = match serde_json::from_str::<TeamManifest>(&m_raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "team share manifest JSON parse failed");
            return load_from_cache(base_dir, ShareStatus::Unreachable);
        }
    };
    let tree = match serde_json::from_str::<TeamTreeFile>(&t_raw) {
        Ok(tf) => tf.tree,
        Err(e) => {
            tracing::warn!(error = %e, "team share tree JSON parse failed");
            return load_from_cache(base_dir, ShareStatus::Unreachable);
        }
    };
    if let Err(e) = refresh_http_cache(base_dir, base_url, &manifest, &tree, &client) {
        tracing::warn!(error = %e, "team share HTTP cache refresh failed");
    }
    TeamPack {
        tree,
        manifest,
        source: ShareStatus::Reachable,
    }
}

fn refresh_http_cache(
    base_dir: &Path,
    base_url: &str,
    manifest: &TeamManifest,
    tree: &[Node],
    client: &Client,
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

    let local_db_dir = target.join(DATABASES_BASENAME);
    let mut keep_names = HashSet::new();

    if manifest.databases.is_empty() {
        if local_db_dir.is_dir() {
            for entry in fs::read_dir(&local_db_dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_file() {
                    let _ = fs::remove_file(path);
                }
            }
        }
        return Ok(());
    }

    fs::create_dir_all(&local_db_dir)?;
    for name in &manifest.databases {
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            continue;
        }
        let url = join_urls(base_url, &format!("{}/{}", DATABASES_BASENAME, name))?;

        match http_fetch_bytes(client, &url) {
            Ok(bytes) => {
                let dst = local_db_dir.join(name);
                let tmp = dst.with_extension("part");
                fs::write(&tmp, bytes).with_context(|| format!("write {}", dst.display()))?;
                fs::rename(&tmp, &dst).with_context(|| format!("rename {}", dst.display()))?;
                keep_names.insert(name.to_ascii_lowercase());
            }
            Err(e) => tracing::warn!(error = %e, %url, "download team database failed"),
        }
    }

    if let Ok(entries) = fs::read_dir(&local_db_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let fname = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !keep_names.contains(&fname) {
                let _ = fs::remove_file(path);
            }
        }
    }
    Ok(())
}

fn http_fetch_bytes(client: &Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("GET {url} -> HTTP {}", resp.status());
    }
    resp.bytes()
        .map(|b| b.to_vec())
        .with_context(|| format!("read bytes {url}"))
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

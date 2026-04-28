use std::path::{Path, PathBuf};

fn walkdir(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some(ext) {
                out.push(path);
            }
        }
    }
    out
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());

    let slint_entry = manifest_dir.join("ui").join("main.slint");
    println!("cargo:rerun-if-changed={}", slint_entry.display());
    let lang_dir = manifest_dir.join("lang");
    if lang_dir.is_dir() {
        for entry in walkdir(&lang_dir, "po") {
            println!("cargo:rerun-if-changed={}", entry.display());
        }
    }
    let style = std::env::var("SLINT_STYLE").unwrap_or_else(|_| "fluent-dark".to_string());
    let mut config = slint_build::CompilerConfiguration::new().with_style(style);
    if lang_dir.is_dir() {
        config = config.with_bundled_translations(lang_dir);
    }
    if let Err(err) = slint_build::compile_with_config(&slint_entry, config) {
        panic!("Failed to compile Slint UI: {err}");
    }

    #[cfg(target_os = "windows")]
    {
        let assets_dir = manifest_dir.join("..").join("..").join("assets");
        let icon_candidates: [PathBuf; 2] =
            if std::env::var_os("CARGO_FEATURE_ADMIN_EDITION").is_some() {
                [
                    assets_dir.join("AppIconAdmin.ico"),
                    assets_dir.join("AppIcon.ico"),
                ]
            } else {
                [
                    assets_dir.join("AppIcon.ico"),
                    assets_dir.join("AppIconAdmin.ico"),
                ]
            };
        for candidate in icon_candidates {
            if candidate.exists() {
                println!("cargo:rerun-if-changed={}", candidate.display());
                let mut resource = winres::WindowsResource::new();
                resource.set_icon(candidate.to_string_lossy().as_ref());
                if let Err(err) = resource.compile() {
                    eprintln!(
                        "cargo:warning=Failed to embed Windows icon resource: {err}. \
                         Continuing without an embedded icon."
                    );
                }
                break;
            }
        }
    }
}

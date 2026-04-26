# Poltergeist Rust Port

Windows-first Rust port of Poltergeist using Slint for UI.

## Prerequisites

- Windows 10/11
- Rust toolchain (`rustup`, `cargo`) with Rust 1.77+
  Check with:

```powershell
rustc --version
cargo --version
```

- (If build fails on linker tools) install **Visual Studio Build Tools** with the C++ desktop workload.

## Project Layout

- Workspace root: `D:\Development\Poltergeist-Rust`
- Main app crate (desktop executable): `crates/poltergeist-app`
- Shared runtime/contracts: `crates/poltergeist-core`
- IO/config/team-share integration: `crates/poltergeist-io`
- Windows platform runtime: `crates/poltergeist-platform-win`

## First Build (Debug)

From the workspace root:

```powershell
cargo build -p poltergeist-app
```

Debug executable output:

- `target\debug\poltergeist-app.exe`

## Run The App (Development)

From the workspace root:

```powershell
cargo run -p poltergeist-app
```

## Release Build

```powershell
cargo build -p poltergeist-app --release
```

Release executable output:

- `target\release\poltergeist-app.exe`

## Useful Validation Commands

From workspace root:

```powershell
cargo fmt --all
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Configuration And Runtime Files

The app uses the **executable directory** as its runtime base folder.

- Loads config from `poltergeist.json` (or falls back to `poltergeist-defaults.json`)
- Writes/saves config to `poltergeist.json`
- Uses team cache folder `team_cache\`

When running via `cargo run`, the base folder is typically under `target\debug\`, so config/cache files will be created there.

## Assets

Keep runtime assets next to the executable in an `assets\` folder:

- `assets\AppIcon.ico`
- `assets\AppIconAdmin.ico`
- Font Awesome files (if configured)
- `assets\Icon to Font Substitution.txt` (if used)

The app has fallback probes for icon/substitution files, but `assets\` is the preferred location.

## Admin vs User Edition

Edition is detected in this order:

1. `POLTERGEIST_EDITION=admin|user` environment variable
2. Presence of `_admin.flag` file in the executable directory
3. Default: user edition

## Quick Start (First Time)

1. Open PowerShell in `D:\Development\Poltergeist-Rust`
2. Run `cargo run -p poltergeist-app`
3. If needed, place assets in `assets\`
4. Close/relaunch after changing edition mode (`POLTERGEIST_EDITION` or `_admin.flag`)


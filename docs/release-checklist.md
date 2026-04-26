# Release And Soak Checklist

## Pre-release Validation
- Run `cargo fmt --all -- --check`
- Run `cargo clippy --workspace --all-targets -- -D warnings`
- Run `cargo test --workspace`
- Run `cargo build --workspace --release`

## Soak Tests
- 500 repeated hotkey-trigger cycles in active desktop targets.
- Team share disconnection/reconnection loop (network cable/VPN simulate).
- DeepL throttling/timeout scenario with fallback messaging.
- Clipboard restoration verification after mixed wait/hotkey snippets.

## Packaging
- Build portable zip/folder with:
  - executable
  - `poltergeist-defaults.json` (optional for onboarding)
  - Font Awesome files (if configured)
  - attribution text in About dialog.

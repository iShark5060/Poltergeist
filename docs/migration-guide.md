# Poltergeist Migration Guide (Python -> Rust)

## Scope
This guide covers moving an existing user profile from the Python build to the Rust build while keeping portable operation and no-admin constraints.

## Files Kept Compatible
- `poltergeist.json`
- `team_cache/manifest.json`
- `team_cache/team.poltergeist.json`
- `team_cache/databases/*`

## Migration Steps
1. Exit the Python app.
2. Copy the Rust portable folder to a target directory.
3. Copy `poltergeist.json` and `team_cache/` from the old app directory into the Rust app directory.
4. If using Team Share, verify `settings.team_share_path` still points to the correct UNC/share path.
5. Launch Rust app and verify:
   - personal snippets load
   - team snippets/cached team snippets load
   - popup/hotkey registration succeeds
   - token expansion works with your common snippets.

## Font Awesome Icons
- Place Font Awesome Free font files in `assets/`.
- Keep `assets/Icon to Font Substitution.txt` alongside the font files.
- Tray/app icons remain raster assets for Windows integration points requiring ICO/PNG.

## DeepL
- Existing API key value from `poltergeist.json` is reused.
- Validate in-app after migration.

## Rollback
To rollback, close Rust app and run the Python version using the same `poltergeist.json` and `team_cache`.

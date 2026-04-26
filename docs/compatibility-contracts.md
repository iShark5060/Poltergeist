# Poltergeist Compatibility Contracts

This file defines the Python-to-Rust behavior contracts that must remain stable for a safe cutover.

## Config Contract
- File name: `poltergeist.json`.
- Bootstrap fallback: `poltergeist-defaults.json` when primary config is missing.
- Schema:
  - `version` (currently `2`)
  - `settings`
  - `tree_personal`
  - `tree_team`
- Migration:
  - Legacy `tree` maps to `tree_personal`.
- Save behavior:
  - Atomic write (`*.tmp` + rename).

## Tree Contract
- Nodes are either `folder` or `snippet`.
- `folder` fields:
  - `id`, `name`, `children`, optional `color`, optional `shortcut`, optional `match`.
- `snippet` fields:
  - `id`, `name`, `text`, optional `injection`, optional `color`, optional `match`,
    `prompt_untranslated_before_paste` default `true`.

## Match Rule Contract
- Clause syntax: `var op value` with `;` as AND separator.
- Supported operators:
  - `=`, `==`, `!=`, `<>`, `in`, `not in`, `!in`
  - `contains`, `startswith`, `endswith`, `regex`/`matches`
  - optional modifier `?` on operators.
- Keywords `hide|never|no` map to a `never` rule and hide item from popup.

## Token Contract
- Escaping:
  - `{{` => `{`
  - `}}` => `}`
- Scalar tokens:
  - `{DATE}` / `{DATE:%d.%m.%Y}` / `{DATE=%d.%m.%Y}`
  - `{CLIPBOARD}`
  - `{VAR=name}` / `{VAR:name}`
  - `{DATABASE=file,key,column}` and `$var` substitutions in args
  - `{INCLUDE=name}` recursive with depth cap
- Key/wait tokens:
  - `{TAB}`, `{ENTER}`, combos `{CTRL+A}`, repeats `{TAB=5}`, wait `{WAIT=300}`
- Conditionals:
  - `{IF ...}{ELSIF ...}{ELSE}{END}` with nested block support.

## Team Pack Contract
- Share files:
  - `manifest.json`
  - `team.poltergeist.json`
  - optional `databases/*`
- Read precedence:
  1. share
  2. local cache (`team_cache/`)
- Cache includes mirrored `databases/`.

## Translation Contract
- Block syntax:
  - `{TRANSLATION=DE}...{TRANSLATION_END}`
  - `{TRANSLATION=EN>DE}...{TRANSLATION_END}`
- Grouping by `(source,target)` pair is required for API efficiency.
- Failure mode is fail-fast and explicit.

## Injection Contract
- Modes:
  - `clipboard`
  - `clipboard_shift_insert`
  - `typing`
  - `typing_compat`
- Clipboard original content is restored after injection attempt.
- `WAIT` and key tokens split clipboard mode into execution segments.

# Poltergeist - Snippet Tutorial (Rust)

Everything you can do inside a snippet body in the Rust/Slint app, with
examples you can paste into the snippet editor and adapt.

## Table of contents

- [Quick primer](#quick-primer)
- [Escaping literal braces](#escaping-literal-braces)
- [Token syntax at a glance](#token-syntax-at-a-glance)
- [Core tokens](#core-tokens)
- [Conditionals](#conditionals)
- ["Show when..." filters](#show-when-filters)
- [Injection mode guidance](#injection-mode-guidance)
- [Worked example](#worked-example)
- [Where to next](#where-to-next)

---

## Quick primer

1. Add one or more regexes in `Options > Context extraction`.
2. Select text in any target app (ticket, hostname, circuit id, etc.).
3. Press the global hotkey (default `Ctrl+Alt+Space`).
4. Pick a snippet from the popup.
5. Poltergeist expands tokens and injects the result.

Named regex groups become context variables consumed by `{VAR=...}`,
`{DATABASE=...}`, `{IF ...}`, and snippet/folder `Show when...` filters.

Example context regex:

```regex
(?P<country>[A-Z]{2})-(?P<type>[A-Za-z]+)-(?P<region>[A-Za-z0-9]+)-(?P<site>[A-Za-z0-9]+)
```

With input `DE-Sto-123-456`, variables resolve to:

- `country = DE`
- `type = Sto`
- `region = 123`
- `site = 456`

---

## Escaping literal braces

Use doubled braces for literal characters:

```text
Output: {{not a token}}
```

Produces:

```text
Output: {not a token}
```

---

## Token syntax at a glance

- Tokens use `{...}`.
- Most argument tokens accept **both** `=` and `:`:
  - `{DATE=%Y-%m-%d}` == `{DATE:%Y-%m-%d}`
  - `{WAIT=250}` == `{WAIT:250}`
  - `{VAR=country}` == `{VAR:country}`
  - `{DATABASE=Sites,key,col}` == `{DATABASE:Sites,key,col}`
  - `{INCLUDE=my-helper}` == `{INCLUDE:my-helper}`
  - `{TRANSLATION=DE>EN-GB}` == `{TRANSLATION:DE>EN-GB}`

This interchangeability is for single argument payload tokens; conditional
expressions keep their own grammar.

---

## Core tokens

### Date and clipboard

- `{DATE}` - uses the default format from settings.
- `{DATE:%d.%m.%Y %H:%M}` - explicit format.
- `{CLIPBOARD}` - text captured once when snippet expansion starts.

### Keys and waits

- Keys: `{TAB}`, `{ENTER}`, `{RETURN}`, `{ESC}`, `{DEL}`, `{BACKSPACE}`, `{SPACE}`, `{F1}`..`{F12}`
- Combos: `{CTRL+A}`, `{CTRL+SHIFT+ESC}`, `{ALT+F4}`, etc.
- Repeats: `{TAB=5}`, `{ENTER=3}`, `{CTRL+TAB=2}`
- Pauses: `{WAIT=300}` in milliseconds

### Context, database, include

- `{VAR=name}` - inject a captured context variable (empty string if missing).
- `{DATABASE=file,key,column}` - lookup in CSV/XLSX databases from team data.
- `{INCLUDE=SnippetName}` - inline another snippet by name (recursive with depth limit).

### Translation

- `{TRANSLATION=DE}...{TRANSLATION_END}`
- `{TRANSLATION=DE>EN-GB}...{TRANSLATION_END}`

Requires a valid DeepL API key in Options. Tokens inside the translation
block are expanded before text is sent to DeepL.

---

## Conditionals

Supported block shapes:

- `{IF ...}...{END}`
- `{IF ...}...{ELSE}...{END}`
- `{IF ...}...{ELSIF ...}...{ELSIF ...}...{ELSE}...{END}`

Aliases `ELIF` and `ELSEIF` are accepted.

Supported operators (case-insensitive):

- `=` / `==`, `!=` / `<>`
- `in`, `not in`, `!in`
- `contains`, `startswith`, `endswith`
- `matches`, `regex`

Optional `?` suffix means "also pass when variable is missing/empty":

- `type contains? Sto`
- `country =? DE`
- `country in? DE,AT,CH`

Example:

```text
{IF country in DE,AT,CH}
Guten Tag,
{ELSIF country in FR,BE}
Bonjour,
{ELSE}
Hello,
{END}
```

---

## "Show when..." filters

Each snippet and folder can define a visibility expression.

- Empty filter -> always visible
- Clauses separated by `;` -> AND logic
- Same operators as `{IF ...}`
- `hide`, `never`, or `no` -> never appears in popup (great for helpers)

Typical helper pattern:

```text
Show when: hide
```

Then reuse it only via `{INCLUDE=...}`.

---

## Injection mode guidance

- `clipboard (CTRL+V)` - default for most desktop targets
- `clipboard (Shift+INS)` - useful in some terminal surfaces
- `typing (Key Events)` - use when Ctrl+V is blocked or key events are required
- `typing (Web Terminal)` - for keycode-sensitive web terminals (xterm.js style behavior)

If a target is slow, insert `{WAIT=...}` between command steps.

---

## Worked example

```text
{IF country in FR,BE}
*** FRANCAIS ***
{TRANSLATION=DE>FR}{INCLUDE=outage-body-de}{TRANSLATION_END}

*** ENGLISH ***
{TRANSLATION=DE>EN-GB}{INCLUDE=outage-body-de}{TRANSLATION_END}
{ELSIF country in DE,AT,CH}
{INCLUDE=outage-body-de}
{ELSE}
{TRANSLATION=DE>EN-GB}{INCLUDE=outage-body-de}{TRANSLATION_END}
{END}
```

Why this pattern works well:

- One source snippet (`outage-body-de`) stays authoritative.
- Locale routing is explicit and readable.
- Adding a new language family is one extra `{ELSIF}` block.

---

## Where to next

- Build reusable helpers and keep them hidden via `Show when: hide`.
- Put shared CSV/XLSX files in the team data location for `{DATABASE=...}`.
- Use context regexes aggressively; good extraction rules remove duplication.
- Keep `README-rust.md` open for build, packaging, edition, and deployment details.

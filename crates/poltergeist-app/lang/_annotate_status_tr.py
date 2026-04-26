"""One-off helper: wrap `.set_status_text("LITERAL".into())` and
`.set_status_text(format!("LITERAL", args).into())` calls in main.rs with
`i18n::tr(...)` / `i18n::tr_format(...)` so status-bar messages are localizable.

Run with `--check` for a dry-run that just prints what would change.

The script is intentionally conservative: it only matches single-line calls
where the literal is plain ASCII-friendly text and uses no escape sequences
beyond the `{0}`-style placeholders we already support. Anything more exotic
(multi-line `.into()` chains, conditional expressions, complex `format!`
macros) is left untouched and must be hand-edited.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
TARGET = HERE.parent / "src" / "main.rs"

# Match `.set_status_text("LITERAL".into())` exactly, on a single line.
_LITERAL_RE = re.compile(
    r"""\.set_status_text\(\s*"((?:[^"\\]|\\.)*)"\.into\(\)\s*\)"""
)
# Match `.set_status_text(format!("LITERAL", a, b).into())` on a single line.
_FORMAT_RE = re.compile(
    r"""\.set_status_text\(\s*format!\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*([^)]*)\)\.into\(\)\s*\)"""
)
# Match a status_text inside a multi-line block where the literal sits alone.
_STANDALONE_LITERAL_RE = re.compile(r'^(\s*)"((?:[^"\\]|\\.)*)"\.into\(\),?\s*$')

# Skip strings that look like internal logs (no spaces, all-lowercase tokens) etc.
_SKIP_PREFIXES = ("DEBUG:", "TRACE:")


def _convert_format_string(literal: str, args_str: str) -> str | None:
    """Convert a Rust format!() literal with `{}` placeholders into the {0},
    {1}, ... form expected by `i18n::tr_format` and produce the matching
    tr_format call expression.

    Returns the replacement Rust expression suitable for use after the original
    `.set_status_text(` call, or None if the literal already mixes named or
    positional placeholders that we cannot safely rewrite.
    """
    # We only handle the simple `{}` (positional) placeholder case here. Rust
    # `format!` also supports `{name}`, `{0}` and format specifiers; if any of
    # those appear, bail out and let the caller hand-edit.
    if re.search(r"\{[^{}]*[^{}\s]\}", literal) and not re.fullmatch(
        r"(?:[^{}]|\{\}|\{\{|\}\})*", literal
    ):
        return None

    args_list = [arg.strip() for arg in _split_top_level(args_str) if arg.strip()]
    if not args_list:
        return None

    converted_literal = []
    placeholder_idx = 0
    i = 0
    while i < len(literal):
        ch = literal[i]
        if ch == "{" and i + 1 < len(literal) and literal[i + 1] == "{":
            converted_literal.append("{{")
            i += 2
            continue
        if ch == "}" and i + 1 < len(literal) and literal[i + 1] == "}":
            converted_literal.append("}}")
            i += 2
            continue
        if ch == "{" and i + 1 < len(literal) and literal[i + 1] == "}":
            converted_literal.append(f"{{{placeholder_idx}}}")
            placeholder_idx += 1
            i += 2
            continue
        converted_literal.append(ch)
        i += 1

    if placeholder_idx != len(args_list):
        return None

    converted = "".join(converted_literal)
    args_block = ", ".join(f"&{arg}" for arg in args_list)
    return f'.set_status_text(i18n::tr_format("{converted}", &[{args_block}]).into())'


def _split_top_level(args_str: str) -> list[str]:
    """Split a Rust comma-separated argument list at top-level commas only."""
    out: list[str] = []
    depth = 0
    buf: list[str] = []
    for ch in args_str:
        if ch in "([{<":
            depth += 1
        elif ch in ")]}>":
            depth -= 1
        if ch == "," and depth == 0:
            out.append("".join(buf))
            buf = []
        else:
            buf.append(ch)
    if buf:
        out.append("".join(buf))
    return out


def _should_skip(literal: str) -> bool:
    if not literal:
        return True
    stripped = literal.strip()
    if not stripped:
        return True
    if any(stripped.startswith(prefix) for prefix in _SKIP_PREFIXES):
        return True
    return False


def _rewrite(text: str) -> tuple[str, int]:
    changed = 0

    def _literal_sub(match: re.Match[str]) -> str:
        nonlocal changed
        literal = match.group(1)
        if _should_skip(literal):
            return match.group(0)
        # Already wrapped (e.g. starts with `i18n::tr(`)? `_LITERAL_RE` only
        # matches the bare literal form so this branch is purely defensive.
        if "i18n::tr" in match.group(0):
            return match.group(0)
        changed += 1
        return f'.set_status_text(i18n::tr("{literal}").into())'

    def _format_sub(match: re.Match[str]) -> str:
        nonlocal changed
        literal = match.group(1)
        args_str = match.group(2)
        if _should_skip(literal):
            return match.group(0)
        replacement = _convert_format_string(literal, args_str)
        if replacement is None:
            return match.group(0)
        changed += 1
        return replacement

    text = _LITERAL_RE.sub(_literal_sub, text)
    text = _FORMAT_RE.sub(_format_sub, text)
    return text, changed


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check", action="store_true", help="Dry-run; do not write file."
    )
    parser.add_argument("--target", type=Path, default=TARGET)
    args = parser.parse_args()

    target: Path = args.target
    if not target.exists():
        print(f"error: target not found: {target}", file=sys.stderr)
        return 2

    original = target.read_text(encoding="utf-8")
    rewritten, count = _rewrite(original)

    if args.check:
        print(f"would update {count} status-text call(s) in {target}")
        return 0

    if count == 0:
        print("no changes")
        return 0

    target.write_text(rewritten, encoding="utf-8")
    print(f"updated {count} status-text call(s) in {target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

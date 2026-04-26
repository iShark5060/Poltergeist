#!/usr/bin/env python3
"""Wrap user-facing string literals in main.slint with `@tr(...)`.

Targets four categories that are *always* labels, never user data:

    text: "<literal>";
    tooltip-text: "<literal>";
    placeholder-text: "<literal>";
    title: "<literal>";

The script is intentionally line-oriented and conservative:

* Skips lines where the literal is already wrapped (`@tr(`).
* Skips empty strings and single Font-Awesome glyphs (literals that
  contain `\\u`).
* Skips internal magic strings (the small allow-list at the bottom),
  things like ``"PAIR"`` that flow through Rust as identifiers, not
  display text.
* Leaves the rest of the surrounding line untouched (indentation,
  trailing comma/semicolon, comments).

The ``--check`` flag prints the diff without writing — handy when you
want to audit the planned rewrites before letting the script touch
``main.slint``.

Run from anywhere; the path resolution is rooted at this file's parent
directory so a stale CWD won't silently rewrite the wrong file:

    python crates/poltergeist-app/lang/_annotate_tr.py
    python crates/poltergeist-app/lang/_annotate_tr.py --check
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SLINT_FILE = HERE.parent / "ui" / "main.slint"

# Properties whose literal value is always a human-readable label.
# (TextInput / LineEdit `text:` bindings would normally be user data,
# but those don't appear with literal strings in main.slint — every
# editable field is data-bound. Verified by grep before adding.)
_LABEL_PROPS = ("text", "tooltip-text", "placeholder-text", "title")

_PROP_GROUP = "|".join(re.escape(p) for p in _LABEL_PROPS)

# Two patterns the rewrite considers:
#
# 1. "Standalone" form — the property sits on its own line inside a
#    component body. Matches the bulk of declarations:
#
#        text: "Hello";
#
# 2. "Inline" form — `FieldLabel { text: "..."; }` and similar
#    one-liners. We only rewrite inline matches when the leading
#    component identifier is in the allow-list below; this keeps the
#    rewriter from touching struct-literal model entries (e.g. the
#    `[ { text: "Foo", code: "bar" }, ... ]` arrays consumed by Rust).
_LINE_RE = re.compile(
    r'^(?P<indent>\s*)(?P<prop>(?:%s))\s*:\s*"(?P<lit>(?:[^"\\]|\\.)*)"\s*(?P<tail>;.*)?$'
    % _PROP_GROUP
)

# Allow-list of inline wrapper components whose `text:` field is a
# label rather than data. Anything else is left alone to avoid
# accidentally translating model-literal struct fields.
_INLINE_WRAPPERS = ("FieldLabel", "SectionTitle")
_INLINE_RE = re.compile(
    r'(?P<wrapper>(?:%s))\s*\{\s*(?P<prop>(?:%s))\s*:\s*"(?P<lit>(?:[^"\\]|\\.)*)"\s*;'
    % ("|".join(re.escape(w) for w in _INLINE_WRAPPERS), _PROP_GROUP)
)

# Literals we should *never* mark for translation even when they
# textually look label-shaped — they are codes/identifiers consumed by
# Rust (e.g. for ContextMenuArea entries that ship a code string up).
_SKIP_LITERALS = {
    "PAIR",
    "snippet",
    "folder",
    "none",
    "ok",
    "cancel",
    "yes",
    "no",
    "true",
    "false",
}


# Slint's `@tr(...)` treats `{N}` as a positional placeholder, so any
# *literal* curly brace inside the source text needs to be escaped as
# `{{` / `}}` (matching gettext / Rust's format!() conventions). Token
# names like `{DATE}` or `{VAR=name}` are *not* placeholders to Slint
# and would otherwise crash the build.
_PLACEHOLDER_RE = re.compile(r"\{(\d+)\}")


def _escape_braces(literal: str) -> str:
    out: list[str] = []
    i = 0
    while i < len(literal):
        if literal[i] == "{":
            m = _PLACEHOLDER_RE.match(literal, i)
            if m:
                out.append(m.group(0))
                i = m.end()
                continue
            out.append("{{")
            i += 1
            continue
        if literal[i] == "}":
            out.append("}}")
            i += 1
            continue
        out.append(literal[i])
        i += 1
    return "".join(out)


def _should_skip(literal: str) -> bool:
    if not literal:
        return True
    if "\\u" in literal:
        # Font-Awesome glyph escape like "\u{f07c}".
        return True
    if literal in _SKIP_LITERALS:
        return True
    if not any(ch.isalpha() for ch in literal):
        # Pure punctuation/numbers — not translatable.
        return True
    return False


def _rewrite(text: str) -> tuple[str, int]:
    """Return (new_text, count) — count is the number of lines rewritten."""
    out_lines: list[str] = []
    changes = 0
    for line in text.splitlines(keepends=True):
        ending = ""
        body = line
        if line.endswith("\r\n"):
            ending = "\r\n"
            body = line[:-2]
        elif line.endswith("\n"):
            ending = "\n"
            body = line[:-1]
        m = _LINE_RE.match(body)
        if m:
            literal = m.group("lit")
            if not _should_skip(literal) and not literal.startswith("@tr("):
                indent = m.group("indent")
                prop = m.group("prop")
                tail = m.group("tail") or ";"
                wrapped = _escape_braces(literal)
                body = f'{indent}{prop}: @tr("{wrapped}"){tail}'
                out_lines.append(body + ending)
                changes += 1
                continue

        # Inline wrapper rewrite — only fires for known label
        # components. We re-scan with `_INLINE_RE.sub` so a single line
        # could in theory hold multiple matches (rare in practice, but
        # cheap to support).
        def _sub_inline(match: re.Match[str]) -> str:
            nonlocal changes
            literal = match.group("lit")
            if _should_skip(literal):
                return match.group(0)
            wrapped = _escape_braces(literal)
            changes += 1
            return (
                f'{match.group("wrapper")} {{ {match.group("prop")}: @tr("{wrapped}");'
            )

        new_body, n = _INLINE_RE.subn(_sub_inline, body)
        out_lines.append(new_body + ending)
        if n == 0 and m is None:
            continue
    return "".join(out_lines), changes


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="dry-run; print diff only")
    args = parser.parse_args()
    original = SLINT_FILE.read_text(encoding="utf-8")
    rewritten, count = _rewrite(original)
    if args.check:
        if original == rewritten:
            print("no changes")
            return 0
        import difflib

        diff = difflib.unified_diff(
            original.splitlines(keepends=True),
            rewritten.splitlines(keepends=True),
            fromfile=str(SLINT_FILE),
            tofile=str(SLINT_FILE) + " (rewritten)",
        )
        sys.stdout.writelines(diff)
        print(f"\n[would rewrite {count} lines]")
        return 0
    if original == rewritten:
        print("no changes")
        return 0
    SLINT_FILE.write_text(rewritten, encoding="utf-8")
    print(f"rewrote {count} lines in {SLINT_FILE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

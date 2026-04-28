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
import difflib
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SLINT_FILE = HERE.parent / "ui" / "main.slint"

_LABEL_PROPS = ("text", "tooltip-text", "placeholder-text", "title")

_PROP_GROUP = "|".join(re.escape(p) for p in _LABEL_PROPS)

_LINE_RE = re.compile(
    rf'^(?P<indent>\s*)(?P<prop>(?:{_PROP_GROUP}))\s*:\s*"'
    r'(?P<lit>(?:[^"\\]|\\.)*)"\s*(?P<tail>;.*)?$'
)

_INLINE_WRAPPERS = ("FieldLabel", "SectionTitle")
_INLINE_RE = re.compile(
    r'(?P<wrapper>(?:%s))\s*\{\s*(?P<prop>(?:%s))\s*:\s*"(?P<lit>(?:[^"\\]|\\.)*)"\s*;'
    % ("|".join(re.escape(w) for w in _INLINE_WRAPPERS), _PROP_GROUP)
)

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
        return True
    if literal in _SKIP_LITERALS:
        return True
    if not any(ch.isalpha() for ch in literal):
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
    """Run the Slint translation-annotation rewrite in check or write mode."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="dry-run; print diff only")
    args = parser.parse_args()
    original = SLINT_FILE.read_text(encoding="utf-8")
    rewritten, count = _rewrite(original)
    if args.check:
        if original == rewritten:
            print("no changes")
            return 0

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

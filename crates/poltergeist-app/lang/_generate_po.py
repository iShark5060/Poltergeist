#!/usr/bin/env python3
"""Generate gettext .po files from the Python build's translation table.

Usage::

    python crates/poltergeist-app/lang/_generate_po.py

Reads ``D:/Development/PythonApps/Poltergeist/translations/_apply_translations.py``
to harvest the TRANSLATIONS dictionary, then emits one .po per locale
under ``crates/poltergeist-app/lang/<locale>/LC_MESSAGES/poltergeist-app.po``.

The .po files are consumed at build time by ``slint-build`` via the
``with_bundled_translations`` configuration option (see
``crates/poltergeist-app/build.rs``); after rebuilding, calling
``slint::select_bundled_translation("de")`` flips the UI to that
locale without an app restart.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
PYTHON_TRANSLATIONS_FILE = Path(
    "D:/Development/PythonApps/Poltergeist/translations/_apply_translations.py"
)
DOMAIN = "poltergeist-app"
LOCALES = ("de", "es", "fr")


def _load_translations() -> dict[str, dict[str, str]]:
    if not PYTHON_TRANSLATIONS_FILE.exists():
        sys.exit(f"Translation source not found: {PYTHON_TRANSLATIONS_FILE}")
    spec = importlib.util.spec_from_file_location(
        "py_translations", PYTHON_TRANSLATIONS_FILE
    )
    if spec is None or spec.loader is None:
        sys.exit("Could not import Python translation table")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.TRANSLATIONS


def _escape_po(text: str) -> str:
    """Escape a string for a .po `msgid`/`msgstr` literal."""
    out = text.replace("\\", "\\\\")
    out = out.replace('"', '\\"')
    out = out.replace("\n", "\\n")
    out = out.replace("\t", "\\t")
    return out


def _emit_msg_lines(label: str, text: str) -> list[str]:
    """Return the `label "..."` block (or multi-line continuation
    block when the string contains an embedded `\\n`).

    Multi-line .po format puts the first segment on its own (empty)
    line and the rest as continuations underneath. This matches what
    ``msgfmt`` and most translation tools produce, and keeps git
    diffs readable.
    """
    if "\n" not in text:
        return [f'{label} "{_escape_po(text)}"']
    parts = text.split("\n")
    lines = [f'{label} ""']
    for i, part in enumerate(parts):
        suffix = "\\n" if i < len(parts) - 1 else ""
        lines.append(f'"{_escape_po(part)}{suffix}"')
    return lines


def _write_po(locale: str, translations: dict[str, str]) -> Path:
    out_dir = HERE / locale / "LC_MESSAGES"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"{DOMAIN}.po"
    header = (
        'msgid ""\n'
        'msgstr ""\n'
        f'"Project-Id-Version: poltergeist-app\\n"\n'
        '"MIME-Version: 1.0\\n"\n'
        '"Content-Type: text/plain; charset=UTF-8\\n"\n'
        '"Content-Transfer-Encoding: 8bit\\n"\n'
        f'"Language: {locale}\\n"\n'
    )
    body_blocks: list[str] = []
    for src, target in translations.items():
        if not target:
            continue
        msgid_lines = _emit_msg_lines("msgid", src)
        msgstr_lines = _emit_msg_lines("msgstr", target)
        body_blocks.append("\n".join(msgid_lines + msgstr_lines))
    payload = header + "\n" + "\n\n".join(body_blocks) + "\n"
    out_path.write_text(payload, encoding="utf-8")
    return out_path


def main() -> None:
    table = _load_translations()
    counts: dict[str, int] = {}
    for locale in LOCALES:
        per_locale = {src: per_lang.get(locale, "") for src, per_lang in table.items()}
        path = _write_po(locale, per_locale)
        counts[locale] = sum(1 for v in per_locale.values() if v)
        print(f"[{locale}] wrote {counts[locale]} entries -> {path}")
    print("done.")


if __name__ == "__main__":
    main()

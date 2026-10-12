#!/usr/bin/env python3
"""Validate the plaudon threat-model documentation set.

This is the docs-only CI entry point. It performs exactly the same checks as
``crates/security/tests/threat_model_traceability.rs`` so that either entry
point catches drift:

* ``docs/threat-model/traceability.json`` parses and every row carries every
  required key, with unique sequential ids.
* every ``spec_threats`` entry (the verbatim SPEC v1.3 §16.2 threat column) is
  covered by exactly one row.
* every ``verification`` entry resolves to a Rust test function that really
  exists in the repository; ``status: planned`` rows carry none.
* every ``owner`` path exists on disk unless the row is ``planned``.
* every string of ``plaudon_security::outbound::NOT_GUARANTEED`` appears
  verbatim in ``docs/threat-model/README.md``.
* the threat-model documents contain no prohibited assurance language.
* every relative Markdown link in the threat-model documents resolves.
* the generated matrix table in ``README.md`` agrees with the JSON.
* every ``planned``/``open`` row is restated in ``open-findings.md``.

Usage:
    python3 scripts/check-threat-model.py            # verify, exit 0/1
    python3 scripts/check-threat-model.py --update   # regenerate README table
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DOC_DIR = REPO_ROOT / "docs" / "threat-model"
MATRIX = DOC_DIR / "traceability.json"
README = DOC_DIR / "README.md"
OPEN_FINDINGS = DOC_DIR / "open-findings.md"
OUTBOUND_RS = REPO_ROOT / "crates" / "security" / "src" / "outbound.rs"

ADR_FILES = [
    REPO_ROOT / "docs" / "adr" / "ADR-0013-single-egress-gate.md",
    REPO_ROOT / "docs" / "adr" / "ADR-0014-no-plaintext-credential-fallback.md",
    REPO_ROOT / "docs" / "adr" / "ADR-0015-out-of-process-untrusted-input.md",
]

REQUIRED_KEYS = ["id", "spec", "threat", "control", "owner", "epic", "verification", "status", "notes"]
STATUSES = ["implemented", "open", "planned"]
KNOWN_EPICS = set("ABCDEFGHIJKLMN")

TABLE_BEGIN = "<!-- BEGIN GENERATED MATRIX -->"
TABLE_END = "<!-- END GENERATED MATRIX -->"

# Language that the specification forbids us from using about this system
# (§16.2A, §16.6, §23 reference accuracy notes). The documents must describe
# the limit instead of asserting the assurance.
PROHIBITED_PHRASES = [
    "fully isolated",
    "completely isolated",
    "cryptographically verified identity",
    "military-grade",
    "unhackable",
    "guarantees privacy",
    "guarantees confidentiality",
    "gdpr compliant",
    "hipaa compliant",
    "data residency guarantee",
    "securely erased",
    "forensically erased",
]

FN_RE = re.compile(r"\bfn\s+([a-z_][A-Za-z0-9_]*)\s*[(<]")
STRING_RE = re.compile(r'"((?:[^"\\]|\\.)*)"')
LINK_RE = re.compile(r"\[[^\]]*\]\(([^)]+)\)")


def fail(errors: list[str], message: str) -> None:
    errors.append(message)


def rust_source_files() -> list[Path]:
    """Every Rust file that may define a test, mirroring the Rust checker."""
    roots = [REPO_ROOT / "crates", REPO_ROOT / "tests", REPO_ROOT / "apps"]
    files: list[Path] = []
    for root in roots:
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*.rs")):
            if "target" in path.parts:
                continue
            files.append(path)
    return files


def collect_test_functions() -> set[str]:
    """Names of functions annotated as tests anywhere in the repository."""
    names: set[str] = set()
    for path in rust_source_files():
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        pending_attributes: list[str] = []
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("#["):
                pending_attributes.append(stripped)
                continue
            match = FN_RE.search(stripped)
            if match is not None:
                if any("test" in attribute for attribute in pending_attributes):
                    names.add(match.group(1))
                pending_attributes = []
                continue
            if stripped and not stripped.startswith("//"):
                pending_attributes = []
    return names


def not_guaranteed_statements() -> list[str]:
    text = OUTBOUND_RS.read_text(encoding="utf-8")
    start = text.find("pub const NOT_GUARANTEED")
    if start < 0:
        raise SystemExit(f"NOT_GUARANTEED not found in {OUTBOUND_RS}")
    end = text.find("];", start)
    if end < 0:
        raise SystemExit(f"unterminated NOT_GUARANTEED literal in {OUTBOUND_RS}")
    body = text[start:end]
    body = body[body.index("=") + 1 :]
    return [m.group(1) for m in STRING_RE.finditer(body)]


def render_table(rows: list[dict]) -> str:
    lines = [
        "| ID | Spec | Threat | Control | Owner | Epic | Status | Verification |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for row in rows:
        verification = row["verification"]
        cell = ", ".join(f"`{name}`" for name in verification) if verification else "—"
        lines.append(
            "| {id} | §{spec} | {threat} | {control} | `{owner}` | {epic} | {status} | {verification} |".format(
                id=row["id"],
                spec=row["spec"],
                threat=row["threat"],
                control=row["control"],
                owner=row["owner"],
                epic=row["epic"],
                status=row["status"],
                verification=cell,
            )
        )
    return "\n".join(lines)


def replace_table(readme_text: str, table: str) -> str:
    begin = readme_text.find(TABLE_BEGIN)
    end = readme_text.find(TABLE_END)
    if begin < 0 or end < 0 or end < begin:
        raise SystemExit("README.md is missing the generated-matrix markers")
    head = readme_text[: begin + len(TABLE_BEGIN)]
    tail = readme_text[end:]
    return f"{head}\n\n{table}\n\n{tail}"


def extract_table(readme_text: str) -> str | None:
    begin = readme_text.find(TABLE_BEGIN)
    end = readme_text.find(TABLE_END)
    if begin < 0 or end < 0 or end < begin:
        return None
    return readme_text[begin + len(TABLE_BEGIN) : end].strip()


def check_matrix_shape(matrix: dict, errors: list[str]) -> list[dict]:
    if matrix.get("version") != 1:
        fail(errors, "traceability.json: version must be 1")
    rows = matrix.get("rows")
    if not isinstance(rows, list) or not rows:
        fail(errors, "traceability.json: `rows` must be a non-empty array")
        return []
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            fail(errors, f"row {index}: not an object")
            continue
        for key in REQUIRED_KEYS:
            if key not in row:
                fail(errors, f"row {index}: missing key `{key}`")
        if set(row) - set(REQUIRED_KEYS):
            extra = sorted(set(row) - set(REQUIRED_KEYS))
            fail(errors, f"row {index}: unexpected keys {extra}")
        expected_id = f"TM-{index + 1:03d}"
        if row.get("id") != expected_id:
            fail(errors, f"row {index}: id is {row.get('id')!r}, expected {expected_id!r} (ids must be sequential)")
        if row.get("status") not in STATUSES:
            fail(errors, f"{row.get('id')}: status {row.get('status')!r} is not one of {sorted(STATUSES)}")
        if row.get("epic") not in KNOWN_EPICS:
            fail(errors, f"{row.get('id')}: epic {row.get('epic')!r} is not a known epic letter")
        if not isinstance(row.get("verification"), list):
            fail(errors, f"{row.get('id')}: verification must be an array")
        for key in ("threat", "control", "owner", "notes", "spec"):
            value = row.get(key)
            if not isinstance(value, str) or not value.strip():
                fail(errors, f"{row.get('id')}: `{key}` must be a non-empty string")
            elif "|" in value:
                fail(errors, f"{row.get('id')}: `{key}` must not contain a pipe character (breaks the table)")
    return [row for row in rows if isinstance(row, dict)]


def check_spec_coverage(matrix: dict, rows: list[dict], errors: list[str]) -> None:
    spec_threats = matrix.get("spec_threats")
    if not isinstance(spec_threats, list) or not spec_threats:
        fail(errors, "traceability.json: `spec_threats` must be a non-empty array")
        return
    if len(set(spec_threats)) != len(spec_threats):
        fail(errors, "traceability.json: `spec_threats` contains duplicates")
    row_threats = [row.get("threat") for row in rows]
    if len(set(row_threats)) != len(row_threats):
        duplicates = sorted({t for t in row_threats if row_threats.count(t) > 1})
        fail(errors, f"traceability.json: duplicate row threats {duplicates}")
    for threat in spec_threats:
        count = row_threats.count(threat)
        if count != 1:
            fail(errors, f"SPEC §16.2 threat {threat!r} is covered by {count} rows, expected exactly 1")
    for row in rows:
        if row.get("spec") == "16.2" and row.get("threat") not in spec_threats:
            fail(errors, f"{row.get('id')}: claims spec 16.2 but its threat is not in `spec_threats`")


def check_rows_against_repo(rows: list[dict], errors: list[str]) -> None:
    test_functions = collect_test_functions()
    for row in rows:
        row_id = row.get("id")
        status = row.get("status")
        verification = row.get("verification") or []
        owner = row.get("owner", "")
        if status == "planned" and verification:
            fail(errors, f"{row_id}: planned rows must not cite verification tests")
        if status == "implemented" and not verification:
            fail(errors, f"{row_id}: implemented rows must cite at least one verification test")
        for name in verification:
            if name not in test_functions:
                fail(errors, f"{row_id}: verification test `{name}` does not exist in the repository")
        if status != "planned" and not (REPO_ROOT / owner).exists():
            fail(errors, f"{row_id}: owner path `{owner}` does not exist")


def check_readme(matrix: dict, rows: list[dict], errors: list[str]) -> None:
    text = README.read_text(encoding="utf-8")
    for statement in not_guaranteed_statements():
        if statement not in text:
            fail(errors, f"README.md does not reproduce NOT_GUARANTEED statement: {statement}")
    table = extract_table(text)
    if table is None:
        fail(errors, "README.md is missing the generated-matrix markers")
    elif table != render_table(rows):
        fail(errors, "README.md matrix table disagrees with traceability.json (run --update)")
    _ = matrix


def check_prohibited_language(errors: list[str]) -> None:
    for path in sorted(DOC_DIR.glob("*.md")) + ADR_FILES:
        if not path.exists():
            fail(errors, f"missing document: {path.relative_to(REPO_ROOT)}")
            continue
        lowered = path.read_text(encoding="utf-8").lower()
        for phrase in PROHIBITED_PHRASES:
            # The prohibition list itself lives in this script, not in the docs.
            if phrase in lowered:
                fail(errors, f"{path.relative_to(REPO_ROOT)}: prohibited assurance phrase {phrase!r}")


def check_links(errors: list[str]) -> None:
    for path in sorted(DOC_DIR.glob("*.md")) + ADR_FILES:
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8")
        for match in LINK_RE.finditer(text):
            target = match.group(1).strip()
            if target.startswith(("http://", "https://", "#", "mailto:")):
                continue
            target = target.split("#", 1)[0]
            if not target:
                continue
            resolved = (path.parent / target).resolve()
            if not resolved.exists():
                fail(errors, f"{path.relative_to(REPO_ROOT)}: broken relative link `{match.group(1)}`")


def check_open_findings(rows: list[dict], errors: list[str]) -> None:
    text = OPEN_FINDINGS.read_text(encoding="utf-8")
    for row in rows:
        if row.get("status") in ("planned", "open") and row.get("id") not in text:
            fail(errors, f"{row.get('id')}: status {row.get('status')} but it is not restated in open-findings.md")
        if row.get("status") == "implemented" and row.get("id") in text:
            fail(errors, f"{row.get('id')}: implemented rows must not appear in open-findings.md")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--update", action="store_true", help="regenerate the matrix table in README.md")
    args = parser.parse_args()

    try:
        matrix = json.loads(MATRIX.read_text(encoding="utf-8"))
    except FileNotFoundError:
        print(f"error: {MATRIX} does not exist", file=sys.stderr)
        return 1
    except json.JSONDecodeError as exc:
        print(f"error: {MATRIX} is not valid JSON: {exc}", file=sys.stderr)
        return 1

    errors: list[str] = []
    rows = check_matrix_shape(matrix, errors)
    if args.update:
        README.write_text(replace_table(README.read_text(encoding="utf-8"), render_table(rows)), encoding="utf-8")
        print(f"updated {README.relative_to(REPO_ROOT)}")
        return 0

    check_spec_coverage(matrix, rows, errors)
    check_rows_against_repo(rows, errors)
    check_readme(matrix, rows, errors)
    check_prohibited_language(errors)
    check_links(errors)
    check_open_findings(rows, errors)

    if errors:
        print(f"threat-model check FAILED with {len(errors)} problem(s):", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    implemented = sum(1 for row in rows if row["status"] == "implemented")
    open_rows = sum(1 for row in rows if row["status"] == "open")
    planned = sum(1 for row in rows if row["status"] == "planned")
    print(
        f"threat-model check OK: {len(rows)} rows "
        f"({implemented} implemented, {open_rows} open, {planned} planned), "
        f"{len(matrix['spec_threats'])} SPEC §16.2 threats covered 1:1"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

"""Every `license-files` entry reaches a real file, and `THIRD-PARTY.md` names
each component the wheel redistributes.

A `license-files` path that does not exist is dropped from the wheel without a
build error, and the components with notice obligations are the ones compiled
into the extension module rather than the ones visible in `pyproject.toml`.
"""

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]


def _declared_license_files() -> list[str]:
    text = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    block = re.search(r"^license-files = \[(.*?)\]", text, re.M | re.S)
    assert block is not None, "pyproject.toml declares no license-files"
    return re.findall(r'"([^"]+)"', block.group(1))


def test_every_declared_license_file_exists():
    declared = _declared_license_files()
    assert declared
    missing = [p for p in declared if not (ROOT / p).is_file()]
    assert missing == []


def test_third_party_names_the_redistributed_components():
    text = (ROOT / "THIRD-PARTY.md").read_text(encoding="utf-8")
    for component in ("GHIDRA", "rsleigh", "svg-pan-zoom", "@viz-js/viz"):
        assert component in text, f"THIRD-PARTY.md does not name {component}"


def test_ghidra_notice_and_apache_text_travel_with_the_sources():
    assert "Apache License" in (ROOT / "licenses" / "Apache-2.0.txt").read_text(
        encoding="utf-8"
    )
    assert "National Security Agency" in (
        ROOT / "licenses" / "NOTICE-ghidra.txt"
    ).read_text(encoding="utf-8")
    assert {"LICENSE", "THIRD-PARTY.md"} <= set(_declared_license_files())

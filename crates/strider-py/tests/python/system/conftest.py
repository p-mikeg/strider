"""Per-arch system-test parametrisation.

Mirrors `crates/strider/tests/common/mod.rs::per_arch_test!` for the
Python bindings.  Every test that takes the `arch_id` fixture runs once
per supported architecture; missing fixture binaries skip cleanly.
"""

from __future__ import annotations

import pathlib

import pytest

from ._helpers import ARCHES


# Fixture path: <repo>/fixtures/out
WORKSPACE_ROOT = pathlib.Path(__file__).resolve().parents[5]
FIXTURES_OUT = WORKSPACE_ROOT / "fixtures" / "out"


@pytest.fixture(scope="session")
def fixtures_dir() -> pathlib.Path:
    return FIXTURES_OUT


@pytest.fixture(params=[a.id for a in ARCHES])
def arch_id(request) -> str:
    return request.param

from __future__ import annotations

import pathlib

import pytest

from ._helpers import ARCHES


# Fixture binaries come from <repo>/fixtures/out (built by fixtures/Makefile).
WORKSPACE_ROOT = pathlib.Path(__file__).resolve().parents[5]
FIXTURES_OUT = WORKSPACE_ROOT / "fixtures" / "out"


@pytest.fixture(scope="session")
def fixtures_dir() -> pathlib.Path:
    return FIXTURES_OUT


@pytest.fixture(params=[a.id for a in ARCHES])
def arch_id(request) -> str:
    return request.param

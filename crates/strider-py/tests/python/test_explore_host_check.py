"""A page in the user's browser can resolve its own hostname to a loopback
address and reach the explorer, so a loopback-bound server enforces the `Host`
header. Any address in 127/8 is loopback, not just `127.0.0.1`."""

from __future__ import annotations

import http.client

import pytest

import strider
from strider import explore

from .conftest import fixture_path


@pytest.fixture
def served():
    """The explorer on a thread, over the one fixture function, plus a
    `request(host, bind)` that spoofs `Host`."""
    lift = strider.lift.load_elf(str(fixture_path("x64", "switch")))
    fn = lift.analyze(next(iter(lift.functions())).address).function
    ports: list[int] = []

    def start(bind: str) -> int:
        port = explore.visualize(fn, host=bind, background=True)
        ports.append(port)
        return port

    try:
        yield start
    finally:
        for port in ports:
            explore.shutdown(port)


def _get_entry(bind: str, port: int, host: str) -> int:
    conn = http.client.HTTPConnection(bind, port, timeout=15)
    try:
        conn.putrequest("GET", "/entry", skip_host=True)
        conn.putheader("Host", host)
        conn.endheaders()
        return conn.getresponse().status
    finally:
        conn.close()


@pytest.mark.parametrize("bind", ["127.0.0.1", "127.0.0.2"])
def test_a_loopback_bind_refuses_a_spoofed_host(bind, served):
    port = served(bind)
    assert _get_entry(bind, port, "evil.com") == 421
    assert _get_entry(bind, port, f"{bind}:{port}") == 200


def test_the_loopback_test_covers_every_spelling():
    assert explore._is_loopback("127.0.0.1")
    assert explore._is_loopback("127.0.0.2")
    assert explore._is_loopback("localhost")
    assert explore._is_loopback("::1")
    # An IPv4-mapped loopback address is one too, which `ipaddress` alone
    # reports as global.
    assert explore._is_loopback("::ffff:127.0.0.1")
    assert not explore._is_loopback("evil.com")
    assert not explore._is_loopback("0.0.0.0")
    assert not explore._is_loopback("")

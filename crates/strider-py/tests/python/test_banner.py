"""The import-time wordmark: stderr, a terminal, and `STRIDER_NO_BANNER`."""

import os
import subprocess
import sys

import pytest

pty = pytest.importorskip("pty")

# The wordmark's last row (figlet -f slant).
MARK = r"/____/\__/_/  /_/\__,_/\___/_/"


def _read_all(fd: int) -> bytes:
    chunks = []
    while True:
        try:
            chunk = os.read(fd, 4096)
        except OSError:  # EIO once the child closes the slave
            break
        if not chunk:
            break
        chunks.append(chunk)
    return b"".join(chunks)


def _import(*, stdout_tty: bool, stderr_tty: bool, env_extra=None):
    """Run `import strider`, each stream a pty or a pipe; return
    `(stdout, stderr)` bytes."""
    env = {k: v for k, v in os.environ.items() if k != "STRIDER_NO_BANNER"}
    env.update(env_extra or {})
    masters = {}
    slaves = {}
    for name, tty in (("out", stdout_tty), ("err", stderr_tty)):
        if tty:
            masters[name], slaves[name] = pty.openpty()
    proc = subprocess.Popen(
        [sys.executable, "-c", "import strider"],
        stdin=subprocess.DEVNULL,
        stdout=slaves.get("out", subprocess.PIPE),
        stderr=slaves.get("err", subprocess.PIPE),
        env=env,
    )
    for fd in slaves.values():
        os.close(fd)
    piped_out, piped_err = proc.communicate(timeout=120)
    got = {name: _read_all(fd) for name, fd in masters.items()}
    for fd in masters.values():
        os.close(fd)
    assert proc.returncode == 0, piped_err
    return got.get("out", piped_out), got.get("err", piped_err)


def test_banner_on_a_terminal_stderr():
    out, err = _import(stdout_tty=False, stderr_tty=True)
    assert MARK.encode() in err
    assert out == b""


def test_banner_silenced_by_env():
    out, err = _import(
        stdout_tty=True, stderr_tty=True, env_extra={"STRIDER_NO_BANNER": "1"}
    )
    assert MARK.encode() not in err
    assert out == b"" and err == b""


def test_banner_empty_env_is_unset():
    _, err = _import(
        stdout_tty=False, stderr_tty=True, env_extra={"STRIDER_NO_BANNER": ""}
    )
    assert MARK.encode() in err


def test_no_banner_on_a_piped_stderr():
    out, err = _import(stdout_tty=True, stderr_tty=False)
    assert out == b"" and err == b""


def test_banner_not_in_namespace():
    import strider

    assert not hasattr(strider, "_banner")


_ODD_STDERR = """
import sys

class Sink:
    def __init__(self):
        self.text = []
    def write(self, s):
        self.text.append(s)
    def flush(self):
        pass

class NoIsatty(Sink):
    pass

class IsattyRaises(Sink):
    def isatty(self):
        raise OSError("probe")

class WriteRaises(Sink):
    def isatty(self):
        return True
    def write(self, s):
        raise OSError("write")

kind = sys.argv[1]
if kind == "closed":
    sys.stderr.close()
else:
    sys.stderr = {"no_isatty": NoIsatty, "isatty_raises": IsattyRaises,
                  "write_raises": WriteRaises}[kind]()
import strider
written = "" if kind == "closed" else "".join(sys.stderr.text)
sys.stdout.write(written)
"""


@pytest.mark.parametrize(
    "kind", ["closed", "no_isatty", "isatty_raises", "write_raises"]
)
def test_import_survives_an_odd_stderr(kind):
    env = {k: v for k, v in os.environ.items() if k != "STRIDER_NO_BANNER"}
    proc = subprocess.run(
        [sys.executable, "-c", _ODD_STDERR, kind],
        capture_output=True, text=True, timeout=120, env=env,
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == ""

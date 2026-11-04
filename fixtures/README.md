# Fixtures

`out/<arch>/<case>.elf` is built from `cases/<case>.{c,S}` by `make`, one
target per (arch, case); every C case also builds an unlinked `.o` twin.
Every `.o` is committed: `strider-reader`'s release-only `read_digest`
digests every image under `out/`, so a new one needs its digest row
re-blessed.

The binaries are committed through Git LFS, so a clone without `git-lfs` gets
LFS pointer text where an ELF should be; run `git lfs pull` before
running the test suite.

Rebuilding is only needed when a case or an arch's flags change. See the header
of `Makefile` for the `ARCH=` / `CASE=` selectors.

`loader/out/` holds images the ELF reader tests need a link or flags of their
own for (dynamic GOT slots, IFUNC, TLS, copy relocations, ppc64 local
entries, ...). `make -C loader` builds them from `loader/src/`; see that
Makefile for the list and its cross compilers. The golden suite analyses none
of them.

Each `arch/<arch>.mk` is one row of `FIXTURE_ARCHES` in
`crates/strider-orchestrator/tests/common/fixtures.rs`; `cargo test -p
strider-orchestrator --test golden` fails until the two agree.

`golden/<arch>/<case>.txt` pins what `Strider::analyze` makes of every function
of `out/<arch>/<case>.elf`. The `INTENT` rows in
`crates/strider-orchestrator/tests/golden.rs` must hold before a golden is
compared or written, and `GAPS` names each arch where a dispatch is known not
to resolve. After an intended change, re-bless and review the diff:
`UPDATE_EXPECT=1 cargo test -p strider-orchestrator --test golden`, then
`git diff fixtures/golden`.

## Toolchains

Every arch needs its own cross compiler. A missing one fails the build;
`make ALLOW_MISSING_CC=1` downgrades that to a skip, which is how you build the
single arch you have a toolchain for.

| Arch | Needs |
|---|---|
| `x86`, `x86_kernel` | `i686-linux-gnu-gcc-11` or `i686-linux-gnu-gcc`, else host `gcc` with `gcc-multilib` |
| `x64` | host `gcc` |
| `aarch64` | `gcc-aarch64-linux-gnu` |
| `arm`, `arm_thumb`, `arm_be8` | `gcc-arm-linux-gnueabihf` |
| `mips32be` | `gcc-mips-linux-gnu` |
| `mips32le` | `gcc-mipsel-linux-gnu` |
| `mips64be` | `gcc-mips64-linux-gnuabi64` |
| `mips64le` | `gcc-mips64el-linux-gnuabi64` |
| `ppc32be` | `gcc-powerpc-linux-gnu` |
| `ppc64le` | `gcc-powerpc64le-linux-gnu` |
| `aarch64be`, `ppc32le`, `ppc64be` | `clang`, `lld` |
| `arm_be` | `clang` plus `binutils-arm-linux-gnueabihf` for its `ld` |

On Debian / Ubuntu:

```
sudo apt install clang lld gcc-i686-linux-gnu \
  gcc-aarch64-linux-gnu \
  gcc-arm-linux-gnueabihf binutils-arm-linux-gnueabihf \
  gcc-mips-linux-gnu gcc-mipsel-linux-gnu \
  gcc-mips64-linux-gnuabi64 gcc-mips64el-linux-gnuabi64 \
  gcc-powerpc-linux-gnu gcc-powerpc64le-linux-gnu
```

Not `gcc-multilib`: it conflicts with the aarch64, arm and powerpc64le cross
compilers, so the one line above fails to resolve with it in place.
`gcc-i686-linux-gnu` is what the `x86` arches want anyway. On a
host without that package they fall back to host `gcc -m32`, which
`gcc-multilib` supplies, so install it alone there.

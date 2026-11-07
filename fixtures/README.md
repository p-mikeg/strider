# Fixtures

`out/<arch>/<case>.elf` is built from `cases/<case>.{c,S}` by `make`, one
target per (arch, case). The cases in the Makefile's `OBJ_TWINS` also build an
unlinked `.o` twin, which the reader tests use.

The binaries are Git LFS objects; without `git lfs pull` a clone holds pointer
text where an ELF should be.

Rebuild only when a case or an arch's flags change, selecting it with `ARCH=` /
`CASE=` (see the `Makefile` header). `strider-reader`'s release-only
`read_digest` digests every image under `out/`; re-bless and review:

```
UPDATE_EXPECT=1 cargo test -p strider-reader --release --test read_digest
git diff crates/strider-reader/tests/data/read_digest.txt
```

A changed `.elf` also needs its golden re-blessed (below).

`loader/out/` holds images the ELF reader tests need their own link or flags
for (dynamic GOT slots, IFUNC, TLS, copy relocations, ppc64 local entries,
...). `make -C loader` builds them from `loader/src/`; its Makefile lists them
and their cross compilers. The golden suite analyses none of them.

Each `arch/<arch>.mk` is one row of `FIXTURE_ARCHES` in
`crates/strider-orchestrator/tests/common/fixtures.rs`; `cargo test -p
strider-orchestrator --test golden` fails until the two agree.

`golden/<arch>/<case>.txt` pins what `Strider::analyze` makes of every function
of `out/<arch>/<case>.elf`. The `INTENT` rows in
`crates/strider-orchestrator/tests/golden.rs` must hold before a golden is
compared or written; `GAPS` names each arch where a dispatch is known not to
resolve. After an intended change, re-bless and review:
`UPDATE_EXPECT=1 cargo test -p strider-orchestrator --test golden`, then
`git diff fixtures/golden`.

## Toolchains

Each arch needs its cross compiler; a missing one fails the build unless
`make ALLOW_MISSING_CC=1` skips it.

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

Not `gcc-multilib`, which conflicts with the aarch64, arm and powerpc64le
cross compilers; install it only on a host without `gcc-i686-linux-gnu`, where
the `x86` arches fall back to `gcc -m32`.

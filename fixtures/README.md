# Fixtures

`out/<arch>/<case>.elf` (and a `.o` twin for every C case) is built from
`cases/<case>.{c,S}` by `make`, one target per (arch, case). The binaries are
committed through Git LFS, so a clone without `git-lfs` gets 129-byte pointer
text where an ELF should be; run `git lfs pull` before running the test suite.

Rebuilding is only needed when a case or an arch's flags change. See the header
of `Makefile` for the `ARCH=` / `CASE=` selectors.

## Toolchains

Every arch needs its own cross compiler. A missing one fails the build;
`make ALLOW_MISSING_CC=1` downgrades that to a skip, which is how you build the
single arch you have a toolchain for.

| Arch | Needs |
|---|---|
| `x86`, `x86_kernel` | `i686-linux-gnu-gcc`, else host `gcc` with `gcc-multilib` |
| `x64` | host `gcc` |
| `aarch64` | `gcc-aarch64-linux-gnu` |
| `arm`, `arm_thumb` | `gcc-arm-linux-gnueabihf` |
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
sudo apt install clang lld gcc-multilib \
  gcc-aarch64-linux-gnu \
  gcc-arm-linux-gnueabihf binutils-arm-linux-gnueabihf \
  gcc-mips-linux-gnu gcc-mipsel-linux-gnu \
  gcc-mips64-linux-gnuabi64 gcc-mips64el-linux-gnuabi64 \
  gcc-powerpc-linux-gnu gcc-powerpc64le-linux-gnu
```

`i686-linux-gnu-gcc` is not packaged on current Debian; the `x86` arches fall
back to host `gcc -m32`, which is what `gcc-multilib` supplies.

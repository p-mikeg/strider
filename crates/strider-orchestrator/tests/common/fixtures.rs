//! The committed fixture corpus, `fixtures/out/<dir>/<case>.elf`: one
//! [`FixtureArch`] row per `fixtures/arch/<dir>.mk`, and [`Fixture`], the one
//! way to open an image, list its functions and analyse them.
//!
//! Also included by `#[path]` outside the test crates, so it names nothing
//! under `crate::`.

use std::io::Read as _;
use std::path::PathBuf;

use object::{Object, ObjectSymbol, SymbolKind};
use strider_orchestrator::Strider;
use strider_orchestrator::opt::ReadOnlyMemory;
use strider_reader::ElfFileMemReader;
use strider_target::{BuiltCallingConvention, CallingConvention as Cc, SleighArch as Sa};

/// One fixture directory: the arch it decodes as and the convention its
/// build ABI follows.
pub struct FixtureArch {
    pub dir: &'static str,
    pub sleigh: fn() -> Sa,
    pub cc: fn() -> Cc,
}

/// One row per `fixtures/arch/*.mk`, in `Arch` order.
pub const FIXTURE_ARCHES: &[FixtureArch] = &[
    FixtureArch {
        dir: "x86",
        sleigh: Sa::x86,
        cc: Cc::x86_cdecl,
    },
    // `-mregparm=3`, which leaves no trace in the ELF.
    FixtureArch {
        dir: "x86_kernel",
        sleigh: Sa::x86,
        cc: Cc::x86_linux_kernel,
    },
    FixtureArch {
        dir: "x64",
        sleigh: Sa::x86_64,
        cc: Cc::x86_64_systemv,
    },
    FixtureArch {
        dir: "aarch64",
        sleigh: Sa::aarch64,
        cc: Cc::aarch64_aapcs64,
    },
    FixtureArch {
        dir: "aarch64be",
        sleigh: Sa::aarch64be,
        cc: Cc::aarch64_aapcs64,
    },
    FixtureArch {
        dir: "arm",
        sleigh: Sa::arm,
        cc: Cc::arm_aapcs,
    },
    FixtureArch {
        dir: "arm_be",
        sleigh: Sa::arm_be,
        cc: Cc::arm_aapcs,
    },
    FixtureArch {
        dir: "arm_be8",
        sleigh: Sa::arm_be_kernel,
        cc: Cc::arm_aapcs,
    },
    FixtureArch {
        dir: "arm_thumb",
        sleigh: Sa::arm_thumb,
        cc: Cc::arm_aapcs,
    },
    FixtureArch {
        dir: "mips32le",
        sleigh: Sa::mipsle32,
        cc: Cc::mips_o32,
    },
    FixtureArch {
        dir: "mips32be",
        sleigh: Sa::mipsbe32,
        cc: Cc::mips_o32,
    },
    FixtureArch {
        dir: "mips64le",
        sleigh: Sa::mipsle64,
        cc: Cc::mips_n64,
    },
    FixtureArch {
        dir: "mips64be",
        sleigh: Sa::mipsbe64,
        cc: Cc::mips_n64,
    },
    FixtureArch {
        dir: "ppc32be",
        sleigh: Sa::ppc32be,
        cc: Cc::powerpc_sysv32,
    },
    FixtureArch {
        dir: "ppc32le",
        sleigh: Sa::ppc32le,
        cc: Cc::powerpc_sysv32,
    },
    // clang + lld build ELFv2 for both byte orders.
    FixtureArch {
        dir: "ppc64be",
        sleigh: Sa::ppc64be,
        cc: Cc::powerpc64_elf_v2,
    },
    FixtureArch {
        dir: "ppc64le",
        sleigh: Sa::ppc64le,
        cc: Cc::powerpc64_elf_v2,
    },
];

/// The row for `dir`.
pub fn fixture_arch(dir: &str) -> &'static FixtureArch {
    FIXTURE_ARCHES
        .iter()
        .find(|a| a.dir == dir)
        .unwrap_or_else(|| panic!("no fixture arch {dir:?}"))
}

/// `<workspace>/fixtures`.
pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

const LFS_POINTER: &[u8] = b"version https://git-lfs.github.com/spec/v1";

/// One fixture image.
pub struct Fixture {
    pub arch: &'static FixtureArch,
    /// The file stem, e.g. `arithmetic`.
    pub case: String,
    pub path: PathBuf,
    elf: strider_reader::OwnedElf,
}

impl Fixture {
    /// `fixtures/out/<dir>/<case>.elf`.
    pub fn open(arch: &'static FixtureArch, case: &str) -> Self {
        let path = fixtures_root()
            .join("out")
            .join(arch.dir)
            .join(format!("{case}.elf"));
        Self::at(arch, path)
    }

    /// The image at `path`, analysed as `arch`. A missing file or a Git LFS
    /// pointer panics: a committed fixture is never optional.
    pub fn at(arch: &'static FixtureArch, path: PathBuf) -> Self {
        let mut head = Vec::new();
        std::fs::File::open(&path)
            .and_then(|file| file.take(LFS_POINTER.len() as u64).read_to_end(&mut head))
            .unwrap_or_else(|e| {
                panic!(
                    "{}: {e}; run `make -C fixtures ARCH={}`",
                    path.display(),
                    arch.dir
                )
            });
        assert!(
            head != LFS_POINTER,
            "{} is a Git LFS pointer; run `git lfs pull`",
            path.display()
        );
        let elf = strider_reader::load_elf(&path)
            .unwrap_or_else(|e| panic!("load_elf({}): {e:#}", path.display()));
        let case = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            arch,
            case,
            path,
            elf,
        }
    }

    /// Every `.elf` of `arch`, by case name.
    pub fn all(arch: &'static FixtureArch) -> Vec<Self> {
        let dir = fixtures_root().join("out").join(arch.dir);
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|entry| entry.expect("a directory entry").path())
            .filter(|p| p.extension().is_some_and(|e| e == "elf"))
            .collect();
        paths.sort();
        paths.into_iter().map(|p| Self::at(arch, p)).collect()
    }

    pub fn file(&self) -> object::File<'_> {
        self.elf
            .checked_file()
            .expect("the mapped file is unchanged")
    }

    pub fn elf(&self) -> &strider_reader::OwnedElf {
        &self.elf
    }

    /// Each sized function symbol as `(address, name)`, by address, one per
    /// address (the first name). The address is as the symbol table spells
    /// it: an ARM Thumb entry keeps bit 0, which is its ISA mode.
    pub fn functions(&self) -> Vec<(u64, String)> {
        let mut symbols: Vec<(u64, String)> = self
            .file()
            .symbols()
            .filter(|s| s.kind() == SymbolKind::Text && s.size() > 0)
            .map(|s| (s.address(), s.name().unwrap_or_default().to_owned()))
            .collect();
        symbols.sort();
        symbols.dedup_by_key(|s| s.0);
        symbols
    }

    /// `name`'s address, spelled as in [`Self::functions`].
    pub fn symbol(&self, name: &str) -> u64 {
        self.file()
            .symbol_by_name(name)
            .unwrap_or_else(|| panic!("symbol {name:?} not in {}", self.path.display()))
            .address()
    }

    /// An engine over the image with a second view of it as the optimiser's
    /// ROM, and the arch's convention built against that engine.
    pub fn strider(&self) -> (Strider<ElfFileMemReader>, BuiltCallingConvention) {
        let rom = ElfFileMemReader::new(&self.elf, strider_reader::elf::RelocationMode::Unapplied)
            .expect("rom reader");
        self.strider_over(Box::new(rom))
    }

    /// [`Self::strider`] whose ROM also serves the relocated GOT
    /// (`LoadFilter::ImmutableAndRelocatedGot`), the opt-in a PIC dispatch
    /// whose table base is loaded through the GOT needs.
    pub fn strider_serving_got(&self) -> (Strider<ElfFileMemReader>, BuiltCallingConvention) {
        let rom = ElfFileMemReader::new(
            &self.elf,
            strider_reader::elf::RelocationMode::Applied { got_is_rom: true },
        )
        .expect("rom reader");
        self.strider_over(Box::new(rom))
    }

    fn strider_over(
        &self,
        rom: Box<dyn ReadOnlyMemory>,
    ) -> (Strider<ElfFileMemReader>, BuiltCallingConvention) {
        let arch = (self.arch.sleigh)();
        let mem = ElfFileMemReader::new(&self.elf, strider_reader::elf::RelocationMode::Unapplied)
            .expect("mem reader");
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem).expect("sleigh");
        let strider = Strider::new(arch, sleigh, Some(rom)).expect("Strider::new");
        let cc = (self.arch.cc)()
            .build(strider.sleigh_regs())
            .expect("build cc");
        (strider, cc)
    }
}

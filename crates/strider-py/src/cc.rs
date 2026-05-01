//! `PyCallingConvention` — opaque wrapper over `target::CallingConvention`
//! with one Python classmethod per Rust preset.

use pyo3::prelude::*;
use pyo3::types::PyType;

#[pyclass(name = "CallingConvention", module = "strider", frozen)]
#[derive(Clone)]
pub struct PyCallingConvention {
    pub(crate) inner: target::CallingConvention,
    pub(crate) preset_name: &'static str,
}

#[pymethods]
impl PyCallingConvention {
    #[classmethod]
    fn x86_64_systemv_abi(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::x86_64_systemv_abi(),
            preset_name: "x86_64_systemv_abi",
        }
    }
    #[classmethod]
    fn aarch64_aapcs64(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::aarch64_aapcs64(),
            preset_name: "aarch64_aapcs64",
        }
    }
    #[classmethod]
    fn arm_aapcs(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::arm_aapcs(),
            preset_name: "arm_aapcs",
        }
    }
    #[classmethod]
    fn mips_o32(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::mips_o32(),
            preset_name: "mips_o32",
        }
    }
    #[classmethod]
    fn mips_n64(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::mips_n64(),
            preset_name: "mips_n64",
        }
    }
    #[classmethod]
    fn powerpc_sysv32(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::powerpc_sysv32(),
            preset_name: "powerpc_sysv32",
        }
    }
    #[classmethod]
    fn powerpc64_elf_v1(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::powerpc64_elf_v1(),
            preset_name: "powerpc64_elf_v1",
        }
    }
    #[classmethod]
    fn powerpc64_elf_v2(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::powerpc64_elf_v2(),
            preset_name: "powerpc64_elf_v2",
        }
    }
    #[classmethod]
    fn x86_cdecl(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::x86_cdecl(),
            preset_name: "x86_cdecl",
        }
    }

    // ── Linux kernel + syscall presets ───────────────────────────────────
    //
    // One classmethod per (arch, role) pair, one-to-one with the
    // factory methods on `target::CallingConvention`.  See
    // `docs/superpowers/specs/2026-05-01-linux-kernel-cc-design.md`
    // for the full list and rationale.

    #[classmethod]
    fn x86_linux_kernel(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::x86_linux_kernel(),
            preset_name: "x86_linux_kernel",
        }
    }
    #[classmethod]
    fn x86_64_linux_kernel(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::x86_64_linux_kernel(),
            preset_name: "x86_64_linux_kernel",
        }
    }
    #[classmethod]
    fn aarch64_linux_kernel(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::aarch64_linux_kernel(),
            preset_name: "aarch64_linux_kernel",
        }
    }
    #[classmethod]
    fn arm_linux_kernel(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::arm_linux_kernel(),
            preset_name: "arm_linux_kernel",
        }
    }
    #[classmethod]
    fn mips_linux_kernel_o32(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::mips_linux_kernel_o32(),
            preset_name: "mips_linux_kernel_o32",
        }
    }
    #[classmethod]
    fn mips_linux_kernel_n64(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::mips_linux_kernel_n64(),
            preset_name: "mips_linux_kernel_n64",
        }
    }

    #[classmethod]
    fn x86_linux_syscall(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::x86_linux_syscall(),
            preset_name: "x86_linux_syscall",
        }
    }
    #[classmethod]
    fn x86_64_linux_syscall(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::x86_64_linux_syscall(),
            preset_name: "x86_64_linux_syscall",
        }
    }
    #[classmethod]
    fn aarch64_linux_syscall(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::aarch64_linux_syscall(),
            preset_name: "aarch64_linux_syscall",
        }
    }
    #[classmethod]
    fn arm_linux_syscall(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::arm_linux_syscall(),
            preset_name: "arm_linux_syscall",
        }
    }
    #[classmethod]
    fn mips_linux_syscall_o32(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::mips_linux_syscall_o32(),
            preset_name: "mips_linux_syscall_o32",
        }
    }
    #[classmethod]
    fn mips_linux_syscall_n64(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::mips_linux_syscall_n64(),
            preset_name: "mips_linux_syscall_n64",
        }
    }

    fn name(&self) -> &'static str {
        self.preset_name
    }

    fn __repr__(&self) -> String {
        format!("CallingConvention.{}()", self.preset_name)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCallingConvention>()
}

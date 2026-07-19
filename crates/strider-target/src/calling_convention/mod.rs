use anyhow::anyhow;

use crate::Result;

pub(crate) fn vn_for_name(sleigh_regs: &rsleigh::SleighRegs, name: &str) -> Result<rsleigh::Vn> {
    sleigh_regs
        .name_to_vn(name)
        .ok_or_else(|| anyhow!("unknown sleigh register name {name:?}"))
}

/// Order-preserving; short-circuits on the first unknown name.
pub(crate) fn regs_to_vns(
    sleigh_regs: &rsleigh::SleighRegs,
    reg_names: &[&str],
) -> Result<Vec<rsleigh::Vn>> {
    reg_names
        .iter()
        .map(|&name| vn_for_name(sleigh_regs, name))
        .collect()
}

/// A calling convention as static register-name slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallingConvention {
    stack_ptr_reg_name: &'static str,
    /// In positional order.
    arg_passing_regs: &'static [&'static str],
    /// Excludes the stack pointer.
    callee_saved_regs: &'static [&'static str],
    /// In positional order.
    ret_val_regs: &'static [&'static str],
    /// Float return registers (`q0` on aarch64, `XMM0` on x86_64, `d0` on ARM
    /// AAPCS soft-float, `f0` on MIPS O32).
    ret_val_regs_float: &'static [&'static str],
    /// `None` when the convention passes no arguments on the stack.
    stack_args: Option<StackArgs>,
    /// Net byte change the callee's `ret` inflicts on the caller's SP: the
    /// pointer size (4 / 8) on stack-push ISAs where `ret` pops the return
    /// address, 0 on link-register ISAs where the call never touches SP.
    ret_stack_pop: i64,
    /// Register holding the return address across a call; `None` on
    /// stack-push ISAs (x86, x86_64), where it lives on the stack.
    link_register_reg_name: Option<&'static str>,
    /// `true` if calls under this convention preserve **all** observable
    /// state, memory included.
    ///
    /// `false` for every standard ABI; `true` only for transparent-hook
    /// presets like [`Self::x86_64_all_preserving`] (Linux-kernel
    /// `__fentry__` / `mcount` callbacks).
    preserves_memory: bool,
}

/// A [`CallingConvention`] with its register names resolved to varnodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    /// In positional order.
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    /// Excludes the stack pointer.
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    /// In positional order.
    pub ret_val_regs: Vec<rsleigh::Vn>,
    /// In positional order.
    pub ret_val_regs_float: Vec<rsleigh::Vn>,
    /// Never present in the four register lists above.
    pub stack_vn: rsleigh::Vn,
    /// `None` when the convention passes no arguments on the stack.
    pub stack_args: Option<StackArgs>,
    /// Net byte change the callee's `ret` inflicts on the caller's SP: `8` on
    /// x86_64 (pops the return address), `0` on link-register ISAs.
    pub ret_stack_pop: i64,
    /// `None` on stack-push ISAs (x86, x86_64).
    pub link_register_vn: Option<rsleigh::Vn>,
    /// `true` for zero-side-effect hooks like `__fentry__` / `mcount`.
    pub preserves_memory: bool,
    /// `true` for a `noreturn` callee (`exit`, `abort`, `panic`,
    /// `__stack_chk_fail`), attached to a call TARGET via a per-address CC
    /// override.
    pub no_return: bool,
}

/// The trivial convention for synthetic / mock graphs that have no real
/// target ABI: everything empty, plus a synthetic `stack_vn`.
impl Default for BuiltCallingConvention {
    fn default() -> Self {
        Self {
            arg_passing_regs: Vec::new(),
            callee_saved_regs: Vec::new(),
            ret_val_regs: Vec::new(),
            ret_val_regs_float: Vec::new(),
            stack_vn: rsleigh::Vn {
                addr_off: SYNTHETIC_STACK_VN_OFFSET,
                addr_space: rsleigh::VnSpace::REGISTER,
                size: 8,
            },
            stack_args: None,
            ret_stack_pop: 0,
            link_register_vn: None,
            preserves_memory: false,
            no_return: false,
        }
    }
}

/// Far outside any real architecture's register file.
pub(crate) const SYNTHETIC_STACK_VN_OFFSET: u64 = 0xFFFF_FFFF_FFFF_0000;

/// First varnode of `a` that is also in `b`.
fn first_in_both<'a>(a: &'a [rsleigh::Vn], b: &[rsleigh::Vn]) -> Option<&'a rsleigh::Vn> {
    a.iter().find(|vn| b.contains(vn))
}

fn first_dup(list: &[rsleigh::Vn]) -> Option<&rsleigh::Vn> {
    list.iter()
        .enumerate()
        .find(|(i, vn)| list[i + 1..].contains(vn))
        .map(|(_, vn)| vn)
}

/// Named-field inputs to [`BuiltCallingConvention::try_new`], one per
/// identically named [`BuiltCallingConvention`] field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltCallingConventionParts {
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    /// Excludes the stack pointer.
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    pub ret_val_regs: Vec<rsleigh::Vn>,
    pub ret_val_regs_float: Vec<rsleigh::Vn>,
    pub stack_vn: rsleigh::Vn,
    pub stack_args: Option<StackArgs>,
    pub ret_stack_pop: i64,
    pub link_register_vn: Option<rsleigh::Vn>,
    pub preserves_memory: bool,
}

impl BuiltCallingConvention {
    /// Splits this convention's clobbered registers over `tracked_vns` into
    /// the ret-val group and the non-ret caller-clobbered group.  These are
    /// exactly the two output groups a `Call` emits past `[Control, Memory]`.
    ///
    /// A register is *clobbered* iff it is neither callee-saved nor the stack
    /// pointer.  `ret_vals` holds the tracked clobbered containers of
    /// `ret_val_regs` then `ret_val_regs_float` in ABI order; `clobbers` holds
    /// every other clobbered REGISTER/UNIQUE entry in `tracked_vns` order.
    pub fn ret_and_clobber_vns(
        &self,
        tracked_vns: &[rsleigh::Vn],
        container_of: impl Fn(&rsleigh::Vn) -> rsleigh::Vn,
    ) -> (Vec<rsleigh::Vn>, Vec<rsleigh::Vn>) {
        let stack_vn = self.stack_vn;
        // Register lists are tiny (1-4 regs): linear `contains` beats hashing
        // and needs no extra dependency.
        let callee_saved: Vec<rsleigh::Vn> =
            self.callee_saved_regs.iter().map(&container_of).collect();
        let is_clobbered = |v: &rsleigh::Vn| !callee_saved.contains(v) && *v != stack_vn;

        let ret_containers: Vec<rsleigh::Vn> = self
            .ret_val_regs
            .iter()
            .chain(self.ret_val_regs_float.iter())
            .map(&container_of)
            .collect();
        let ret_vals: Vec<rsleigh::Vn> = ret_containers
            .iter()
            .copied()
            .filter(|c| tracked_vns.contains(c) && is_clobbered(c))
            .collect();
        let clobbers: Vec<rsleigh::Vn> = tracked_vns
            .iter()
            .copied()
            .filter(|v| {
                matches!(
                    v.addr_space,
                    rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE
                ) && is_clobbered(v)
                    && !ret_containers.contains(v)
            })
            .collect();
        (ret_vals, clobbers)
    }

    /// Validating constructor.  Checks the canonical ABI invariants:
    ///
    /// - `callee_saved_regs` is disjoint from `arg_passing_regs`,
    ///   `ret_val_regs`, and `ret_val_regs_float`
    /// - `ret_val_regs` and `ret_val_regs_float` are disjoint
    /// - `stack_vn` is in none of the four register lists
    /// - no duplicates within any single list
    /// - a `Some` `link_register_vn` is also in `callee_saved_regs` (the
    ///   deliberate link-register tradeoff)
    /// - `ret_stack_pop >= 0`
    /// - a `Some` `stack_args` has `increment > 0` and `base_offset >= 0`
    ///
    /// # Errors
    ///
    /// Returns the first violation found, naming the offending varnodes.
    pub fn try_new(parts: BuiltCallingConventionParts) -> std::result::Result<Self, anyhow::Error> {
        let BuiltCallingConventionParts {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_vn,
            stack_args,
            ret_stack_pop,
            link_register_vn,
            preserves_memory,
        } = parts;
        if let Some(vn) = first_in_both(&arg_passing_regs, &callee_saved_regs) {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: varnode {:?} appears in both \
                 arg_passing_regs and callee_saved_regs (a single varnode \
                 cannot be both caller-supplied and callee-preserved)",
                vn,
            ));
        }
        // The callee writes ret-val regs to deliver results, so they cannot
        // also be required-preserved.
        for vn in ret_val_regs.iter().chain(ret_val_regs_float.iter()) {
            if callee_saved_regs.contains(vn) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: varnode {:?} appears in both \
                     ret_val_regs/ret_val_regs_float and callee_saved_regs",
                    vn,
                ));
            }
        }
        // Integer and float returns are physically distinct register files on
        // every supported arch.  arg-vs-ret overlap is deliberately NOT
        // checked: x86_64 SysV RDX is legitimately 3rd arg and 2nd int return.
        if let Some(vn) = first_in_both(&ret_val_regs, &ret_val_regs_float) {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: varnode {:?} appears in both \
                 ret_val_regs and ret_val_regs_float (integer and float \
                 return registers are physically distinct)",
                vn,
            ));
        }
        for (list_name, list) in [
            ("arg_passing_regs", &arg_passing_regs),
            ("callee_saved_regs", &callee_saved_regs),
            ("ret_val_regs", &ret_val_regs),
            ("ret_val_regs_float", &ret_val_regs_float),
        ] {
            if list.contains(&stack_vn) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: stack_vn {:?} appears in {} \
                     (the SP is implicit and must not be in any reg list)",
                    stack_vn,
                    list_name,
                ));
            }
            if let Some(vn) = first_dup(list) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: duplicate varnode {:?} in {}",
                    vn,
                    list_name,
                ));
            }
        }
        if let Some(lr) = link_register_vn
            && !callee_saved_regs.contains(&lr)
        {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: link_register_vn {:?} must also \
                 be present in callee_saved_regs (CLAUDE.md deliberate \
                 tradeoff so InitialVar(lr) propagates through call sites)",
                lr,
            ));
        }
        // A negative pop would mean the callee's `ret` grew the stack, which
        // no real ABI does.
        if ret_stack_pop < 0 {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: ret_stack_pop must be >= 0, got {}",
                ret_stack_pop,
            ));
        }
        if let Some(sa) = stack_args {
            if sa.increment <= 0 {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: stack-arg increment must be > 0, got {}",
                    sa.increment,
                ));
            }
            // Rejected at the construction boundary so `index_of` / `slot_of`
            // can subtract `base_offset` overflow-free for any
            // `offset >= base_offset`, even a garbage decoded one.
            if sa.base_offset < 0 {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: stack-arg base_offset must be >= 0, got {}",
                    sa.base_offset,
                ));
            }
        }
        Ok(Self {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_vn,
            stack_args,
            ret_stack_pop,
            link_register_vn,
            preserves_memory,
            no_return: false,
        })
    }
}

/// Stack-arg layout: an unbounded series where the N-th stack argument sits
/// at `base_offset + N * increment` bytes from the call-time SP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StackArgs {
    /// Byte offset from call-time SP of the first stack-passed argument.
    pub base_offset: i128,
    /// Byte stride between slots (the ABI word size); always `> 0`.
    pub increment: i128,
}

impl StackArgs {
    /// Byte offset from call-time SP of the `n`-th stack argument.
    ///
    /// Saturates rather than overflowing on a runaway index.
    #[must_use]
    pub fn offset_of(&self, n: usize) -> i128 {
        self.base_offset
            .saturating_add((n as i128).saturating_mul(self.increment))
    }

    /// The stack-arg index whose slot fully contains a `size`-byte access at
    /// `offset` from call-time SP; `None` below `base_offset` or when the
    /// access straddles a slot boundary.  A zero-size access trivially fits.
    ///
    /// Offsets come from binary content, so `offset + size` is a checked add:
    /// a garbage offset degrades to `None` instead of panicking in debug and
    /// wrapping in release.
    #[cfg(test)]
    #[must_use]
    pub fn index_of(&self, offset: i128, size: i128) -> Option<usize> {
        // `try_new` enforces `increment > 0`, but a directly-constructed zero
        // should surface as an assertion, not a divide-by-zero.
        debug_assert!(
            self.increment > 0,
            "StackArgs::index_of requires increment > 0"
        );
        if offset < self.base_offset {
            return None;
        }
        let rel = offset - self.base_offset;
        let idx = (rel / self.increment) as usize;
        // `idx * increment <= rel`, so `slot_start <= offset` and cannot
        // overflow; the slot end and access end can, so both are checked.
        let slot_start = self.base_offset + (idx as i128) * self.increment;
        let slot_end = slot_start.checked_add(self.increment)?;
        let access_end = offset.checked_add(size)?;
        (access_end <= slot_end).then_some(idx)
    }

    /// The slot containing the *start byte* of an access at `offset`:
    /// `floor((offset - base_offset) / increment)`, or `None` below
    /// `base_offset`.
    ///
    /// The access size is not bounded at all: an argument wider than a slot
    /// (a 32-bit-ABI `double`, an x86-64 `long double`) is attributed to the
    /// slot its first byte lands in, as is a sub-field read landing mid-slot.
    ///
    /// The result is a byte-position slot index, NOT an argument ordinal: a
    /// wider-than-slot argument spans several slots but advances the ordinal
    /// by one.
    #[must_use]
    pub fn slot_of(&self, offset: i128) -> Option<usize> {
        debug_assert!(
            self.increment > 0,
            "StackArgs::slot_of requires increment > 0"
        );
        if offset < self.base_offset {
            return None;
        }
        Some(((offset - self.base_offset) / self.increment) as usize)
    }

    /// Consecutive slots a `size`-byte argument occupies:
    /// `ceil(max(size, 1) / increment)`, always `>= 1`.
    ///
    /// Saturating, so a garbage decoded `size` from arbitrary lifted
    /// arithmetic degrades to a large-but-finite span instead of wrapping.
    #[must_use]
    pub fn slots_spanned(&self, size: i128) -> usize {
        debug_assert!(
            self.increment > 0,
            "StackArgs::slots_spanned requires increment > 0"
        );
        let size = size.max(1);
        // ceil(size / increment), saturating so a pathological `size` cannot
        // overflow the add.
        let numerator = size.saturating_add(self.increment - 1);
        (numerator / self.increment) as usize
    }
}

/// One row of `CC_PRESETS`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CcPresetRow {
    /// The Rust factory name, also the Python classmethod name.
    name: &'static str,
    cc: CallingConvention,
}

/// Base for `x86_cdecl` and `x86_linux_kernel`, which differ only in
/// `arg_passing_regs`.
const X86_CDECL_BASE: CallingConvention = CallingConvention {
    stack_ptr_reg_name: "ESP",
    arg_passing_regs: &[],
    callee_saved_regs: &["EBX", "ESI", "EDI", "EBP"],
    ret_val_regs: &["EAX", "EDX"],
    // cdecl returns floats in ST0, the x87 80-bit top-of-stack: GCC's i686
    // default lowers floats through x87 even when arithmetic uses SSE.  XMM0
    // covers SSE-default builds (`-mfpmath=sse2`).  When a function
    // references neither, `FunctionBuilder::new`'s upgrade-to-container logic
    // skips them harmlessly.
    ret_val_regs_float: &["ST0", "XMM0"],
    // +4 because `call` pushes a 4-byte return address: SP-at-call points at
    // that address, so arg 0 sits one slot above it.
    stack_args: Some(StackArgs {
        base_offset: 4,
        increment: 4,
    }),
    ret_stack_pop: 4,
    // `call` pushes the return address; x86 has no link register.
    link_register_reg_name: None,
    preserves_memory: false,
};

/// Base for `powerpc64_elf_v1` and `powerpc64_elf_v2`, which differ only in
/// `stack_args.base_offset` (48-byte ELFv1 linkage area vs 32-byte ELFv2).
const POWERPC64_ELF_BASE: CallingConvention = CallingConvention {
    stack_ptr_reg_name: "r1",
    arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
    callee_saved_regs: &[
        "r2", "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23", "r24", "r25",
        "r26", "r27", "r28", "r29", "r30", "r31",
        // PPC64 ELFv1 3.4 marks LR volatile/caller-saved.  Listing it
        // callee-saved anyway is the deliberate link-register tradeoff (same
        // as `powerpc_sysv32`): it makes `InitialVar(lr)` propagate through
        // call sites, so the indirect-branch resolver's `LinkRegister` arm
        // fires for functions returning via the entry LR.
        "LR",
    ],
    ret_val_regs: &["r3", "r4"],
    ret_val_regs_float: &["f1"],
    stack_args: Some(StackArgs {
        base_offset: 48,
        increment: 8,
    }),
    ret_stack_pop: 0,
    link_register_reg_name: Some("LR"),
    preserves_memory: false,
};

/// Base for `mips_o32` and `mips_n64`, which differ in `arg_passing_regs`
/// and `stack_args`.
const MIPS_O32_BASE: CallingConvention = CallingConvention {
    stack_ptr_reg_name: "sp",
    arg_passing_regs: &["a0", "a1", "a2", "a3"],
    callee_saved_regs: &[
        "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra",
    ],
    ret_val_regs: &["v0", "v1"],
    // Single-precision; doubles use the f0/f1 pair.  Harmless on soft-float
    // builds, where these are simply unused.
    ret_val_regs_float: &["f0", "f2"],
    stack_args: Some(StackArgs {
        base_offset: 16,
        increment: 4,
    }),
    ret_stack_pop: 0,
    // `jal`/`jalr` writes the return address to `$ra` (`$31`); Sleigh's mips32
    // register table names it lowercase `ra`.
    link_register_reg_name: Some("ra"),
    preserves_memory: false,
};

/// Every supported calling-convention preset.
pub(crate) static CC_PRESETS: &[CcPresetRow] = &[
    // x86-64 System V.  RSP is not listed callee-saved: `ret` pops the return
    // address, so the caller observes SP shifted by `ret_stack_pop`.
    CcPresetRow {
        name: "x86_64_systemv",
        cc: CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: &["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
            callee_saved_regs: &["RBX", "RBP", "R12", "R13", "R14", "R15"],
            ret_val_regs: &["RAX", "RDX"],
            ret_val_regs_float: &["XMM0", "XMM1"],
            // +8 because `call` pushes an 8-byte return address: SP-at-call
            // points at it, so the first stack arg (arg 7) is one slot above.
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 8,
            }),
            ret_stack_pop: 8,
            // `call` pushes the return address; x86-64 has no link register.
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },
    // "All-preserving" x86_64: every userland caller-clobbered register listed
    // callee-saved, for sites like Linux-kernel `__fentry__` / `mcount`
    // callbacks that preserve all caller state.
    CcPresetRow {
        name: "x86_64_all_preserving",
        cc: CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: &[],
            callee_saved_regs: &[
                "RAX", "RBX", "RCX", "RDX", "RSI", "RDI", "RBP", "R8", "R9", "R10", "R11", "R12",
                "R13", "R14", "R15", "XMM0", "XMM1", "XMM2", "XMM3", "XMM4", "XMM5", "XMM6",
                "XMM7", "XMM8", "XMM9", "XMM10", "XMM11", "XMM12", "XMM13", "XMM14", "XMM15",
            ],
            ret_val_regs: &[],
            ret_val_regs_float: &[],
            stack_args: None,
            // Still a normal `call`/`ret`: only the register set is
            // preserved, not the stack mechanics, so SP shifts by 8 exactly
            // as in `x86_64_systemv`.
            ret_stack_pop: 8,
            link_register_reg_name: None,
            preserves_memory: true,
        },
    },
    // AArch64 AAPCS64.  `ret_stack_pop` is 0 because `bl` writes the return
    // address to `lr` rather than pushing it.  Register conventions are
    // byte-order independent, so this pairs with both `aarch64` and
    // `aarch64be`.
    CcPresetRow {
        name: "aarch64_aapcs64",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            callee_saved_regs: &[
                "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30",
            ],
            ret_val_regs: &["x0", "x1"],
            // The ABI-correct 16-byte SIMD regs, containing the s0/d0/q0
            // sub-registers.
            ret_val_regs_float: &["q0", "q1"],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ret_stack_pop: 0,
            // `lr` is an alias for `x30`, and Sleigh's aarch64 table only
            // registers `x30`.
            link_register_reg_name: Some("x30"),
            preserves_memory: false,
        },
    },
    // ARM 32-bit AAPCS.  `bl` stores the return address in `lr` rather than
    // pushing it, so the first stack arg sits at SP+0 and `ret_stack_pop` is
    // 0.  Byte-order independent: pairs with `arm`, `arm_be`, and
    // `arm_thumb`.  r0/r1 double as the pair for 64-bit returns.
    CcPresetRow {
        name: "arm_aapcs",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["r0", "r1", "r2", "r3"],
            callee_saved_regs: &["r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "lr"],
            ret_val_regs: &["r0", "r1"],
            // 8-byte VFP regs, also accessed as 4-byte s0/s1.  Under
            // `-mfloat-abi=soft` the float result flows through r0/r1 instead
            // and these are simply unused.
            ret_val_regs_float: &["d0", "d1"],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
            // `bl` writes the return address to `lr` (= `r14`); Sleigh
            // registers it lowercase.
            link_register_reg_name: Some("lr"),
            preserves_memory: false,
        },
    },
    // MIPS O32, byte-order independent: pairs with both `mipsle32` and
    // `mipsbe32`.  `ret_stack_pop` is 0 because `jal`/`jalr` writes the return
    // address to `$ra` rather than pushing it.  Offsets 0..16 are MIPS's
    // reserved shadow space for the four register args, so positional stack
    // args start at 16.
    //
    // Sleigh's MIPS spec uses lowercase names and calls the frame pointer
    // `s8`; `fp` does not resolve in its register table.
    CcPresetRow {
        name: "mips_o32",
        cc: MIPS_O32_BASE,
    },
    // MIPS N64 (`mips64-linux-gnuabi64-gcc`, LE and BE).  Extends O32's 4
    // register args to 8 (`$4`..`$11`) and drops the shadow space, so stack
    // args start at SP+0.  Sleigh's `mips64` spec uses the older naming where
    // `$8`..`$11` are `t0`..`t3`, so they are listed under those names.
    CcPresetRow {
        name: "mips_n64",
        cc: CallingConvention {
            arg_passing_regs: &["a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3"],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ..MIPS_O32_BASE
        },
    },
    // PowerPC 32-bit System V, byte-order independent (`powerpc-linux-gnu-gcc`
    // with either `-mbig-endian` or `-mlittle-endian`).  `r1` is the SP by
    // PowerPC convention.  Stack args start at 8: a 4-byte back-chain plus a
    // 4-byte LR save.  r3/r4 double as the pair for 64-bit returns.
    CcPresetRow {
        name: "powerpc_sysv32",
        cc: CallingConvention {
            stack_ptr_reg_name: "r1",
            arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
            callee_saved_regs: &[
                "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23", "r24", "r25",
                "r26", "r27", "r28", "r29", "r30", "r31", "LR",
            ],
            ret_val_regs: &["r3", "r4"],
            ret_val_regs_float: &["f1"],
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 4,
            }),
            ret_stack_pop: 0,
            // `bl` writes the return address to the `LR` SPR; Sleigh's PPC
            // table names it uppercase.
            link_register_reg_name: Some("LR"),
            preserves_memory: false,
        },
    },
    // PowerPC 64-bit ELFv1, BE (`powerpc64-linux-gnu-gcc`).  Stack args start
    // at 48, the ELFv1 linkage area size.
    //
    // ELFv1 has function descriptors: an external function symbol resolves to
    // a 3-pointer descriptor (entry, TOC, env), not the entry itself.  Only
    // the register-level ABI is modelled here.
    CcPresetRow {
        name: "powerpc64_elf_v1",
        cc: POWERPC64_ELF_BASE,
    },
    // PowerPC 64-bit ELFv2, LE (`powerpc64le-linux-gnu-gcc`).  Drops function
    // descriptors, so symbols point straight at the entry point, and shrinks
    // the linkage area from 48 to 32 bytes.  Register usage matches ELFv1.
    CcPresetRow {
        name: "powerpc64_elf_v2",
        cc: CallingConvention {
            stack_args: Some(StackArgs {
                base_offset: 32,
                increment: 8,
            }),
            ..POWERPC64_ELF_BASE
        },
    },
    // x86 cdecl: all arguments on the stack, so `arg_passing_regs` is empty.
    // ESP is not listed callee-saved: `ret` pops the 4-byte return address, so
    // the caller observes SP shifted by `ret_stack_pop`.
    CcPresetRow {
        name: "x86_cdecl",
        cc: X86_CDECL_BASE,
    },
    // The only kernel-internal preset.  Every other supported arch's kernel CC
    // is byte-identical to its userland preset, so callers pick that directly
    // rather than a redundant alias.  Syscall ABIs are not calling conventions
    // at all: the `syscall` / `int 0x80` / `svc` traps lift to `CallOther`,
    // whose register footprint lives in `call_other_abi`.
    //
    // x86 32-bit `-mregparm=3`: first three integer args in EAX, EDX, ECX, the
    // rest on the stack at the same cdecl offsets.
    CcPresetRow {
        name: "x86_linux_kernel",
        cc: CallingConvention {
            arg_passing_regs: &["EAX", "EDX", "ECX"],
            ..X86_CDECL_BASE
        },
    },
];

pub(crate) fn lookup_preset(name: &str) -> Option<&'static CcPresetRow> {
    CC_PRESETS.iter().find(|row| row.name == name)
}

/// Panics if no row matches `name`.
fn cc_from_table(name: &'static str) -> CallingConvention {
    lookup_preset(name)
        .unwrap_or_else(|| panic!("calling-convention preset not registered: {name}"))
        .cc
}

/// Emits a named factory wrapper around [`cc_from_table`], with `$desc` as
/// the first rustdoc paragraph.
macro_rules! cc_factory {
    ($name:ident, $desc:expr) => {
        #[doc = concat!($desc, "  See `CC_PRESETS` for the full field table.")]
        pub fn $name() -> CallingConvention {
            cc_from_table(stringify!($name))
        }
    };
}

impl CallingConvention {
    cc_factory!(
        x86_64_systemv,
        "Returns the x86-64 System V ABI calling convention."
    );
    cc_factory!(
        x86_64_all_preserving,
        "\"All-preserving\" x86_64 calling convention: every userland \
         caller-clobbered register is listed as callee-saved.  Used for \
         sites like Linux-kernel `__fentry__` / `mcount` callbacks that \
         preserve all caller state.  Pair with the per-address override \
         map on [`crate::CallingConvention`] consumers (e.g. \
         `strider_lift::LiftOptions::per_address_ccs`) so the override applies only \
         to specific Call sites; the function-default CC stays SystemV."
    );
    cc_factory!(
        aarch64_aapcs64,
        "Returns the AArch64 AAPCS64 calling convention."
    );
    cc_factory!(
        arm_aapcs,
        "Returns the ARM 32-bit AAPCS calling convention."
    );
    cc_factory!(mips_o32, "Returns the MIPS O32 calling convention.");
    cc_factory!(mips_n64, "Returns the MIPS N64 calling convention.");
    cc_factory!(
        powerpc_sysv32,
        "Returns the PowerPC 32-bit System V ABI calling convention."
    );
    cc_factory!(
        powerpc64_elf_v1,
        "Returns the PowerPC 64-bit ELFv1 calling convention."
    );
    cc_factory!(
        powerpc64_elf_v2,
        "Returns the PowerPC 64-bit ELFv2 calling convention."
    );
    cc_factory!(x86_cdecl, "Returns the x86 cdecl calling convention.");

    /// Resolves every register name in this convention against `sleigh_regs`.
    ///
    /// # Errors
    ///
    /// Short-circuits on the first name (the stack pointer included) that
    /// does not resolve.
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        let arg_passing_regs = regs_to_vns(sleigh_regs, self.arg_passing_regs)?;
        let callee_saved_regs = regs_to_vns(sleigh_regs, self.callee_saved_regs)?;
        let ret_val_regs = regs_to_vns(sleigh_regs, self.ret_val_regs)?;
        let ret_val_regs_float = regs_to_vns(sleigh_regs, self.ret_val_regs_float)?;
        let stack_vn = vn_for_name(sleigh_regs, self.stack_ptr_reg_name)?;
        let link_register_vn = self
            .link_register_reg_name
            .map(|name| vn_for_name(sleigh_regs, name))
            .transpose()?;
        BuiltCallingConvention::try_new(BuiltCallingConventionParts {
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_vn,
            stack_args: self.stack_args,
            ret_stack_pop: self.ret_stack_pop,
            link_register_vn,
            preserves_memory: self.preserves_memory,
        })
    }
}

impl CallingConvention {
    cc_factory!(
        x86_linux_kernel,
        "Returns the Linux kernel-internal CC for x86 32-bit (`-mregparm=3`)."
    );
}

#[cfg(test)]
mod tests;

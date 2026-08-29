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
    /// Float / vector argument registers, in positional order, from a register
    /// file `arg_passing_regs` never names (`XMM0..7` on x86-64 SysV, `q0..7`
    /// on AAPCS64, `d0..7` on AAPCS-VFP, `$f12`/`$f14` on MIPS O32, `f1..13` on
    /// PowerPC64 ELF and `f1..8` on 32-bit SysV PowerPC).  Empty where floats
    /// are stack-passed (i386 cdecl).
    arg_passing_regs_float: &'static [&'static str],
    /// Excludes the stack pointer.
    callee_saved_regs: &'static [&'static str],
    /// In positional order.
    ret_val_regs: &'static [&'static str],
    /// Float return registers (`q0` on aarch64, `XMM0` on x86_64, `d0` on ARM
    /// AAPCS hard-float (VFP), `f0` on MIPS O32).
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
    /// `true` if calls under this convention preserve memory. `false` for
    /// every standard ABI; set by [`Self::preserves_all`] for transparent
    /// hooks (Linux-kernel `__fentry__` / `mcount`).
    preserves_memory: bool,
    /// `true` if a call clobbers NO register (every register callee-saved).
    /// Set by [`Self::preserves_all`] / [`Self::preserves_regs`]; false for
    /// every real ABI.
    preserves_all_registers: bool,
}

/// A [`CallingConvention`] with its register names resolved to varnodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    /// In positional order.
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    /// Float / vector argument registers, in positional order, from a register
    /// file `arg_passing_regs` never names.  Legitimately overlaps
    /// `ret_val_regs_float`: `XMM0` / `d0` / `f1` are both.
    pub arg_passing_regs_float: Vec<rsleigh::Vn>,
    /// Excludes the stack pointer.
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    /// In positional order.
    pub ret_val_regs: Vec<rsleigh::Vn>,
    /// In positional order.
    pub ret_val_regs_float: Vec<rsleigh::Vn>,
    /// Never present in the five register lists above.
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
    /// `true` if a call clobbers NO register (every register callee-saved),
    /// as for `__fentry__` / `mcount`.
    pub preserves_all_registers: bool,
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
            arg_passing_regs_float: Vec::new(),
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
            preserves_all_registers: false,
            no_return: false,
        }
    }
}

/// Far outside any real architecture's register file.
pub(crate) const SYNTHETIC_STACK_VN_OFFSET: u64 = 0xFFFF_FFFF_FFFF_0000;

fn first_in_both<'a>(a: &'a [rsleigh::Vn], b: &[rsleigh::Vn]) -> Option<&'a rsleigh::Vn> {
    a.iter().find(|vn| b.contains(vn))
}

fn first_dup(list: &[rsleigh::Vn]) -> Option<&rsleigh::Vn> {
    list.iter()
        .enumerate()
        .find(|(i, vn)| list[i + 1..].contains(vn))
        .map(|(_, vn)| vn)
}

impl BuiltCallingConvention {
    /// True when `callee_saved_regs` covers every byte of `vn`.
    ///
    /// Byte coverage rather than membership, because `vn` is a tracked
    /// container that may be wider than the registers the ABI names: ARM's
    /// `q4` is `d8`|`d9` and AAPCS 5.1.2.1 preserves both, while AArch64's
    /// `q8` holds `d8` plus 64 bits AAPCS64 6.1.2 lets a callee trash.
    fn preserves_all_bytes_of(&self, vn: &rsleigh::Vn) -> bool {
        // Exact, as `vn_container::end_of` is: saturating at `u64::MAX` would
        // shorten the range and report a partly-clobbered container preserved.
        let end = u128::from(vn.addr_off) + u128::from(vn.size);
        let mut covered_to = u128::from(vn.addr_off);
        while covered_to < end {
            let Some(reach) = self
                .callee_saved_regs
                .iter()
                .filter(|s| s.addr_space == vn.addr_space && u128::from(s.addr_off) <= covered_to)
                .map(|s| u128::from(s.addr_off) + u128::from(s.size))
                .filter(|&s_end| s_end > covered_to)
                .max()
            else {
                return false;
            };
            covered_to = reach;
        }
        true
    }

    /// Splits this convention's clobbered registers over `tracked_vns` into
    /// the ret-val group and the non-ret caller-clobbered group.  These are
    /// exactly the two output groups a `Call` emits past `[Control, Memory]`.
    ///
    /// A register is *clobbered* iff it is not the stack pointer and
    /// `callee_saved_regs` leaves at least one of its bytes free.  `ret_vals`
    /// holds the tracked clobbered containers of `ret_val_regs` then
    /// `ret_val_regs_float` in ABI order; `clobbers` holds every other
    /// clobbered REGISTER/UNIQUE entry in `tracked_vns` order.
    pub fn ret_and_clobber_vns(
        &self,
        tracked_vns: &[rsleigh::Vn],
        container_of: impl Fn(&rsleigh::Vn) -> rsleigh::Vn,
    ) -> (Vec<rsleigh::Vn>, Vec<rsleigh::Vn>) {
        let stack_vn = self.stack_vn;
        let is_clobbered = |v: &rsleigh::Vn| {
            !self.preserves_all_registers && !self.preserves_all_bytes_of(v) && *v != stack_vn
        };

        let ret_containers: Vec<rsleigh::Vn> = self
            .ret_val_regs
            .iter()
            .chain(self.ret_val_regs_float.iter())
            .map(&container_of)
            .collect();
        // Aliased ABI return registers share a container (ARM `d0` and `d1`
        // both live inside `q0`), and a `Call` output varnode must be unique,
        // so the group is deduplicated.
        let mut ret_vals: Vec<rsleigh::Vn> = Vec::new();
        for c in ret_containers.iter().copied() {
            if tracked_vns.contains(&c) && is_clobbered(&c) && !ret_vals.contains(&c) {
                ret_vals.push(c);
            }
        }
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

    /// The varnode carrying each float argument, by ABI POSITION: slot `j` is
    /// the j-th float argument register's tracked container, or `None` when
    /// the function names nothing containing it and it therefore has no SSA
    /// slot to read.  An untracked register leaves a gap rather than shifting
    /// the registers after it down one index.
    ///
    /// A container SHARED by several float argument registers yields the
    /// registers themselves instead, one slice each: AAPCS-VFP `d0` and `d1`
    /// both sit in `q0`, and returning `q0` twice would make one argument out
    /// of two.  A container holding a single argument stays whole, which is
    /// what carries an o32 MIPS `double` passed in the `$f12`/`$f13` pair the
    /// ABI names by its low half.
    pub fn float_arg_slots(
        &self,
        tracked_vns: &[rsleigh::Vn],
        container_of: impl Fn(&rsleigh::Vn) -> rsleigh::Vn,
    ) -> Vec<Option<rsleigh::Vn>> {
        let containers: Vec<rsleigh::Vn> = self
            .arg_passing_regs_float
            .iter()
            .map(container_of)
            .collect();
        containers
            .iter()
            .zip(self.arg_passing_regs_float.iter())
            .map(|(container, reg)| {
                if !tracked_vns.contains(container) {
                    return None;
                }
                let shared = containers.iter().filter(|c| *c == container).count() > 1;
                Some(if shared { *reg } else { *container })
            })
            .collect()
    }

    /// The Vn-resolved [`CallingConvention::preserves_all`].
    pub fn preserves_all(self) -> Self {
        Self {
            arg_passing_regs: Vec::new(),
            arg_passing_regs_float: Vec::new(),
            ret_val_regs: Vec::new(),
            ret_val_regs_float: Vec::new(),
            stack_args: None,
            preserves_memory: true,
            preserves_all_registers: true,
            ..self
        }
    }

    /// The Vn-resolved [`CallingConvention::preserves_regs`].
    pub fn preserves_regs(self) -> Self {
        Self {
            ret_val_regs: Vec::new(),
            ret_val_regs_float: Vec::new(),
            preserves_memory: false,
            preserves_all_registers: true,
            ..self
        }
    }

    /// Checks the canonical ABI invariants.  Advisory: every field is `pub`
    /// and [`Default`] builds one directly, so a convention reaching the
    /// lifter has not necessarily been through here.
    ///
    /// - `callee_saved_regs` is disjoint from `arg_passing_regs`,
    ///   `arg_passing_regs_float`, `ret_val_regs`, and `ret_val_regs_float`
    /// - `ret_val_regs` and `ret_val_regs_float` are disjoint
    /// - `stack_vn` is in none of the five register lists
    /// - no duplicates within any single list
    /// - a `Some` `link_register_vn` is also in `callee_saved_regs` (the
    ///   deliberate link-register tradeoff)
    /// - `ret_stack_pop >= 0`
    /// - a `Some` `stack_args` has `increment > 0` and `base_offset >= 0`
    /// - `preserves_all_registers` is not combined with a populated
    ///   `ret_val_regs` / `ret_val_regs_float`
    ///
    /// # Errors
    ///
    /// Returns the first violation found, naming the offending varnodes.
    pub fn validate(&self) -> Result<()> {
        // `is_clobbered` short-circuits on this flag, so it silently wins over
        // a populated return list and the `Call` emits no ret-val output.
        if self.preserves_all_registers
            && !(self.ret_val_regs.is_empty() && self.ret_val_regs_float.is_empty())
        {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: preserves_all_registers is set alongside \
                 {} return register(s); a convention that clobbers nothing cannot \
                 deliver a result",
                self.ret_val_regs.len() + self.ret_val_regs_float.len(),
            ));
        }
        for (list_name, list) in [
            ("arg_passing_regs", &self.arg_passing_regs),
            ("arg_passing_regs_float", &self.arg_passing_regs_float),
        ] {
            if let Some(vn) = first_in_both(list, &self.callee_saved_regs) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: varnode {vn:?} appears in both \
                     {list_name} and callee_saved_regs (a single varnode cannot be \
                     both caller-supplied and callee-preserved)",
                ));
            }
        }
        // The callee writes ret-val regs to deliver results, so they cannot
        // also be required-preserved.
        for vn in self
            .ret_val_regs
            .iter()
            .chain(self.ret_val_regs_float.iter())
        {
            if self.callee_saved_regs.contains(vn) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: varnode {vn:?} appears in both \
                     ret_val_regs/ret_val_regs_float and callee_saved_regs",
                ));
            }
        }
        // Integer and float returns are physically distinct register files on
        // every supported arch.  An argument register may legitimately also be
        // a return register: x86_64 SysV RDX is 3rd arg and 2nd int return.
        if let Some(vn) = first_in_both(&self.ret_val_regs, &self.ret_val_regs_float) {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: varnode {vn:?} appears in both \
                 ret_val_regs and ret_val_regs_float (integer and float \
                 return registers are physically distinct)",
            ));
        }
        // Float ABI position `j` lands at `arg_passing_regs.len() + j`, which
        // only holds while the two lists name different registers: one register
        // in both would emit the same value as two different arguments.
        if let Some(vn) = first_in_both(&self.arg_passing_regs, &self.arg_passing_regs_float) {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: varnode {vn:?} appears in both \
                 arg_passing_regs and arg_passing_regs_float (integer and float \
                 argument registers are physically distinct)",
            ));
        }
        for (list_name, list) in [
            ("arg_passing_regs", &self.arg_passing_regs),
            ("arg_passing_regs_float", &self.arg_passing_regs_float),
            ("callee_saved_regs", &self.callee_saved_regs),
            ("ret_val_regs", &self.ret_val_regs),
            ("ret_val_regs_float", &self.ret_val_regs_float),
        ] {
            if list.contains(&self.stack_vn) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: stack_vn {:?} appears in {} \
                     (the SP is implicit and must not be in any reg list)",
                    self.stack_vn,
                    list_name,
                ));
            }
            if let Some(vn) = first_dup(list) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: duplicate varnode {vn:?} in {list_name}",
                ));
            }
        }
        if let Some(lr) = self.link_register_vn
            && !self.callee_saved_regs.contains(&lr)
        {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: link_register_vn {lr:?} must also \
                 be present in callee_saved_regs, the deliberate tradeoff \
                 that makes InitialVar(lr) propagate through call sites)",
            ));
        }
        // A negative pop would mean the callee's `ret` grew the stack, which
        // no real ABI does.
        if self.ret_stack_pop < 0 {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: ret_stack_pop must be >= 0, got {}",
                self.ret_stack_pop,
            ));
        }
        if let Some(sa) = self.stack_args {
            if sa.increment <= 0 {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: stack-arg increment must be > 0, got {}",
                    sa.increment,
                ));
            }
            // `slot_of` subtracts `base_offset` overflow-free for any
            // `offset >= base_offset`, even a garbage decoded one, only while
            // it is non-negative.
            if sa.base_offset < 0 {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: stack-arg base_offset must be >= 0, got {}",
                    sa.base_offset,
                ));
            }
        }
        Ok(())
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
    /// by one.  An index past `usize::MAX` clamps there rather than wrapping.
    #[must_use]
    pub fn slot_of(&self, offset: i128) -> Option<usize> {
        debug_assert!(
            self.increment > 0,
            "StackArgs::slot_of requires increment > 0"
        );
        if offset < self.base_offset {
            return None;
        }
        Some(clamp_to_usize((offset - self.base_offset) / self.increment))
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
        clamp_to_usize(numerator / self.increment)
    }
}

/// Saturating i128 -> usize.  A slot index or span past `usize::MAX` would
/// otherwise wrap, and a wrapped 0 breaks the `>= 1` span every loop-advancing
/// caller relies on.
fn clamp_to_usize(n: i128) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
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
    // Intel386 psABI: floats go on the stack like every other cdecl argument.
    arg_passing_regs_float: &[],
    callee_saved_regs: &["EBX", "ESI", "EDI", "EBP"],
    ret_val_regs: &["EAX", "EDX"],
    // cdecl returns scalar float / double / long double in ST0, the x87 80-bit
    // top-of-stack, and a COMPLEX_X87 value in ST0/ST1 (Intel386 psABI).  XMM0
    // is the return register only for the `__m128` / vector classes.
    // `FunctionBuilder::new` seeds all three on EVERY function, so ST0 (10
    // bytes at register offset 0x1100) subsumes MM0 (8 bytes, same offset) in
    // `dedup_overlapping_largest` and MMX accesses slice out of ST0.
    ret_val_regs_float: &["ST0", "ST1", "XMM0"],
    // +4 because `call` pushes a 4-byte return address: SP-at-call points at
    // that address, so arg 0 sits one slot above it.
    stack_args: Some(StackArgs {
        base_offset: 4,
        increment: 4,
    }),
    ret_stack_pop: 4,
    // `call` pushes the return address.
    link_register_reg_name: None,
    preserves_memory: false,
    preserves_all_registers: false,
};

/// Base for `powerpc64_elf_v1` and `powerpc64_elf_v2`, which differ in
/// `stack_args.base_offset` (ELFv1's 48-byte linkage area vs ELFv2's 32, each
/// plus the 64-byte parameter save area) and in `ret_val_regs_float`.
const POWERPC64_ELF_BASE: CallingConvention = CallingConvention {
    stack_ptr_reg_name: "r1",
    arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
    // ELFv1 3.2.3 / ELFv2 2.2.3.1: floating-point arguments in f1..f13.  The
    // f1..f8 window is the 32-bit SysV rule (`powerpc_sysv32`).
    arg_passing_regs_float: &[
        "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13",
    ],
    callee_saved_regs: &[
        // r13 (TLS thread pointer) is reserved and genuinely never written.
        // r2 (TOC) is NOT callee-saved: ELFv1 3.5.11 / ELFv2 2.3.2.1 put the
        // save and restore on the CALLER, and a cross-module callee does alter
        // it.  Modelled preserved anyway, the same tradeoff as `LR` below: it
        // keeps TOC-relative loads resolvable across a call, and a caller that
        // needs r2 afterwards reloads it from the save slot, so the reload is
        // what a correct binary reads.
        "r2", "r13", "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23", "r24",
        "r25", "r26", "r27", "r28", "r29", "r30", "r31",
        // PPC64 ELFv1 3.4 marks LR volatile/caller-saved.  Listing it
        // callee-saved anyway is the deliberate link-register tradeoff (same
        // as `powerpc_sysv32`): it makes `InitialVar(lr)` propagate through
        // call sites, so the indirect-branch resolver's `LinkRegister` arm
        // fires for functions returning via the entry LR.
        //
        // ELFv1 3.2.2 / ELFv2 2.2.1 make f14-f31 non-volatile.
        "LR", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24", "f25",
        "f26", "f27", "f28", "f29", "f30", "f31",
    ],
    ret_val_regs: &["r3", "r4"],
    // `long double` is IBM double-double and returns in the f1:f2 pair, as
    // does `_Complex double`.
    ret_val_regs_float: &["f1", "f2"],
    // 48-byte linkage area, then the 8-doubleword parameter save area that
    // homes r3-r10; the first stack-ONLY argument is above both.  GHIDRA's
    // ppc_64_be.cspec agrees on 112.
    stack_args: Some(StackArgs {
        base_offset: 112,
        increment: 8,
    }),
    ret_stack_pop: 0,
    link_register_reg_name: Some("LR"),
    preserves_memory: false,
    preserves_all_registers: false,
};

/// Base for `arm_aapcs` and `arm_aapcs_soft`, which differ only in the VFP
/// argument and return lists.  `bl` stores the return address in `lr` rather
/// than pushing it, so the first stack arg sits at SP+0 and `ret_stack_pop` is
/// 0.  Byte-order independent: pairs with `arm`, `arm_be`, `arm_be_kernel`,
/// and `arm_thumb`.  r0/r1 double as the pair for 64-bit returns.
const ARM_AAPCS_VFP_BASE: CallingConvention = CallingConvention {
    stack_ptr_reg_name: "sp",
    arg_passing_regs: &["r0", "r1", "r2", "r3"],
    // AAPCS-VFP: the VFP argument bank is d0..d7 (also viewed as s0..s15).
    arg_passing_regs_float: &["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"],
    callee_saved_regs: &[
        "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "lr",
        // AAPCS 5.1.2.1: d8-d15 (= s16-s31) are callee-saved.
        "d8", "d9", "d10", "d11", "d12", "d13", "d14", "d15",
    ],
    ret_val_regs: &["r0", "r1"],
    // d0..d3, also accessed as s0..s7: a homogeneous float aggregate returns
    // up to four members.
    ret_val_regs_float: &["d0", "d1", "d2", "d3"],
    stack_args: Some(StackArgs {
        base_offset: 0,
        increment: 4,
    }),
    ret_stack_pop: 0,
    // `bl` writes the return address to `lr` (= `r14`); Sleigh registers it
    // lowercase.
    link_register_reg_name: Some("lr"),
    preserves_memory: false,
    preserves_all_registers: false,
};

/// Base for `mips_o32` and `mips_n64`; n64 widens both argument banks,
/// narrows the callee-saved float set, and drops the shadow space.
const MIPS_O32_BASE: CallingConvention = CallingConvention {
    stack_ptr_reg_name: "sp",
    arg_passing_regs: &["a0", "a1", "a2", "a3"],
    // O32 passes at most two float arguments, in $f12 and $f14; $f13 / $f15
    // are the odd halves of those double pairs, never argument slots of their
    // own.
    //
    // Positional here, which o32 is NOT: it uses the FP registers only while
    // every preceding argument is also FP, so `g(int, double)` passes the
    // double in a2/a3 and `g(float, int, float)` passes the second float in
    // a2. Which rule applies depends on the callee's signature, and the IR does
    // not carry one, so a function whose arguments do not lead with floats can
    // report an $f12 temporary as float argument 0.
    arg_passing_regs_float: &["f12", "f14"],
    callee_saved_regs: &[
        // `gp` is preserved by the callee only in non-PIC o32.  Under
        // `-mabicalls` the callee recomputes it from `t9` and the CALLER
        // restores it (`.cprestore`), so o32 PIC gets the same deliberate
        // over-preservation as `LR` on PowerPC: `InitialVar(gp)` reaches GOT
        // loads, and a PIC caller needing `gp` after a call reloads it.
        "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra",
        // o32 preserves $f20-$f31; n64 narrows it to $f24-$f31 and overrides
        // this whole list.
        "f20", "f21", "f22", "f23", "f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31",
    ],
    ret_val_regs: &["v0", "v1"],
    // Single-precision; doubles use the f0/f1 pair.  Unused on soft-float
    // builds.
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
    preserves_all_registers: false,
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
            // psABI 3.2.3: SSE-class arguments in XMM0..XMM7.  X87-class
            // (`long double`) arguments go on the stack, so ST0/ST1 are
            // return-only here.
            arg_passing_regs_float: &[
                "XMM0", "XMM1", "XMM2", "XMM3", "XMM4", "XMM5", "XMM6", "XMM7",
            ],
            callee_saved_regs: &["RBX", "RBP", "R12", "R13", "R14", "R15"],
            ret_val_regs: &["RAX", "RDX"],
            // SSE class in XMM0/XMM1; X87 class (`long double`) in ST0; the
            // COMPLEX_X87 class puts the real part in ST0 and the imaginary
            // part in ST1 (psABI 3.2.3).  Without ST1 nothing roots the
            // imaginary half's cone and DCE deletes it.
            //
            // `FunctionBuilder::new` tracks every `ret_val_regs_float` entry on
            // EVERY function, so listing ST0 (10 bytes at register offset
            // 0x1100) makes it subsume MM0 (8 bytes, same offset) in
            // `dedup_overlapping_largest`, and MMX accesses slice out of ST0.
            ret_val_regs_float: &["XMM0", "XMM1", "ST0", "ST1"],
            // +8 because `call` pushes an 8-byte return address: SP-at-call
            // points at it, so the first stack arg (arg 7) is one slot above.
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 8,
            }),
            ret_stack_pop: 8,
            // `call` pushes the return address.
            link_register_reg_name: None,
            preserves_memory: false,
            preserves_all_registers: false,
        },
    },
    // AArch64 AAPCS64.  `ret_stack_pop` is 0 because `bl` writes the return
    // address to `lr` rather than pushing it.  Register conventions are
    // byte-order independent, so this pairs with both `aarch64` and
    // `aarch64be`.
    //
    // AAPCS64 6.9's indirect result location register `x8` is NOT listed: it
    // carries the destination address for a large aggregate return, not an
    // argument, and listing it would renumber every `arg(n)`.  The cost is that
    // a caller passing a stack slot there has an escape `frame_escape` cannot
    // see, so `assumptions.escape_analysis` (off by default) could forward a
    // spill across such a call.  `x8` is still a `Call` clobber, so the
    // register itself is never stale.
    CcPresetRow {
        name: "aarch64_aapcs64",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            // AAPCS64 6.4.1: SIMD/FP arguments in v0..v7, listed in their
            // widest form so an s/d/q access slices out of one container.
            arg_passing_regs_float: &["q0", "q1", "q2", "q3", "q4", "q5", "q6", "q7"],
            callee_saved_regs: &[
                "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30",
                // AAPCS64 6.1.2 preserves only the LOW 64 bits of v8-v15, so
                // these name the `d` views; naming `q8`-`q15` would claim the
                // upper halves preserved too.
                "d8", "d9", "d10", "d11", "d12", "d13", "d14", "d15",
            ],
            ret_val_regs: &["x0", "x1"],
            // v0..v3, the widest form of the s/d/q sub-registers: a
            // homogeneous float aggregate returns up to four members.
            ret_val_regs_float: &["q0", "q1", "q2", "q3"],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ret_stack_pop: 0,
            // `lr` is an alias for `x30`, and Sleigh's aarch64 table only
            // registers `x30`.
            link_register_reg_name: Some("x30"),
            preserves_memory: false,
            preserves_all_registers: false,
        },
    },
    // ARM 32-bit AAPCS, hard-float (VFP) argument variant.  A binary built
    // `-mfloat-abi=soft` / `softfp` wants `arm_aapcs_soft` instead; nothing in
    // an ELF header distinguishes them for the lifter, so the caller picks.
    CcPresetRow {
        name: "arm_aapcs",
        cc: ARM_AAPCS_VFP_BASE,
    },
    // ARM 32-bit AAPCS base standard: floats pass and return in the core
    // registers (`-mfloat-abi=soft` and `softfp` alike), so the VFP bank
    // carries no arguments.  d8-d15 stay callee-saved, which is a property of
    // the register file rather than of the float variant.
    CcPresetRow {
        name: "arm_aapcs_soft",
        cc: CallingConvention {
            arg_passing_regs_float: &[],
            ret_val_regs_float: &[],
            ..ARM_AAPCS_VFP_BASE
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
            // N64 widens the float argument bank to eight, $f12..$f19.
            arg_passing_regs_float: &["f12", "f13", "f14", "f15", "f16", "f17", "f18", "f19"],
            // n64's float argument bank is $f12-$f19; the callee-saved set
            // narrows from o32's $f20-$f31 to $f24-$f31.
            callee_saved_regs: &[
                "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra", "f24", "f25",
                "f26", "f27", "f28", "f29", "f30", "f31",
            ],
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
            // SysV PPC32: floating-point arguments in f1..f8.
            arg_passing_regs_float: &["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"],
            callee_saved_regs: &[
                // r2 (system/TLS thread pointer) and r13 (small-data-area base)
                // are reserved/dedicated: preserved across calls like r14-r31.
                "r2", "r13", "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23",
                "r24", "r25", "r26", "r27", "r28", "r29", "r30", "r31", "LR",
                // SysV PPC32 3-16: f14-f31 are non-volatile.
                "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24", "f25",
                "f26", "f27", "f28", "f29", "f30", "f31",
            ],
            ret_val_regs: &["r3", "r4"],
            ret_val_regs_float: &["f1", "f2"],
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 4,
            }),
            ret_stack_pop: 0,
            // `bl` writes the return address to the `LR` SPR; Sleigh's PPC
            // table names it uppercase.
            link_register_reg_name: Some("LR"),
            preserves_memory: false,
            preserves_all_registers: false,
        },
    },
    // PowerPC 64-bit ELFv1, BE (`powerpc64-linux-gnu-gcc`).  Stack args start
    // at 112: the 48-byte linkage area plus the 64-byte parameter save area.
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
            // 32-byte linkage area plus the same 8-doubleword parameter save
            // area.  Measured against `powerpc64le-linux-gnu-gcc`: a 16-argument
            // call stores arguments 9..16 at r1+96..152.  GHIDRA's
            // ppc_64_le.cspec says 112, which is the ELFv1 figure copied over.
            stack_args: Some(StackArgs {
                base_offset: 96,
                increment: 8,
            }),
            // ELFv2 2.2.3.3 returns a homogeneous float aggregate of up to
            // eight members in f1-f8; `powerpc64le-linux-gnu-gcc -O1` reads
            // members 7 and 8 straight out of f7/f8.  ELFv1 returns such an
            // aggregate through a hidden pointer, so the base's f1:f2 pair
            // (`long double`, `_Complex double`) is the whole story there.
            ret_val_regs_float: &["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"],
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
    // The only kernel-internal preset: every other supported arch's kernel CC
    // is byte-identical to its userland preset, so callers pick that directly.
    // A syscall ABI lives in `call_other_abi`: the `syscall` / `int 0x80` /
    // `svc` traps lift to `CallOther`.
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
        aarch64_aapcs64,
        "Returns the AArch64 AAPCS64 calling convention."
    );
    cc_factory!(
        arm_aapcs,
        "Returns the ARM 32-bit AAPCS hard-float (VFP) calling convention."
    );
    cc_factory!(
        arm_aapcs_soft,
        "Returns the ARM 32-bit AAPCS calling convention for a \
         `-mfloat-abi=soft` / `softfp` binary."
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
    /// does not resolve, then on the first
    /// [`BuiltCallingConvention::validate`] violation.
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        let arg_passing_regs = regs_to_vns(sleigh_regs, self.arg_passing_regs)?;
        let arg_passing_regs_float = regs_to_vns(sleigh_regs, self.arg_passing_regs_float)?;
        let callee_saved_regs = regs_to_vns(sleigh_regs, self.callee_saved_regs)?;
        let ret_val_regs = regs_to_vns(sleigh_regs, self.ret_val_regs)?;
        let ret_val_regs_float = regs_to_vns(sleigh_regs, self.ret_val_regs_float)?;
        let stack_vn = vn_for_name(sleigh_regs, self.stack_ptr_reg_name)?;
        let link_register_vn = self
            .link_register_reg_name
            .map(|name| vn_for_name(sleigh_regs, name))
            .transpose()?;
        let built = BuiltCallingConvention {
            arg_passing_regs,
            arg_passing_regs_float,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_vn,
            stack_args: self.stack_args,
            ret_stack_pop: self.ret_stack_pop,
            link_register_vn,
            preserves_memory: self.preserves_memory,
            preserves_all_registers: self.preserves_all_registers,
            no_return: false,
        };
        built.validate()?;
        Ok(built)
    }

    /// Returns a variant of this convention that clobbers nothing: every
    /// register callee-saved and memory unchanged, with no arguments or
    /// return value. Models a transparent hook (`__fentry__` / `mcount`)
    /// invoked under `self`'s stack/link-register geometry.
    pub fn preserves_all(self) -> Self {
        Self {
            arg_passing_regs: &[],
            arg_passing_regs_float: &[],
            ret_val_regs: &[],
            ret_val_regs_float: &[],
            stack_args: None,
            preserves_memory: true,
            preserves_all_registers: true,
            ..self
        }
    }

    /// Like [`Self::preserves_all`] but leaves memory clobberable: registers
    /// are all preserved, memory is not.
    ///
    /// Keeps `arg_passing_regs` and `stack_args`, unlike `preserves_all`: the
    /// callee may write memory, so the escape-based load-forwarding gate has
    /// to see a frame address handed to it. Dropping the argument registers
    /// would hide that escape and forward a spill across the call.
    pub fn preserves_regs(self) -> Self {
        Self {
            ret_val_regs: &[],
            ret_val_regs_float: &[],
            preserves_memory: false,
            preserves_all_registers: true,
            ..self
        }
    }

    cc_factory!(
        x86_linux_kernel,
        "Returns the Linux kernel-internal CC for x86 32-bit (`-mregparm=3`)."
    );
}

#[cfg(test)]
mod tests;

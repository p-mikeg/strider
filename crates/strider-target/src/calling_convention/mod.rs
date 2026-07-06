use anyhow::anyhow;

use crate::Result;

/// Resolves a single Sleigh register name to its [`rsleigh::Vn`], or returns
/// an error if the name is not known.  Single source of truth for the
/// name-to-varnode error path.
pub(crate) fn vn_for_name(sleigh_regs: &rsleigh::SleighRegs, name: &str) -> Result<rsleigh::Vn> {
    sleigh_regs
        .name_to_vn(name)
        .ok_or_else(|| anyhow!("unknown sleigh register name {name:?}"))
}

/// Resolves a slice of Sleigh register names to varnodes in the same order.
/// Short-circuits on the first unknown name.
pub(crate) fn regs_to_vns(
    sleigh_regs: &rsleigh::SleighRegs,
    reg_names: &[&str],
) -> Result<Vec<rsleigh::Vn>> {
    reg_names
        .iter()
        .map(|&name| vn_for_name(sleigh_regs, name))
        .collect()
}

/// A calling convention definition expressed as static string slices of
/// register names.
///
/// Call [`CallingConvention::build`] to resolve the names to concrete varnodes
/// using a Sleigh register table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallingConvention {
    /// The Sleigh register name of the hardware stack pointer.  Stored on
    /// the convention because every built convention needs the stack
    /// pointer's `Vn` resolved, and the `SleighArch` that would otherwise
    /// own this fact is already passed separately to `Strider::new`.
    stack_ptr_reg_name: &'static str,
    /// Sleigh register names for the ABI's argument-passing registers, in
    /// positional order.  Resolved into
    /// [`BuiltCallingConvention::arg_passing_regs`] by [`Self::build`].
    arg_passing_regs: &'static [&'static str],
    /// Sleigh register names for registers the callee must preserve across
    /// the call.  Resolved into [`BuiltCallingConvention::callee_saved_regs`]
    /// by [`Self::build`].  Excludes the stack pointer; SP's cross-call
    /// behaviour is expressed through [`Self::ret_stack_pop`].
    callee_saved_regs: &'static [&'static str],
    /// Sleigh register names for return-value registers, in positional order.
    /// Resolved into [`BuiltCallingConvention::ret_val_regs`] by
    /// [`Self::build`].
    ret_val_regs: &'static [&'static str],
    /// Sleigh register names for float return-value registers (e.g. `q0` on
    /// aarch64, `XMM0` on x86_64, `d0` on ARM AAPCS soft-float, `f0` on
    /// MIPS O32).  Listed separately from [`Self::ret_val_regs`] because
    /// these registers have different widths from the integer return regs
    /// (and the size invariant in the test suite checks the integer list).
    /// Resolved into [`BuiltCallingConvention::ret_val_regs_float`] by
    /// [`Self::build`].
    ret_val_regs_float: &'static [&'static str],
    /// Stack-passed-argument layout (`base_offset` + `increment`, unbounded),
    /// or `None` when the convention passes no arguments on the stack.
    stack_args: Option<StackArgs>,
    /// Net byte change the callee's `ret` inflicts on the caller's stack
    /// pointer.  On stack-push ISAs (x86, x86_64) `ret` pops the return
    /// address, so this equals the pointer size (4 / 8).  On link-register
    /// ISAs (ARM, AArch64, MIPS, PowerPC) the call does not touch SP, so
    /// this is 0.
    ret_stack_pop: i64,
    /// The Sleigh register name of the calling convention's link register
    /// (the register that holds the return address across a call), or
    /// `None` on stack-push ISAs (x86, x86_64) where the return address
    /// lives on the stack instead.  Resolved into
    /// [`BuiltCallingConvention::link_register_vn`] by [`Self::build`].
    /// Used by the indirect-branch resolver to recognise `bx lr` /
    /// `pop {pc}` / `jr ra` shapes as `Return`.
    link_register_reg_name: Option<&'static str>,
    /// `true` if calls under this convention preserve **all** observable
    /// state, including memory.  When set, `strider_ir::FunctionBuilder::build_call`
    /// skips emitting a Memory output on the resulting Call node and does not
    /// advance the region's memory chain — so passes like `LoadReadOnly` and
    /// `LoadForward` can forward loads across the call.
    ///
    /// `false` for every standard ABI; `true` only on
    /// [`Self::x86_64_all_preserving`] and analogous "transparent hook"
    /// presets (e.g. Linux-kernel `__fentry__` / `mcount` callbacks that
    /// preserve all caller state).
    preserves_memory: bool,
}

/// A calling convention whose register names have been resolved to concrete
/// [`rsleigh::Vn`] varnodes.
///
/// Produced by [`CallingConvention::build`] (canonical path) or
/// [`Self::try_new`] (test/override construction).  Fields are `pub`
/// because the type is immutable post-construction — the validating
/// constructor [`Self::try_new`] checks every ABI invariant, and there
/// is no mutating API.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    /// Argument-passing register varnodes, in positional order.
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    /// Callee-saved register varnodes.  Excludes the stack pointer; SP's
    /// callee-side preservation is expressed through [`Self::ret_stack_pop`].
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    /// Integer return-value register varnodes, in positional order.
    pub ret_val_regs: Vec<rsleigh::Vn>,
    /// Float return-value register varnodes (e.g. `[q0, q1]` on AArch64,
    /// `[XMM0, XMM1]` on x86_64).  Tracked separately from
    /// [`Self::ret_val_regs`] because their widths differ.
    pub ret_val_regs_float: Vec<rsleigh::Vn>,
    /// Hardware stack-pointer varnode.  Deliberately absent from the three
    /// register-list fields above — SP's cross-call behaviour is expressed
    /// through [`Self::ret_stack_pop`] instead.
    pub stack_vn: rsleigh::Vn,
    /// Stack-passed-argument layout (`base_offset` + `increment`, unbounded),
    /// or `None` when the convention passes no arguments on the stack.
    pub stack_args: Option<StackArgs>,
    /// Net byte change the callee's `ret` inflicts on the caller's SP.
    /// `8` on x86_64 (pops return address); `0` on link-register ISAs.
    pub ret_stack_pop: i64,
    /// Link-register varnode on link-register ISAs (ARM, AArch64, MIPS,
    /// PowerPC); `None` on stack-push ISAs (x86, x86_64).  Consumed by the
    /// indirect-branch resolver to classify return-shaped indirect branches.
    pub link_register_vn: Option<rsleigh::Vn>,
    /// `true` when calls under this CC preserve memory (zero-side-effect
    /// hooks like `__fentry__` / `mcount`).  Consumed by the IR builder's
    /// `build_call` to suppress the Call's Memory output so
    /// `LoadReadOnly` / `LoadForward` can forward across the call.
    pub preserves_memory: bool,
    /// `true` when a call under this CC never returns — a `noreturn` / `__dead`
    /// callee such as `exit`, `abort`, `panic`, or `__stack_chk_fail`.
    ///
    /// Attached to a call TARGET via a per-address CC override.  The CFG
    /// builder terminates the calling region `NoReturn` (lowered to
    /// `Call + Unreachable`) at such a call regardless of where the return
    /// address lands, so the unreachable fall-through — including a
    /// *mid-function* one — is never lifted as a live successor.  The default
    /// (`false`) leaves the CFG builder's function-end structural fallback in
    /// charge: a call whose return address leaves the function bound is treated
    /// as unreachable-after even for an unmarked callee.
    pub no_return: bool,
}

/// The trivial / synthetic calling convention: no real ABI.
///
/// Every [`crate::BuiltCallingConvention`]-bearing type (notably
/// `strider_ir::Function`) requires a convention, but synthetic / mock
/// graphs constructed in tests have no real target ABI.  This `Default`
/// is what they get: empty register lists, no stack arguments,
/// `ret_stack_pop = 0`, `preserves_memory = false`, no link register,
/// and a **synthetic `stack_vn`** that is a real, sized register matching
/// no real machine register.
///
/// The synthetic SP is an 8-byte REGISTER-space varnode at the
/// out-of-range offset `SYNTHETIC_STACK_VN_OFFSET`.  It is a *real*
/// sized register (unlike the former zero-sized const sentinel) so a
/// `Call` built under the trivial CC can mint a well-typed
/// `InitialVar(stack_vn)` SP anchor — a `Call` always requires a real
/// stack pointer.  The offset is far outside any architecture's register
/// file, so it never collides with a tracked register: stack analyses
/// (`StackOffsetDetect`, `LoadForward`) still find no matches against it
/// on a trivial-CC function (which is the correct "no modelled stack"
/// behaviour), and the SP-exclusion-from-clobbers filter
/// (`*v != stack_vn`) never spuriously drops a real tracked register.
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

/// REGISTER-space offset of the synthetic stack-pointer varnode minted by
/// [`BuiltCallingConvention::default`].  Chosen far outside any real
/// architecture's register file so it never aliases a tracked register.
pub(crate) const SYNTHETIC_STACK_VN_OFFSET: u64 = 0xFFFF_FFFF_FFFF_0000;

/// Returns the first element of `a` that also appears in `b`, or `None`
/// when the two lists are disjoint.  Shared "find first offending element"
/// helper for [`BuiltCallingConvention::try_new`]'s pairwise
/// disjointness checks; preserves first-offender reporting (the first
/// element of `a` in iteration order).  O(|a|·|b|) over the ≤31-element ABI
/// register lists.
fn first_in_both<'a>(a: &'a [rsleigh::Vn], b: &[rsleigh::Vn]) -> Option<&'a rsleigh::Vn> {
    a.iter().find(|vn| b.contains(vn))
}

/// Returns the first element of `list` that recurs later in the same list
/// (the first duplicate), or `None` when every element is unique.  Shared
/// within-list-uniqueness helper for [`BuiltCallingConvention::try_new`];
/// O(n²) over the ≤31-element ABI register lists.
fn first_dup(list: &[rsleigh::Vn]) -> Option<&rsleigh::Vn> {
    list.iter()
        .enumerate()
        .find(|(i, vn)| list[i + 1..].contains(vn))
        .map(|(_, vn)| vn)
}

impl BuiltCallingConvention {
    /// Split this convention's clobbered registers into the ret-val group and
    /// the (non-ret) caller-clobbered group, over the given `tracked_vns`.
    ///
    /// This is the **single source of truth** for CC register-list projection:
    /// the production lifter and the strider-ir test-fixture `build_call_cc`
    /// both call it, so their `Call` output shapes agree.  Each CC register is
    /// resolved to its tracked container via the injected `container_of`
    /// (the lifter passes its O(1) vn→container map; IR-side callers pass a
    /// `largest_container_in` scan) — that resolution is the only
    /// machine-register knowledge involved, and it stays with the caller.
    ///
    /// A register is *clobbered* iff it is neither callee-saved nor the stack
    /// pointer.  The returned `(ret_vals, clobbers)`:
    /// - `ret_vals`: the tracked, clobbered containers of the combined return
    ///   list (`ret_val_regs` then `ret_val_regs_float`), in ABI order.
    /// - `clobbers`: every other clobbered REGISTER/UNIQUE `tracked_vns` entry,
    ///   in `tracked_vns` order.
    ///
    /// These are exactly the two output groups a `Call` emits past
    /// `[Control, Memory]`.
    pub fn ret_and_clobber_vns(
        &self,
        tracked_vns: &[rsleigh::Vn],
        container_of: impl Fn(&rsleigh::Vn) -> rsleigh::Vn,
    ) -> (Vec<rsleigh::Vn>, Vec<rsleigh::Vn>) {
        let stack_vn = self.stack_vn;
        // The register lists are tiny (1-4 regs), so a linear `Vec::contains`
        // is cheaper than hashing and needs no extra dependency.
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

    /// Validating constructor.  Builds a
    /// `BuiltCallingConvention` from explicit fields and checks the
    /// canonical ABI invariants:
    ///
    /// - `arg_passing_regs ∩ callee_saved_regs == ∅`
    /// - `ret_val_regs ∩ callee_saved_regs == ∅`
    /// - `ret_val_regs_float ∩ callee_saved_regs == ∅`
    /// - `stack_vn` is not in any of the four register lists
    /// - No duplicates within any single list
    /// - When `link_register_vn` is `Some`, it must be present in
    ///   `callee_saved_regs` (CLAUDE.md "Note (link-register
    ///   handling)" deliberate tradeoff)
    /// - `ret_stack_pop` is non-negative
    /// - When `stack_args` is `Some`, its `increment` is `> 0` and its
    ///   `base_offset` is `>= 0`
    ///
    /// # Errors
    ///
    /// Returns `Err` describing the first invariant violation
    /// detected.  The error is intentionally specific so a CC author
    /// debugging a typo (e.g. listing the same Vn in both
    /// `arg_passing_regs` and `callee_saved_regs`) sees the offending
    /// names rather than a downstream miscompile.
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        arg_passing_regs: Vec<rsleigh::Vn>,
        callee_saved_regs: Vec<rsleigh::Vn>,
        ret_val_regs: Vec<rsleigh::Vn>,
        ret_val_regs_float: Vec<rsleigh::Vn>,
        stack_vn: rsleigh::Vn,
        stack_args: Option<StackArgs>,
        ret_stack_pop: i64,
        link_register_vn: Option<rsleigh::Vn>,
        preserves_memory: bool,
    ) -> std::result::Result<Self, anyhow::Error> {
        // Disjointness: arg-passing must not overlap callee-saved.
        if let Some(vn) = first_in_both(&arg_passing_regs, &callee_saved_regs) {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: varnode {:?} appears in both \
                 arg_passing_regs and callee_saved_regs (a single varnode \
                 cannot be both caller-supplied and callee-preserved)",
                vn,
            ));
        }
        // Ret-val regs must not overlap callee-saved (the callee writes
        // them to deliver results — they cannot be required-preserved).
        for vn in ret_val_regs.iter().chain(ret_val_regs_float.iter()) {
            if callee_saved_regs.contains(vn) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: varnode {:?} appears in both \
                     ret_val_regs/ret_val_regs_float and callee_saved_regs",
                    vn,
                ));
            }
        }
        // Integer- and float-return registers are physically distinct register
        // files on every supported arch; the same varnode in both is a
        // CC-author bug.  (arg ∩ ret is deliberately *not* checked — x86_64
        // SysV RDX is legitimately both the 3rd arg and the 2nd int return.)
        if let Some(vn) = first_in_both(&ret_val_regs, &ret_val_regs_float) {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: varnode {:?} appears in both \
                 ret_val_regs and ret_val_regs_float (integer and float \
                 return registers are physically distinct)",
                vn,
            ));
        }
        // Per-list checks: SP-not-present + within-list uniqueness.
        // Walked in one pass over the four named lists.
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
        // Link-register-as-callee-saved invariant (CLAUDE.md note).
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
        // ret_stack_pop is non-negative (a negative value would mean the
        // callee's `ret` *grew* the stack, which no real ABI does).
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
            // A negative base_offset would let `index_of` / `slot_of`'s
            // `offset - base_offset` overflow on a garbage offset; reject it here
            // (the construction boundary) so those hot-path subtractions stay
            // overflow-free for any `offset >= base_offset`.
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

/// Layout of stack-passed arguments: an unbounded arithmetic series of
/// slots.  The N-th stack argument (0-indexed among the stack args)
/// occupies `base_offset + N * increment` bytes from the call-time stack
/// pointer.  Every supported ABI's stack-arg series has a uniform stride
/// equal to its word size, so this captures all of them exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StackArgs {
    /// Byte offset from call-time SP of the first stack-passed argument.
    pub base_offset: i128,
    /// Byte stride between consecutive stack-arg slots (the ABI word size);
    /// always `> 0`.
    pub increment: i128,
}

impl StackArgs {
    /// Byte offset (from call-time SP) of the `n`-th stack argument.
    ///
    /// Saturates at `i128::MAX` for a runaway index rather than overflowing —
    /// a saturated offset matches no real stack store, so the over-collecting
    /// `call_stack_args` cursor walk terminates cleanly instead of panicking.
    #[must_use]
    pub fn offset_of(&self, n: usize) -> i128 {
        self.base_offset
            .saturating_add((n as i128).saturating_mul(self.increment))
    }

    /// The stack-arg index whose slot fully contains a `size`-byte access
    /// starting at `offset` (from call-time SP), or `None` when `offset` is
    /// below `base_offset` or the access straddles a slot boundary.
    ///
    /// A zero-size access (`size == 0`) trivially fits any slot, so it yields
    /// `Some(slot-of-start)` for any `offset >= base_offset`.  Offsets are
    /// decoded from binary content, so `offset + size` is computed with a
    /// checked add: an overflowing (garbage) offset degrades to `None` rather
    /// than panicking in debug / wrapping in release.
    ///
    /// Superseded in prod by [`Self::slot_of`] + `slots_spanned`; retained
    /// only for the strict within-one-slot tests.
    #[cfg(test)]
    #[must_use]
    pub fn index_of(&self, offset: i128, size: i128) -> Option<usize> {
        // `increment > 0` is a type invariant (enforced by `try_new`); guard
        // it here too so a directly-constructed `increment == 0` surfaces as a
        // clear assertion in debug builds rather than an integer divide-by-zero.
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
        // overflow; the slot end and the access end can, so both are checked.
        let slot_start = self.base_offset + (idx as i128) * self.increment;
        let slot_end = slot_start.checked_add(self.increment)?;
        let access_end = offset.checked_add(size)?;
        (access_end <= slot_end).then_some(idx)
    }

    /// The stack-arg slot whose range *contains the start byte* of an access
    /// at `offset` (from call-time SP): `floor((offset - base_offset) /
    /// increment)`, or `None` when `offset` is below `base_offset`.
    ///
    /// Unlike the (test-only) `index_of` this imposes **no upper bound on the access
    /// size**: an argument wider than one slot (a 32-bit-ABI `double`, an
    /// x86-64 `long double`) is attributed to the slot its first byte lands
    /// in, and a sub-field read landing mid-slot is attributed to the slot it
    /// starts in.  The returned value is a *byte-position* slot index, not an
    /// argument ordinal — a wider-than-slot argument advances the ordinal by
    /// one while spanning several slots, so a caller wanting the positional
    /// ordinal walks these slot indices with a width-aware cursor.
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

    /// The number of consecutive stack slots a `size`-byte argument occupies:
    /// `ceil(max(size, 1) / increment)`, always `>= 1`.
    ///
    /// A zero- or one-byte argument occupies one slot; an argument wider than
    /// `increment` (a 32-bit-ABI `double`, an x86-64 `long double`) spans the
    /// slots its bytes cover.  This is the cursor-advance companion to
    /// [`Self::slot_of`]: `slot_of` anchors a wide argument at the slot of its
    /// first byte, and `slots_spanned` says how many slots to step past it.
    ///
    /// Like [`Self::offset_of`] the arithmetic **saturates** rather than
    /// overflowing: a garbage decoded `size` (from arbitrary lifted
    /// arithmetic) degrades to a large-but-finite span instead of wrapping the
    /// `i128` intermediate.
    #[must_use]
    pub fn slots_spanned(&self, size: i128) -> usize {
        debug_assert!(
            self.increment > 0,
            "StackArgs::slots_spanned requires increment > 0"
        );
        let size = size.max(1);
        // ceil(size / increment): add `increment - 1` to the numerator, but
        // saturate so a pathological `size` can't overflow the i64 add.
        let numerator = size.saturating_add(self.increment - 1);
        (numerator / self.increment) as usize
    }
}

/// One row of the calling-convention preset table.  Carries the
/// preset's lookup name alongside the `CallingConvention` itself —
/// the entire data surface lives in the `CC_PRESETS` table below, and
/// the named factory functions (`x86_64_systemv`, `x86_cdecl`, ...)
/// are thin wrappers that perform a name lookup.
///
/// `CallingConvention` is `Copy` (all fields are `&'static`), so each
/// row stays on the static heap with no runtime construction cost.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CcPresetRow {
    /// The Rust factory name (also the Python classmethod name).
    name: &'static str,
    /// The convention itself.  `Copy` — every field is `&'static`.
    cc: CallingConvention,
}

/// Shared base for the two x86 32-bit presets (`x86_cdecl` and
/// `x86_linux_kernel`), which differ only in `arg_passing_regs`.
const X86_CDECL_BASE: CallingConvention = CallingConvention {
    stack_ptr_reg_name: "ESP",
    arg_passing_regs: &[],
    callee_saved_regs: &["EBX", "ESI", "EDI", "EBP"],
    ret_val_regs: &["EAX", "EDX"],
    // x86 cdecl returns floats in `ST0` (the x87 FPU's 80-bit
    // top-of-stack).  GCC's i686 default lowers floats through
    // x87 even when arithmetic is via SSE.  Listing ST0 here
    // (now that the IR has F80 / I80 support) keeps the Return
    // node connected to the float chain.
    //
    // XMM0 is also listed as a fallback for SSE-default builds
    // (`-mfpmath=sse2`).  When neither is referenced by the
    // function, `FunctionBuilder::new`'s upgrade-to-container
    // logic skips them harmlessly.
    ret_val_regs_float: &["ST0", "XMM0"],
    // Offsets start at +4: the `call` instruction pushes a 4-byte
    // return address, so SP-at-call points to the return address
    // and arg 0 lives one slot above it.
    stack_args: Some(StackArgs {
        base_offset: 4,
        increment: 4,
    }),
    ret_stack_pop: 4,
    // x86 `call` pushes the return address on the stack; there is
    // no architectural link register.
    link_register_reg_name: None,
    preserves_memory: false,
};

/// Shared base for the two PowerPC64 ELF presets (`powerpc64_elf_v1`
/// and `powerpc64_elf_v2`), which differ only in `stack_args.base_offset`
/// (48-byte ELFv1 linkage area vs 32-byte ELFv2).
const POWERPC64_ELF_BASE: CallingConvention = CallingConvention {
    stack_ptr_reg_name: "r1",
    arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
    callee_saved_regs: &[
        "r2", "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23", "r24",
        "r25", "r26", "r27", "r28", "r29", "r30", "r31",
        // include `LR` per the CLAUDE.md
        // "Note (link-register handling)" deliberate tradeoff
        // (consistent with `powerpc_sysv32`).  PPC64 ELFv1
        // §3.4 marks LR as volatile/caller-saved, but listing
        // it here makes `InitialVar(lr)` propagate through
        // call sites so the indirect-branch resolver's
        // `LinkRegister` arm fires for functions returning
        // via the entry LR.
        "LR",
    ],
    ret_val_regs: &["r3", "r4"],
    ret_val_regs_float: &["f1"],
    stack_args: Some(StackArgs {
        base_offset: 48,
        increment: 8,
    }),
    ret_stack_pop: 0,
    // Same as 32-bit PPC SysV: the return address lives in `LR`.
    link_register_reg_name: Some("LR"),
    preserves_memory: false,
};

/// Shared base for the two MIPS presets (`mips_o32` and `mips_n64`),
/// which differ in `arg_passing_regs` and `stack_args`.
const MIPS_O32_BASE: CallingConvention = CallingConvention {
    stack_ptr_reg_name: "sp",
    arg_passing_regs: &["a0", "a1", "a2", "a3"],
    callee_saved_regs: &[
        "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra",
    ],
    ret_val_regs: &["v0", "v1"],
    // FPU return regs (4-byte single-precision; doubles use the
    // f0/f1 pair).  Even on soft-float builds the listing is harmless
    // — these regs are simply unused.
    ret_val_regs_float: &["f0", "f2"],
    stack_args: Some(StackArgs {
        base_offset: 16,
        increment: 4,
    }),
    ret_stack_pop: 0,
    // MIPS `jal`/`jalr` writes the return address to `$ra` (`$31`);
    // Sleigh's mips32 register table uses lowercase `ra`.
    link_register_reg_name: Some("ra"),
    preserves_memory: false,
};

/// Static data table of every supported calling convention preset.
///
/// Adding a new preset means appending one [`CcPresetRow`] entry — the
/// dispatch scaffolding (named factory wrappers + `lookup_preset`) does
/// not need changes.
///
/// Linear lookup is fine: ~22 rows, each `name` comparison is a string
/// `eq` that short-circuits on length.
pub(crate) static CC_PRESETS: &[CcPresetRow] = &[
    // ── Userland presets ────────────────────────────────────────────
    //
    // x86-64 System V ABI.
    //   Argument registers: RDI, RSI, RDX, RCX, R8, R9
    //   Callee-saved: RBX, RBP, R12–R15
    //   Return value: RAX, RDX
    //   RSP is the stack pointer and is not listed as callee-saved —
    //   `ret` pops the return address, so the caller observes SP shifted
    //   by `ret_stack_pop` across the call.
    CcPresetRow {
        name: "x86_64_systemv",
        cc: CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: &["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
            callee_saved_regs: &["RBX", "RBP", "R12", "R13", "R14", "R15"],
            ret_val_regs: &["RAX", "RDX"],
            // SSE return regs (16-byte XMM); used for `float`/`double` returns.
            ret_val_regs_float: &["XMM0", "XMM1"],
            // Offsets start at +8: the `call` instruction pushes an 8-byte
            // return address, so SP-at-call points to the return address and
            // the first stack-passed arg (arg 7) lives one slot above it.
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 8,
            }),
            ret_stack_pop: 8,
            // x86-64 `call` pushes the return address on the stack; there
            // is no architectural link register.
            link_register_reg_name: None,
            preserves_memory: false,
        },
    },
    // "All-preserving" x86_64: every userland caller-clobbered register is
    // listed as callee-saved.  Empty arg-passing list, empty ret-val list.
    // Used for sites like Linux-kernel `__fentry__` / `mcount` callbacks
    // that preserve all caller state.  Pair with the per-address override
    // map on `CallingConvention` consumers so the override applies only
    // to specific Call sites; the function-default CC stays SystemV.
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
            // An all-preserving site is still a normal `call`/`ret`: the
            // callee's `ret` pops the 8-byte return address, so SP shifts
            // by 8 across the call exactly as in `x86_64_systemv`.  Only
            // the *register* set is preserved, not the stack mechanics.
            ret_stack_pop: 8,
            link_register_reg_name: None,
            // The defining property of "all-preserving": memory is also
            // preserved.  build_call skips the Memory output so
            // LoadReadOnly / LoadForward forward across the call.
            preserves_memory: true,
        },
    },
    // AArch64 AAPCS64.
    //   Argument registers: x0–x7
    //   Callee-saved: x19–x28, x29 (frame pointer), x30 (link register)
    //   Return value: x0, x1
    //   `sp` is the stack pointer and is not listed as callee-saved —
    //   `ret_stack_pop` is 0 on AAPCS64 because `bl` writes the return
    //   address to `lr` rather than pushing it.
    //   AAPCS64 register conventions are independent of byte order, so
    //   this preset pairs equally with `SleighArch::aarch64` (LE) and
    //   `SleighArch::aarch64be` (BE).
    CcPresetRow {
        name: "aarch64_aapcs64",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            callee_saved_regs: &[
                "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29", "x30",
            ],
            ret_val_regs: &["x0", "x1"],
            // AArch64 SIMD return regs (16-byte vector; contain s0/d0/q0
            // sub-registers).  Now that vn_mask + build_int_const support
            // I128, the ABI-correct q0/q1 (16-byte) is preferred over d0/d1
            // (which was an earlier workaround for missing I128 support).
            ret_val_regs_float: &["q0", "q1"],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ret_stack_pop: 0,
            // AArch64's `lr` is an alias for `x30`; Sleigh's aarch64
            // register table only registers `x30`.
            link_register_reg_name: Some("x30"),
            preserves_memory: false,
        },
    },
    // ARM 32-bit AAPCS.
    //   Argument registers: r0–r3
    //   Callee-saved: r4–r11, lr
    //   Return value: r0, r1  (r0/r1 pair used for 64-bit return values)
    //   `sp` is the stack pointer and is not listed as callee-saved.
    //   Unlike x86, ARM `bl` stores the return address in `lr` rather
    //   than pushing it, so the first stack-passed arg sits at SP + 0 and
    //   `ret_stack_pop` is 0.
    //   AAPCS register conventions are independent of byte order, so this
    //   preset pairs equally with `SleighArch::arm` (LE), `arm_be` (BE),
    //   and `arm_thumb` (Thumb-2).
    CcPresetRow {
        name: "arm_aapcs",
        cc: CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["r0", "r1", "r2", "r3"],
            callee_saved_regs: &["r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "lr"],
            ret_val_regs: &["r0", "r1"],
            // VFP return regs (8-byte d0/d1, also accessed as 4-byte s0/s1).
            // For VFP-disabled (-mfloat-abi=soft) builds the float result still
            // flows through r0/r1 — listing d0/d1 doesn't hurt because they're
            // simply unused in that case.
            ret_val_regs_float: &["d0", "d1"],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 4,
            }),
            ret_stack_pop: 0,
            // ARM's `bl` writes the return address to `lr` (= `r14`);
            // Sleigh registers it under the lowercase `lr` name.
            link_register_reg_name: Some("lr"),
            preserves_memory: false,
        },
    },
    // MIPS O32.
    //   Used by 32-bit MIPS Linux binaries on both LE and BE targets —
    //   the ABI is identical regardless of byte order.  Pairs equally with
    //   `SleighArch::mipsle32` and `mipsbe32`.
    //   Argument registers: a0, a1, a2, a3 (= r4–r7)
    //   Callee-saved:       s0–s7, s8 (= fp), gp, ra
    //   Return value:       v0, v1 (= r2, r3)
    //   `sp` (= r29) is the stack pointer.  `ret_stack_pop` is 0 because
    //   MIPS `jal`/`jalr` writes the return address to `$ra` rather than
    //   pushing it.  The first 16 bytes of stack-arg space (offsets 0..16)
    //   are MIPS's reserved "shadow space" for the four register args;
    //   positional stack args start at offset 16.
    //   Note: Sleigh's MIPS spec uses lowercase names (`a0`, `s0`, `sp`,
    //   `ra`, `gp`) and `s8` for the frame pointer register (not `fp`,
    //   which does not resolve in the Sleigh register table).
    CcPresetRow {
        name: "mips_o32",
        cc: MIPS_O32_BASE,
    },
    // MIPS N64 (used by 64-bit MIPS Linux binaries on both LE and BE —
    // `mips64-linux-gnuabi64-gcc`).
    //   The N64 ABI extends O32's 4 register args to 8 register args
    //   (`$4`–`$11`).  Sleigh's `mips64` spec uses the older naming where
    //   `$4`–`$7` are `a0`–`a3` and `$8`–`$11` are `t0`–`t3`, so the
    //   arg-passing list lists the latter under their Sleigh names.
    //   Argument registers: a0–a3 (`$4`–`$7`), t0–t3 (`$8`–`$11`)
    //   Callee-saved:       s0–s7, s8 (= fp), gp, ra
    //   Return value:       v0, v1
    //   Float return:       f0, f2
    //   Stack args start at offset 0 from SP (no O32-style shadow space).
    CcPresetRow {
        name: "mips_n64",
        // Same as O32 except 8 register args (adds t0–t3) and no O32-style
        // 16-byte shadow space (stack args start at SP+0, 8-byte stride).
        cc: CallingConvention {
            arg_passing_regs: &["a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3"],
            stack_args: Some(StackArgs {
                base_offset: 0,
                increment: 8,
            }),
            ..MIPS_O32_BASE
        },
    },
    // PowerPC 32-bit System V ABI.  Used by `powerpc-linux-gnu-gcc` (with
    // both `-mbig-endian` and `-mlittle-endian` — the ABI is byte-order
    // independent).
    //   Argument registers: r3–r10 (8 GPRs)
    //   Callee-saved:       r14–r31, LR
    //   Return value:       r3, r4 (r3:r4 pair for 64-bit returns)
    //   Float return:       f1
    //   Stack args start at offset 8 (4-byte back-chain + 4-byte LR save).
    //   `r1` is the stack pointer in PowerPC convention.
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
            // PowerPC `bl` writes the return address to the `LR` SPR;
            // Sleigh's PPC register table uses uppercase `LR`.
            link_register_reg_name: Some("LR"),
            preserves_memory: false,
        },
    },
    // PowerPC 64-bit ELFv1 calling convention (BE — used by
    // `powerpc64-linux-gnu-gcc`).
    //   ELFv1 has function descriptors: an external function symbol
    //   resolves to a 3-pointer descriptor (entry, TOC, env) rather than
    //   the entry directly.  The analyzer treats indirect calls in ELFv1
    //   binaries as pointer-to-descriptor; pattern queries that need the
    //   entry address must follow the descriptor convention.  For now we
    //   register the register-level ABI; descriptor-aware lifting is a
    //   follow-up.
    //   Argument registers: r3–r10 (8 GPRs)
    //   Callee-saved:       r2 (TOC), r14–r31
    //   Return value:       r3
    //   Float return:       f1
    //   Stack args start at offset 48 (ELFv1 linkage area is 48 bytes).
    CcPresetRow {
        name: "powerpc64_elf_v1",
        cc: POWERPC64_ELF_BASE,
    },
    // PowerPC 64-bit ELFv2 calling convention (LE — used by
    // `powerpc64le-linux-gnu-gcc`).
    //   ELFv2 drops function descriptors — symbols point directly to the
    //   entry point.  Linkage area shrinks from 48 to 32 bytes.  Otherwise
    //   register usage matches ELFv1.
    //   Argument registers: r3–r10 (8 GPRs)
    //   Callee-saved:       r2 (TOC), r14–r31
    //   Return value:       r3
    //   Float return:       f1
    //   Stack args start at offset 32 (ELFv2 linkage area is 32 bytes).
    CcPresetRow {
        name: "powerpc64_elf_v2",
        // Identical to ELFv1 except the linkage area shrinks 48 → 32 bytes,
        // so stack args start at SP+32.
        cc: CallingConvention {
            stack_args: Some(StackArgs {
                base_offset: 32,
                increment: 8,
            }),
            ..POWERPC64_ELF_BASE
        },
    },
    // x86 cdecl.  Arguments passed on the stack, so `arg_passing_regs` is
    // empty.
    //   Callee-saved: EBX, ESI, EDI, EBP
    //   Return value: EAX, EDX
    //   ESP is the stack pointer and is not listed as callee-saved —
    //   `ret` pops the 4-byte return address, so the caller observes SP
    //   shifted by `ret_stack_pop` across the call.
    CcPresetRow {
        name: "x86_cdecl",
        cc: X86_CDECL_BASE,
    },
    // ── Linux kernel-internal preset ────────────────────────────────
    //
    // Only x86 32-bit needs its own row: every other supported arch's
    // kernel-internal CC is byte-identical to the userland preset, so the
    // caller selects the userland preset directly rather than a redundant
    // kernel alias.  (Syscall ABIs are not calling conventions at all —
    // the `syscall` / `int 0x80` / `svc` traps lift to `CallOther`, whose
    // register footprint lives in `call_other_abi`.)

    // x86 32-bit Linux kernel-internal CC (`-mregparm=3`): the first
    // three integer args go in EAX, EDX, ECX; remaining args sit on the
    // stack at the same cdecl offsets.  Differs from `x86_cdecl` only in
    // `arg_passing_regs`.
    CcPresetRow {
        name: "x86_linux_kernel",
        // `-mregparm=3`: first three integer args in EAX, EDX, ECX; the rest
        // sit on the stack at the same cdecl offsets — the only difference.
        cc: CallingConvention {
            arg_passing_regs: &["EAX", "EDX", "ECX"],
            ..X86_CDECL_BASE
        },
    },
];

/// Look up a calling-convention preset by its factory name.
///
/// Linear scan over `CC_PRESETS` — the table holds ~22 rows, and
/// each name comparison short-circuits on length, so the lookup is
/// cheap enough to skip a hash map.
pub(crate) fn lookup_preset(name: &str) -> Option<&'static CcPresetRow> {
    CC_PRESETS.iter().find(|row| row.name == name)
}

/// Internal helper used by every named factory wrapper below — looks
/// up the row and returns its `CallingConvention`.
///
/// Panics if no row matches `name`: every named factory passes its own
/// `stringify!`'d name, which always has a matching `CC_PRESETS` row, so
/// a miss is an internal-consistency failure in this source file rather
/// than a caller error.
fn cc_from_table(name: &'static str) -> CallingConvention {
    lookup_preset(name)
        .unwrap_or_else(|| panic!("calling-convention preset not registered: {name}"))
        .cc
}

/// Emits a named factory wrapper around [`cc_from_table`].  `$desc` is
/// the per-preset description that becomes the first paragraph of the
/// rustdoc.
///
/// `#[doc = concat!(...)]` is used because rustdoc's `///` form
/// doesn't accept macro variables — the macro emits `#[doc = "..."]`
/// directly with the computed string.
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

    /// Resolves all register name strings in this calling convention to their
    /// concrete [`rsleigh::Vn`] varnodes using `sleigh_regs`.
    ///
    /// The number of varnodes in each resulting list equals the length of the
    /// corresponding name list.
    ///
    /// # Errors
    ///
    /// Returns an error if any register name listed in this convention
    /// (including the stack pointer) does not resolve against `sleigh_regs`.
    /// The resolution short-circuits on the first failure.
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        let arg_passing_regs = regs_to_vns(sleigh_regs, self.arg_passing_regs)?;
        let callee_saved_regs = regs_to_vns(sleigh_regs, self.callee_saved_regs)?;
        let ret_val_regs = regs_to_vns(sleigh_regs, self.ret_val_regs)?;
        let ret_val_regs_float = regs_to_vns(sleigh_regs, self.ret_val_regs_float)?;
        let stack_vn = vn_for_name(sleigh_regs, self.stack_ptr_reg_name)?;
        // Resolve the link-register name when one is declared; propagate
        // any `UnknownRegName` from `vn_for_name` so a typo in the preset
        // surfaces at build time rather than later in the indirect-branch
        // resolver.
        let link_register_vn = self
            .link_register_reg_name
            .map(|name| vn_for_name(sleigh_regs, name))
            .transpose()?;
        // Route through `try_new` so the disjointness invariants
        // (SP not in any reg list, arg/callee-saved disjoint, no
        // duplicates within a list, link-reg in callee-saved when set,
        // non-negative ret_stack_pop) are enforced at build time.  The
        // documented presets all satisfy them; routing here means a
        // future preset with a typo (SP in arg_passing_regs, missing
        // link-reg, etc.) fails at construction rather than producing
        // a downstream miscompile.
        BuiltCallingConvention::try_new(
            arg_passing_regs,
            callee_saved_regs,
            ret_val_regs,
            ret_val_regs_float,
            stack_vn,
            self.stack_args,
            self.ret_stack_pop,
            link_register_vn,
            self.preserves_memory,
        )
    }
}

// ── Linux kernel-internal preset wrapper ─────────────────────────────────────
//
// Only x86 32-bit has a kernel-internal CC distinct from its userland
// preset (`-mregparm=3`), so it is the sole kernel wrapper.  Every other
// arch's kernel CC equals its userland preset — callers use that directly.
// Syscall ABIs are not calling conventions: the `syscall` / `int 0x80` /
// `svc` traps lift to `CallOther`, classified through `call_other_abi`.
impl CallingConvention {
    cc_factory!(
        x86_linux_kernel,
        "Returns the Linux kernel-internal CC for x86 32-bit (`-mregparm=3`)."
    );
}

#[cfg(test)]
mod tests;

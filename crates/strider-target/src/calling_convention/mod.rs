use anyhow::anyhow;

use crate::{ArchPreset, Result, SleighArch, family};

/// The register `name` names in `sleigh_regs`.
///
/// # Errors
///
/// `name` is not a register of this table.
pub fn vn_for_name(sleigh_regs: &rsleigh::SleighRegs, name: &str) -> Result<rsleigh::Vn> {
    sleigh_regs
        .name_to_vn(name)
        .ok_or_else(|| anyhow!("unknown sleigh register name {name:?}"))
}

/// The registers `reg_names` names in `sleigh_regs`, in order; stops at the
/// first unknown name.
///
/// # Errors
///
/// A name is not a register of this table.
pub fn regs_to_vns(
    sleigh_regs: &rsleigh::SleighRegs,
    reg_names: impl IntoIterator<Item: AsRef<str>>,
) -> Result<Vec<rsleigh::Vn>> {
    reg_names
        .into_iter()
        .map(|name| vn_for_name(sleigh_regs, name.as_ref()))
        .collect()
}

/// A calling convention carrying each register as `R` and each register list
/// as `L`.
///
/// The two instantiations are [`CallingConvention`], which names registers as
/// static Sleigh strings, and [`BuiltCallingConvention`], the same convention
/// with every one of those names resolved to a varnode.
/// [`CallingConvention::build`] is that substitution and nothing else, so a
/// field means the same thing on both.
///
/// `Copy` only when `R` and `L` are, which keeps [`CallingConvention`] usable
/// in a `const` preset while the `Vec`-carrying [`BuiltCallingConvention`]
/// stays move-or-clone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cc<R, L> {
    /// In register-positional order, which is not source-argument order: ARM
    /// AAPCS, MIPS O32 and 32-bit SysV PowerPC all start a 64-bit integer
    /// argument at an even position of this list, skipping an odd one (PPC32
    /// pairs r5:r6, leaving r4), so from the first 64-bit argument onward
    /// position *n* names a register the caller may never have written.
    pub arg_passing_regs: L,
    /// Float / vector argument registers, in positional order, from a register
    /// file `arg_passing_regs` never names (`XMM0..7` on x86-64 SysV, `q0..7`
    /// on AAPCS64, `d0..7` on AAPCS-VFP, `$f12`/`$f14` on MIPS O32, `f1..13`
    /// then `v2..13` on PowerPC64 ELF and `f1..8` then `v2..13` on 32-bit SysV
    /// PowerPC).  Where the float and vector banks are physically distinct, as
    /// on PowerPC, the vector window is appended so the float positions stay
    /// put, and the `*_linux_kernel` flavours drop it.  Empty where floats are
    /// stack-passed (i386 cdecl).  Legitimately overlaps `ret_val_regs_float`:
    /// `XMM0` / `d0` / `f1` are both.
    ///
    /// Adding an entry RENUMBERS the `Call`'s flat argument list: every
    /// collected stack argument moves up one slot, so a public `call().arg(n)`
    /// query reads a different operand.  `FunctionBuilder::new` also tracks
    /// each entry on EVERY function, whether or not the function names it.
    pub arg_passing_regs_float: L,
    /// Excludes the stack pointer.  The float half matters as much as the
    /// integer one: without the entries covering it, a post-call read of a
    /// register the callee really preserved resolves to a `Call` output
    /// instead of to the incoming value.
    pub callee_saved_regs: L,
    /// In positional order.
    pub ret_val_regs: L,
    /// Float return registers, in positional order (`q0` on aarch64, `XMM0` on
    /// x86_64, `d0` on ARM AAPCS hard-float (VFP), the `f0_1` pair on MIPS
    /// O32).
    pub ret_val_regs_float: L,
    /// The stack pointer.  Shares no byte with the five register lists above.
    pub stack_ptr: R,
    /// `None` when the convention passes no arguments on the stack.
    pub stack_args: Option<StackArgs>,
    /// Net byte change the callee's `ret` inflicts on the caller's SP: the
    /// pointer size (4 on x86, 8 on x86_64) on stack-push ISAs where `ret`
    /// pops the return address, 0 on link-register ISAs where the call never
    /// touches SP.
    pub ret_stack_pop: i64,
    /// Register holding the return address across a call; `None` on
    /// stack-push ISAs (x86, x86_64), where it lives on the stack.
    pub link_register: Option<R>,
    /// `true` if calls under this convention preserve memory. `false` for
    /// every standard ABI; set by [`Self::preserves_all`] for transparent
    /// zero-side-effect hooks (Linux-kernel `__fentry__` / `mcount`).
    pub preserves_memory: bool,
    /// `true` if a call clobbers NO register (every register callee-saved), as
    /// for `__fentry__` / `mcount`.  Set by [`Self::preserves_all`] /
    /// [`Self::preserves_regs`]; false for every real ABI.
    pub preserves_all_registers: bool,
    /// The register the ABI guarantees holds the function's own entry address
    /// on entry to it: PPC64 ELFv2 `r12` (2.2.2.1) and MIPS abicalls `t9`,
    /// which the prologue turns into the TOC / GOT pointer. The lifter binds it
    /// to the entry address. `None` where the ABI promises nothing, ELFv1
    /// included: there the analogue is the descriptor's TOC word, which is not
    /// a register contract.
    pub entry_address: Option<R>,
    /// The register arguments have stack homes just below the stack
    /// arguments, one `stack_args` slot each in register order, so a
    /// by-value argument the ABI splits between registers and the stack, and
    /// a variadic tail, is one object across both: ARM AAPCS and MIPS N64,
    /// whose callee pushes the registers there, and MIPS O32 and PowerPC64
    /// ELF, whose caller reserves the space.
    pub reg_args_adjoin_stack_args: bool,
}

/// A calling convention as static register-name slices.
pub type CallingConvention = Cc<&'static str, &'static [&'static str]>;

/// A [`CallingConvention`] with its register names resolved to varnodes.
pub type BuiltCallingConvention = Cc<rsleigh::Vn, Vec<rsleigh::Vn>>;

/// The trivial convention for synthetic / mock graphs that have no real
/// target ABI: everything empty, plus a synthetic `stack_ptr`.
impl Default for BuiltCallingConvention {
    fn default() -> Self {
        Self {
            arg_passing_regs: Vec::new(),
            arg_passing_regs_float: Vec::new(),
            callee_saved_regs: Vec::new(),
            ret_val_regs: Vec::new(),
            ret_val_regs_float: Vec::new(),
            stack_ptr: rsleigh::Vn {
                addr_off: SYNTHETIC_STACK_VN_OFFSET,
                addr_space: rsleigh::VnSpace::REGISTER,
                size: 8,
            },
            stack_args: None,
            ret_stack_pop: 0,
            link_register: None,
            preserves_memory: false,
            preserves_all_registers: false,
            entry_address: None,
            reg_args_adjoin_stack_args: false,
        }
    }
}

impl<R, L: Default> Cc<R, L> {
    /// Returns a variant of this convention that clobbers nothing: every
    /// register callee-saved and memory unchanged, with no arguments or
    /// return value. Models a transparent hook (`__fentry__` / `mcount`)
    /// invoked under `self`'s stack/link-register geometry.
    pub fn preserves_all(self) -> Self {
        Self {
            arg_passing_regs: L::default(),
            arg_passing_regs_float: L::default(),
            ret_val_regs: L::default(),
            ret_val_regs_float: L::default(),
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
            ret_val_regs: L::default(),
            ret_val_regs_float: L::default(),
            preserves_memory: false,
            preserves_all_registers: true,
            ..self
        }
    }
}

/// Far outside any real architecture's register file.
pub(crate) const SYNTHETIC_STACK_VN_OFFSET: u64 = 0xFFFF_FFFF_FFFF_0000;

/// First entry of `a` sharing a byte with any entry of `b`.  Overlap rather
/// than equality: AArch64's `q8` CONTAINS `d8`, both overlaid on `z8` at
/// `offset=0x5000` (`AARCH64instructions.sinc:638`/`:674` little-endian,
/// `:820`/`:856` big-endian), so a rule saying "disjoint" has to see the
/// containment.  They share a START only on LE; the big-endian banks put
/// `q8` at `z8`+16 and `d8` at `z8`+24.
fn first_overlapping<'a>(a: &'a [rsleigh::Vn], b: &[rsleigh::Vn]) -> Option<&'a rsleigh::Vn> {
    a.iter()
        .find(|vn| b.iter().any(|other| vn_container::vn_overlaps(vn, other)))
}

/// First entry of `list` appearing twice.  EXACT equality, unlike the
/// cross-list [`first_overlapping`]: ARM's `ret_val_regs_float` is `d0..d3`,
/// four registers inside two `q` containers, which an overlap rule would
/// reject.
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
        let end = vn_container::vn_end(vn);
        let mut covered_to = u128::from(vn.addr_off);
        while covered_to < end {
            let Some(reach) = self
                .callee_saved_regs
                .iter()
                .filter(|s| s.addr_space == vn.addr_space && u128::from(s.addr_off) <= covered_to)
                .map(vn_container::vn_end)
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
    /// A register is *clobbered* iff `preserves_all_registers` is unset, it is
    /// not the stack pointer, and `callee_saved_regs` leaves at least one of
    /// its bytes free.  `ret_vals`
    /// holds the tracked clobbered containers of `ret_val_regs` then
    /// `ret_val_regs_float` in ABI order; `clobbers` holds every other
    /// clobbered REGISTER entry in `tracked_vns` order.
    ///
    /// REGISTER only: a p-code CALL writes no Sleigh temporary, so a tracked
    /// UNIQUE varnode is not a clobber.
    pub fn ret_and_clobber_vns(
        &self,
        tracked_vns: &[rsleigh::Vn],
        container_of: impl Fn(&rsleigh::Vn) -> rsleigh::Vn,
    ) -> (Vec<rsleigh::Vn>, Vec<rsleigh::Vn>) {
        let stack_ptr = self.stack_ptr;
        let is_clobbered = |v: &rsleigh::Vn| {
            !self.preserves_all_registers && !self.preserves_all_bytes_of(v) && *v != stack_ptr
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
                v.addr_space == rsleigh::VnSpace::REGISTER
                    && is_clobbered(v)
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

    /// The float arguments a call passes: [`Self::float_arg_slots`] up to the
    /// first `None`, so index `j` stays ABI position `j`, which is what the
    /// callee side records.
    ///
    /// Every convention a lift sees has its float argument registers tracked,
    /// so a `None` means one the function was not lifted against.
    pub fn float_arg_prefix(
        &self,
        tracked_vns: &[rsleigh::Vn],
        container_of: impl Fn(&rsleigh::Vn) -> rsleigh::Vn,
    ) -> Vec<rsleigh::Vn> {
        self.float_arg_slots(tracked_vns, container_of)
            .into_iter()
            .map_while(|slot| slot)
            .collect()
    }

    /// Where the incoming arguments' memory starts, as an offset from the
    /// entry SP: the first stack argument, or under
    /// [`Self::reg_args_adjoin_stack_args`] the first register argument's
    /// home.  `None` when no argument is passed on the stack.
    #[must_use]
    pub fn incoming_args_start(&self) -> Option<i128> {
        let args = self.stack_args?;
        if !self.reg_args_adjoin_stack_args {
            return Some(args.base_offset);
        }
        let homes = self.arg_passing_regs.len() as i128;
        Some(
            args.base_offset
                .saturating_sub(homes.saturating_mul(args.increment)),
        )
    }

    /// Checks the canonical ABI invariants.  Advisory: every field is `pub`
    /// and [`Default`] builds one directly, so a convention reaching the
    /// lifter has not necessarily been through here.
    ///
    /// Disjointness is over BYTES, not varnode identity, so a container and
    /// the register it holds count as the same storage.
    ///
    /// - `callee_saved_regs` is disjoint from `arg_passing_regs`,
    ///   `arg_passing_regs_float`, `ret_val_regs`, and `ret_val_regs_float`
    /// - `ret_val_regs` and `ret_val_regs_float` are disjoint
    /// - `arg_passing_regs` and `arg_passing_regs_float` are disjoint
    /// - `stack_ptr` shares no byte with any of the five register lists
    /// - no duplicates within any single list
    /// - a `Some` `link_register` is also in `callee_saved_regs` (the
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
            if let Some(vn) = first_overlapping(list, &self.callee_saved_regs) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: varnode {vn:?} in {list_name} shares \
                     bytes with callee_saved_regs (a single register cannot be \
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
            if self
                .callee_saved_regs
                .iter()
                .any(|saved| vn_container::vn_overlaps(vn, saved))
            {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: varnode {vn:?} in \
                     ret_val_regs/ret_val_regs_float shares bytes with \
                     callee_saved_regs",
                ));
            }
        }
        // Integer and float returns are physically distinct register files on
        // every supported arch.  An argument register may legitimately also be
        // a return register: x86_64 SysV RDX is 3rd arg and 2nd int return.
        if let Some(vn) = first_overlapping(&self.ret_val_regs, &self.ret_val_regs_float) {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: varnode {vn:?} in ret_val_regs shares \
                 bytes with ret_val_regs_float (integer and float return \
                 registers are physically distinct)",
            ));
        }
        // Float ABI position `j` lands at `arg_passing_regs.len() + j`, which
        // only holds while the two lists name different registers: one register
        // in both would emit the same value as two different arguments.
        if let Some(vn) = first_overlapping(&self.arg_passing_regs, &self.arg_passing_regs_float) {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: varnode {vn:?} in arg_passing_regs \
                 shares bytes with arg_passing_regs_float (integer and float \
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
            if let Some(vn) = first_overlapping(list, std::slice::from_ref(&self.stack_ptr)) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: varnode {vn:?} in {list_name} shares \
                     bytes with the stack pointer {:?} (the SP is implicit and must \
                     not be in any reg list)",
                    self.stack_ptr,
                ));
            }
            if let Some(vn) = first_dup(list) {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: duplicate varnode {vn:?} in {list_name}",
                ));
            }
        }
        if let Some(lr) = self.link_register
            && !self.callee_saved_regs.contains(&lr)
        {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: link_register {lr:?} must also \
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

    /// Rejects a convention whose stack geometry is not that of an arch with
    /// `addr_size`-byte addresses. A convention resolves by register name, and
    /// one arch's names resolve on another: x86 `ESP` is a 4-byte view of
    /// x86-64 `RSP`, ppc32 `r1` the whole 8-byte `r1` on ppc64.
    ///
    /// # Errors
    ///
    /// `stack_ptr` is not `addr_size` bytes, a stack-arg slot is not one
    /// `stack_ptr` wide, or a convention with no link register pops less than
    /// the return address its `call` pushed.
    pub fn check_addr_size(&self, addr_size: u32) -> Result<()> {
        let sp = self.stack_ptr.size;
        if sp != addr_size {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: stack pointer {:?} is {sp} bytes, and this \
                 arch's addresses are {addr_size}: the convention describes another arch",
                self.stack_ptr,
            ));
        }
        if let Some(sa) = self.stack_args
            && sa.increment != i128::from(sp)
        {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: stack-arg increment {} is not the {sp}-byte \
                 stack slot of this arch",
                sa.increment,
            ));
        }
        if self.link_register.is_none() && self.ret_stack_pop < i64::from(sp) {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: ret_stack_pop {} is less than the {sp}-byte \
                 return address a `call` pushes with no link register",
                self.ret_stack_pop,
            ));
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

    /// Byte offset of the `n`-th stack argument measured from `sp_offset`,
    /// the call's SP in the probing coordinate system.  The one stepper the
    /// outgoing-argument window and `CallStackArgCollect` share, so the two
    /// prefixes cannot drift.
    ///
    /// Saturating like [`Self::offset_of`]: stopping short of a slot the ABI
    /// declares would narrow the window, and a window narrower than the
    /// region the callee owns is the unsound direction.  A caller stepping an
    /// unbounded series therefore stops on its own when the offset stops
    /// advancing.
    #[must_use]
    pub fn slot_offset(&self, sp_offset: i128, n: usize) -> i128 {
        sp_offset.saturating_add(self.offset_of(n))
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

/// A named calling-convention preset and the arch presets it describes.
#[derive(Debug)]
pub struct CcPreset {
    /// The [`CallingConvention`] constructor's name, e.g. `"x86_64_systemv"`.
    pub name: &'static str,
    pub cc: CallingConvention,
    /// Every arch preset whose binaries this convention is for.
    pub arches: &'static [ArchPreset],
    /// The Ghidra prototypes it is held to by the oracle test: (cspec path
    /// under the vendored processors tree, prototype name or None for the
    /// default, the arches that cspec is Ghidra's for).
    pub ghidra: &'static [(&'static str, Option<&'static str>, &'static [ArchPreset])],
}

impl CallingConvention {
    /// Every preset, family by family.
    pub fn presets() -> impl Iterator<Item = &'static CcPreset> {
        family::FAMILIES
            .iter()
            .flat_map(|family| family.calling_conventions)
    }

    /// Resolves every register name in this convention against `sleigh_regs`.
    ///
    /// # Errors
    ///
    /// Short-circuits on the first name (the stack pointer included) that
    /// does not resolve, then on the first
    /// [`BuiltCallingConvention::validate`] violation.
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        // Field order here is the short-circuit order the doc promises.
        let one = |name| vn_for_name(sleigh_regs, name);
        let list = |names| regs_to_vns(sleigh_regs, names);
        let built = BuiltCallingConvention {
            arg_passing_regs: list(self.arg_passing_regs)?,
            arg_passing_regs_float: list(self.arg_passing_regs_float)?,
            callee_saved_regs: list(self.callee_saved_regs)?,
            ret_val_regs: list(self.ret_val_regs)?,
            ret_val_regs_float: list(self.ret_val_regs_float)?,
            stack_ptr: one(self.stack_ptr)?,
            stack_args: self.stack_args,
            ret_stack_pop: self.ret_stack_pop,
            link_register: self.link_register.map(one).transpose()?,
            preserves_memory: self.preserves_memory,
            preserves_all_registers: self.preserves_all_registers,
            entry_address: self.entry_address.map(one).transpose()?,
            reg_args_adjoin_stack_args: self.reg_args_adjoin_stack_args,
        };
        built.validate()?;
        Ok(built)
    }
}

impl SleighArch {
    /// The presets whose [`CcPreset::arches`] name this arch.
    pub fn calling_conventions(self) -> impl Iterator<Item = &'static CcPreset> {
        let preset = self.preset();
        CallingConvention::presets().filter(move |cc| cc.arches.contains(&preset))
    }
}

#[cfg(test)]
mod tests;

use anyhow::anyhow;

use crate::{ArchPreset, Result, SleighArch, family};

/// The register `name` names in `sleigh_regs`.
///
/// # Errors
/// `name` is not a register of this table.
pub fn vn_for_name(sleigh_regs: &rsleigh::SleighRegs, name: &str) -> Result<rsleigh::Vn> {
    sleigh_regs
        .name_to_vn(name)
        .ok_or_else(|| anyhow!("unknown sleigh register name {name:?}"))
}

/// The registers `reg_names` names in `sleigh_regs`, in order.
///
/// # Errors
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

/// A calling convention with registers as names ([`CallingConvention`]) or
/// varnodes ([`BuiltCallingConvention`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cc<R, L> {
    /// In register order, which is not source-argument order: AAPCS, MIPS O32
    /// and PPC32 start a 64-bit argument at an even position, skipping one.
    pub arg_passing_regs: L,
    /// Float / vector argument registers, in positional order; a separate
    /// vector bank is appended after the floats. An entry renumbers every
    /// `Call`'s stack arguments and is tracked on every function.
    pub arg_passing_regs_float: L,
    /// Excludes the stack pointer.
    pub callee_saved_regs: L,
    /// In positional order.
    pub ret_val_regs: L,
    /// Float return registers, in positional order.
    pub ret_val_regs_float: L,
    /// Shares no byte with the five register lists above.
    pub stack_ptr: R,
    /// `None` when the convention passes no arguments on the stack.
    pub stack_args: Option<StackArgs>,
    /// Bytes the callee's return adds to the caller's SP: the pushed return
    /// address on x86, 0 with a link register.
    pub ret_stack_pop: i64,
    /// Register holding the return address; `None` where it is pushed.
    pub link_register: Option<R>,
    /// Calls preserve memory ([`Self::preserves_all`]).
    pub preserves_memory: bool,
    /// Calls clobber no register ([`Self::preserves_regs`]).
    pub preserves_all_registers: bool,
    /// The register holding the function's own address on entry (ELFv2 `r12`,
    /// MIPS abicalls `t9`), which the lifter binds to the entry address.
    pub entry_address: Option<R>,
    /// The register arguments have stack homes just below the stack
    /// arguments, one `stack_args` slot each in register order, so an argument
    /// split between registers and stack is one object.
    pub reg_args_adjoin_stack_args: bool,
}

/// A calling convention as static register-name slices.
pub type CallingConvention = Cc<&'static str, &'static [&'static str]>;

/// A [`CallingConvention`] with its register names resolved to varnodes.
pub type BuiltCallingConvention = Cc<rsleigh::Vn, Vec<rsleigh::Vn>>;

/// For synthetic graphs: everything empty, plus a synthetic `stack_ptr`.
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
    /// This convention's stack geometry for a call that clobbers nothing and
    /// passes nothing (`__fentry__` / `mcount`).
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

    /// Every register preserved, memory not. Keeps the arguments, so a frame
    /// address passed to the callee is still seen to escape.
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

/// First entry of `a` sharing a byte with any entry of `b` (AArch64 `q8`
/// contains `d8`).
fn first_overlapping<'a>(a: &'a [rsleigh::Vn], b: &[rsleigh::Vn]) -> Option<&'a rsleigh::Vn> {
    a.iter()
        .find(|vn| b.iter().any(|other| vn_container::vn_overlaps(vn, other)))
}

/// First entry of `list` appearing twice, by equality: ARM's `d0..d3` share
/// two `q` containers.
fn first_dup(list: &[rsleigh::Vn]) -> Option<&rsleigh::Vn> {
    list.iter()
        .enumerate()
        .find(|(i, vn)| list[i + 1..].contains(vn))
        .map(|(_, vn)| vn)
}

impl BuiltCallingConvention {
    /// Whether `callee_saved_regs` covers every byte of `vn`: ARM `q4` is the
    /// preserved `d8`|`d9`, while AArch64 `q8` holds `d8` plus clobbered bits.
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

    /// A `Call`'s two output groups over `tracked_vns`: the clobbered return
    /// containers in ABI order, then every other clobbered REGISTER varnode
    /// in `tracked_vns` order. Clobbered means not the stack pointer and not
    /// wholly callee-saved, unless `preserves_all_registers`.
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
        // ARM `d0` and `d1` share `q0`; outputs must be unique.
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

    /// Slot `j` is the j-th float argument register's tracked container, or
    /// `None` if untracked. A container shared by several argument registers
    /// (AAPCS-VFP `d0`, `d1` in `q0`) yields the registers themselves.
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

    /// [`Self::float_arg_slots`] up to the first `None`.
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
    /// home. `None` when no argument is passed on the stack.
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

    /// [`Self::validate`], plus a callee-saved link register: the tradeoff
    /// that carries `InitialVar(lr)` through call sites, so a function's own
    /// convention, unlike a per-address callee's, needs it.
    ///
    /// # Errors
    /// As [`Self::validate`], or a link register missing from
    /// `callee_saved_regs`.
    pub fn validate_default(&self) -> Result<()> {
        self.validate()?;
        if let Some(lr) = self.link_register
            && !self.callee_saved_regs.contains(&lr)
        {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: link_register {lr:?} must also \
                 be present in callee_saved_regs, the deliberate tradeoff \
                 that makes InitialVar(lr) propagate through call sites; only \
                 a per-address convention may clobber it",
            ));
        }
        Ok(())
    }

    /// Checks the ABI invariants: no return registers under
    /// `preserves_all_registers`; callee-saved bytes disjoint from argument and
    /// return registers, int from float within each, SP from every list; no
    /// duplicates; sane stack geometry. A link register left out of
    /// `callee_saved_regs` is clobbered by a call to this convention (the
    /// ppc32 `-mbss-plt` GOT thunk). Advisory, since every field is `pub`.
    ///
    /// # Errors
    /// The first violation, naming the offending varnodes.
    pub fn validate(&self) -> Result<()> {
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
        if let Some(vn) = first_overlapping(&self.ret_val_regs, &self.ret_val_regs_float) {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: varnode {vn:?} in ret_val_regs shares \
                 bytes with ret_val_regs_float (integer and float return \
                 registers are physically distinct)",
            ));
        }
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
            // `slot_of` relies on it.
            if sa.base_offset < 0 {
                return Err(anyhow::anyhow!(
                    "BuiltCallingConvention: stack-arg base_offset must be >= 0, got {}",
                    sa.base_offset,
                ));
            }
        }
        Ok(())
    }

    /// Rejects stack geometry not of an arch with `addr_size`-byte addresses,
    /// since one arch's register names can resolve on another's table.
    ///
    /// # Errors
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

/// The N-th stack argument is at `base_offset + N * increment` from the
/// call-time SP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StackArgs {
    pub base_offset: i128,
    /// Always `> 0`.
    pub increment: i128,
}

impl StackArgs {
    /// Saturating.
    #[must_use]
    pub fn offset_of(&self, n: usize) -> i128 {
        self.base_offset
            .saturating_add((n as i128).saturating_mul(self.increment))
    }

    /// [`Self::offset_of`] from `sp_offset`. Saturating, so a caller stepping
    /// the series stops when the offset stops advancing.
    #[must_use]
    pub fn slot_offset(&self, sp_offset: i128, n: usize) -> i128 {
        sp_offset.saturating_add(self.offset_of(n))
    }

    /// The slot holding the start byte of an access at `offset`, or `None`
    /// below `base_offset`. A slot index, not an argument ordinal.
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

    /// Slots a `size`-byte argument occupies, saturating and `>= 1`.
    #[must_use]
    pub fn slots_spanned(&self, size: i128) -> usize {
        debug_assert!(
            self.increment > 0,
            "StackArgs::slots_spanned requires increment > 0"
        );
        let size = size.max(1);
        let numerator = size.saturating_add(self.increment - 1);
        clamp_to_usize(numerator / self.increment)
    }
}

/// Saturating i128 -> usize.
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
    /// The Ghidra prototypes the oracle test holds it to: (cspec path,
    /// prototype name or the default, the arches the cspec is for).
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
    /// The first name that does not resolve, or the first
    /// [`BuiltCallingConvention::validate`] violation.
    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
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

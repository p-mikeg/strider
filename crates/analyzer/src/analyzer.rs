use ir::node::NodeOutputType;
use ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
use rsleigh::Opcode;

use crate::error::{ErrorKind, Result};

/// Per-function translation context that converts a [`cfg::Cfg`] into an IR
/// graph region by region.
///
/// Holds a reference to the shared [`Analyzer`] (register / calling-convention
/// information) and a fresh [`ir::FunctionBuilder`].
pub struct IrAnalyzer<'a, R: rsleigh::MemReader> {
    pub(crate) analyzer: &'a Analyzer,
    pub(crate) builder: ir::FunctionBuilder,
    pub(crate) cfg: &'a cfg::Cfg<R>,
}

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    /// Creates a new `IrAnalyzer` for the given CFG.
    ///
    /// Collects all unique varnodes referenced by any instruction in `cfg` and
    /// constructs the IR [`FunctionBuilder`] with calling-convention
    /// information from `analyzer`.
    fn new(analyzer: &'a Analyzer, cfg: &'a cfg::Cfg<R>) -> Result<Self> {
        // Find all variables
        let all_vns = analyzer.find_all_unique_vns(cfg);

        // Create the builder to create the ir graph
        let builder = ir::FunctionBuilder::new(
            all_vns,
            &analyzer.calling_convention.arg_passing_regs,
            &analyzer.calling_convention.callee_saved_regs,
            &analyzer.calling_convention.ret_val_regs,
        )?;

        Ok(Self {
            analyzer,
            builder,
            cfg,
        })
    }

    /// Finds the largest architectural register that fully contains `reg`.
    ///
    /// For example, given `al` (offset 0, size 1) this returns `rax`
    /// (offset 0, size 8) because x86 reads/writes to `al` always go through
    /// `rax`.  The returned register is the widest one in the variable set
    /// that completely covers `reg`'s byte range.
    ///
    /// Returns `None` only if no variable in the builder covers the range,
    /// which should never happen because a register must cover itself.
    fn find_largest_fitting_register(&self, reg: &rsleigh::Vn) -> Result<Option<rsleigh::Vn>> {
        if reg.addr.space != rsleigh::VnSpace::REGISTER {
            return Err(ErrorKind::UnsupportedVnSpace(reg.addr.space).into());
        }
        let reg_start = reg.addr.off;
        let reg_end = reg_start + reg.size as u64;
        let mut largest_reg_container: Option<rsleigh::Vn> = None;
        for sleigh_reg in self.builder.variables() {
            if !matches!(sleigh_reg.addr.space, rsleigh::VnSpace::REGISTER) {
                continue;
            }
            let sleigh_reg_start = sleigh_reg.addr.off;
            let sleigh_reg_end = sleigh_reg_start + sleigh_reg.size as u64;

            if sleigh_reg_start > reg_start {
                continue;
            }
            if sleigh_reg_end < reg_end {
                continue;
            }
            // We know now that the reg is contained by sleigh reg
            if let Some(reg_container) = largest_reg_container {
                // If the current container is larger - choose it
                if reg_container.size < sleigh_reg.size {
                    largest_reg_container = Some(*sleigh_reg);
                }
            } else {
                largest_reg_container = Some(*sleigh_reg);
            }
        }
        Ok(largest_reg_container)
    }

    /// Computes the bit-shift needed to move `reg`'s bits to/from their
    /// position inside `container_reg`.
    ///
    /// For little-endian architectures the shift is simply
    /// `8 * (reg.off − container.off)`.  For big-endian the shift accounts
    /// for the container's total size so that the most-significant byte comes
    /// first.
    fn calculate_reg_shift_from_container(
        &self,
        reg: &rsleigh::Vn,
        container_reg: &rsleigh::Vn,
    ) -> u64 {
        match self.analyzer.arch.endianess {
            crate::arch::Endianess::Little => 8 * (reg.addr.off - container_reg.addr.off),
            crate::arch::Endianess::Big => {
                8 * (container_reg.size as u64 - (reg.addr.off - container_reg.addr.off))
            }
        }
    }

    /// Emits IR nodes to read the value of a register varnode.
    ///
    /// If `reg` is a sub-register (e.g. `al` inside `rax`) the method reads
    /// the container register and inserts a right-shift to extract the
    /// relevant bits.  If `reg` is already the container (or is its own
    /// largest container) the value is returned directly.
    fn read_reg_vn(&mut self, reg: &rsleigh::Vn) -> Result<ir::Value> {
        let container_reg = self
            .find_largest_fitting_register(reg)?
            .ok_or(ErrorKind::NoRegisterContainer(*reg))?;
        let curr_reg_val = self.builder.read_variable(&container_reg)?;
        let mut read_reg_val = curr_reg_val;
        if container_reg != *reg {
            // We need to shift the value if it is in the middle of a register
            let shift_value = self.calculate_reg_shift_from_container(reg, &container_reg);
            if shift_value != 0 {
                let shift_const = self
                    .builder
                    .build_int_const(shift_value, container_reg.size.try_into()?);
                read_reg_val = self.builder.build_int_binary_operation(
                    curr_reg_val,
                    shift_const,
                    IntBinaryOp::ShiftRight,
                    reg.size.try_into()?,
                )?;
            }
        }

        Ok(read_reg_val)
    }

    /// Emits IR nodes to write `val` into a register varnode.
    ///
    /// If `reg` is a sub-register the method:
    /// 1. Reads the current container value.
    /// 2. Shifts and masks `val` into the correct bit range.
    /// 3. Masks out the old bits of `reg` inside the container.
    /// 4. ORs the two together and writes back to the container.
    ///
    /// If `reg` is equal to its own container the write is direct.
    fn write_reg_vn(&mut self, reg: &rsleigh::Vn, val: ir::Value) -> Result<()> {
        let container_reg = self
            .find_largest_fitting_register(reg)?
            .ok_or(ErrorKind::NoRegisterContainer(*reg))?;
        if container_reg == *reg {
            return Ok(self.builder.write_variable(reg, val)?);
        }
        let container_reg_val = self.builder.read_variable(&container_reg)?;

        // The register is in the part of a bigger container
        let shift_bits = self.calculate_reg_shift_from_container(reg, &container_reg);

        // Calculate the shifted value that should be in the reg inside the container
        let shifted_value = if shift_bits == 0 {
            self.builder.extend_if_needed(
                val,
                container_reg.size.try_into()?,
                ExtendOp::ZeroExtend,
            )?
        } else {
            let shift_const = self
                .builder
                .build_int_const(shift_bits, container_reg.size.try_into()?);
            self.builder.build_int_binary_operation(
                val,
                shift_const,
                IntBinaryOp::ShiftLeft,
                reg.size.try_into()?,
            )?
        };

        // Calculate the masked value of the reg in the container
        let reg_mask = crate::utils::vn_mask(reg)?;
        let reg_mask_val = self
            .builder
            .build_int_const(reg_mask, container_reg.size.try_into()?);
        let reg_val = self.builder.build_int_binary_operation(
            reg_mask_val,
            shifted_value,
            IntBinaryOp::And,
            container_reg.size.try_into()?,
        )?;

        // Calculate the rest of the container
        let container_mask = crate::utils::vn_mask(&container_reg)? & (!reg_mask);
        let container_mask_val = self
            .builder
            .build_int_const(container_mask, container_reg.size.try_into()?);
        let container_val = self.builder.build_int_binary_operation(
            container_mask_val,
            container_reg_val,
            IntBinaryOp::And,
            container_reg.size.try_into()?,
        )?;

        // Merge the containers
        let final_container_value = self.builder.build_int_binary_operation(
            container_val,
            reg_val,
            IntBinaryOp::Or,
            container_reg.size.try_into()?,
        )?;
        self.write_reg_vn(&container_reg, final_container_value)?;
        Ok(())
    }

    /// Reads any varnode into an IR value.
    ///
    /// Dispatches based on the varnode's address space:
    /// - `CONST` → an integer constant node.
    /// - `UNIQUE` → read the SSA variable directly.
    /// - default code space → a [`NodeKind::Load`] from the code address space.
    /// - `REGISTER` → delegates to [`read_reg_vn`] for aliasing handling.
    fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<ir::Value> {
        let default_code_space = self.cfg.sleigh.default_code_space();
        let space = vn.addr.space;
        match space {
            rsleigh::VnSpace::CONST => Ok(self
                .builder
                .build_int_const(vn.addr.off, vn.size.try_into()?)),
            rsleigh::VnSpace::UNIQUE => Ok(self.builder.read_variable(vn)?),
            space if space == default_code_space => {
                let space_info = self.cfg.sleigh.space_info(space);
                let addr = self
                    .builder
                    .build_int_const(vn.addr.off, space_info.addr_size().try_into()?);
                Ok(self.builder.build_load(addr, space, vn.size.try_into()?)?)
            }
            rsleigh::VnSpace::REGISTER => self.read_reg_vn(vn),
            _ => Err(ErrorKind::UnsupportedVnSpace(space).into()),
        }
    }

    /// Writes an IR value into any writable varnode.
    ///
    /// Dispatches based on the varnode's address space:
    /// - `CONST` → error (constants cannot be written).
    /// - `UNIQUE` → write the SSA variable directly.
    /// - default code space → a [`NodeKind::Store`] to the code address space.
    /// - `REGISTER` → delegates to [`write_reg_vn`] for aliasing handling.
    fn write_vn(&mut self, vn: &rsleigh::Vn, val: ir::Value) -> Result<()> {
        let default_code_space = self.cfg.sleigh.default_code_space();
        let space = vn.addr.space;
        match space {
            rsleigh::VnSpace::CONST => Err(ErrorKind::WriteToConstSpace(space).into()),
            rsleigh::VnSpace::UNIQUE => Ok(self.builder.write_variable(vn, val)?),
            space if space == default_code_space => {
                let space_info = self.cfg.sleigh.space_info(space);
                let addr = self
                    .builder
                    .build_int_const(vn.addr.off, space_info.addr_size().try_into()?);
                Ok(self.builder.build_store(addr, val, space)?)
            }
            rsleigh::VnSpace::REGISTER => self.write_reg_vn(vn, val),
            _ => Err(ErrorKind::UnsupportedVnSpace(space).into()),
        }
    }

    /// Emits the function entry node into the IR graph.
    fn build_entry(&mut self) -> Result<()> {
        Ok(self.builder.build_entry()?)
    }

    /// Translates a p-code integer unary instruction into an IR unary node and
    /// writes the result to the output varnode.
    fn process_int_unary_op(&mut self, insn: &rsleigh::Insn, op: IntUnaryOp) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self
            .builder
            .build_int_unary_operation(input, op, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    /// Translates a p-code integer binary instruction into an IR binary node
    /// and writes the result to the output varnode.
    fn process_int_binary_op(&mut self, insn: &rsleigh::Insn, op: IntBinaryOp) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self
            .builder
            .build_int_binary_operation(lhs, rhs, op, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    /// Translates a p-code integer comparison instruction into an IR
    /// comparison node and writes the boolean result to the output varnode.
    fn process_int_cmp_op(&mut self, insn: &rsleigh::Insn, op: IntCmpOp) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out =
            self.builder
                .build_int_cmp_operation(lhs, rhs, op, insn.inputs[0].size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    /// Translates a p-code boolean binary instruction into an IR boolean
    /// operation node and writes the result to the output varnode.
    fn process_bool_binary_op(&mut self, insn: &rsleigh::Insn, op: BoolBinaryOp) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self.builder.build_boolean_operation(lhs, rhs, op)?;
        self.write_vn(out_vn, out)
    }

    /// Translates a p-code boolean unary instruction into an IR boolean
    /// unary node and writes the result to the output varnode.
    fn process_bool_unary_op(&mut self, insn: &rsleigh::Insn, op: BoolUnaryOp) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self.builder.build_boolean_unary_operation(input, op)?;
        self.write_vn(out_vn, out)
    }

    /// Translates a p-code zero-extend or sign-extend instruction into an IR
    /// extend node and writes the result to the output varnode.
    fn process_extend(&mut self, insn: &rsleigh::Insn, op: ExtendOp) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self
            .builder
            .extend_if_needed(input, out_vn.size.try_into()?, op)?;
        self.write_vn(out_vn, out)
    }

    // ── Float helpers ─────────────────────────────────────────────────────────

    /// Maps a varnode byte size to the corresponding float [`NodeOutputType`].
    /// Returns an error for sizes other than 4 (F32) or 8 (F64).
    fn float_type_from_vn(vn: &rsleigh::Vn) -> Result<NodeOutputType> {
        match vn.size {
            4 => Ok(NodeOutputType::F32),
            8 => Ok(NodeOutputType::F64),
            n => Err(ErrorKind::UnsupportedFloatSize(n).into()),
        }
    }

    /// Bitcasts a float result back to an integer of the same width and writes
    /// it to the output varnode (float results in registers are stored as ints).
    fn write_float_to_vn(&mut self, vn: &rsleigh::Vn, float_val: ir::Value) -> Result<()> {
        let int_ty: NodeOutputType = vn.size.try_into()?;
        let int_val = self.builder.build_float_bits_to_int(float_val, int_ty)?;
        self.write_vn(vn, int_val)
    }

    /// Translates a float binary p-code instruction into an IR float binary node.
    ///
    /// Reads inputs via `read_vn` (may produce int or float values); the builder
    /// automatically inserts `CastToFloat` nodes as needed.
    fn process_float_binary_op(&mut self, insn: &rsleigh::Insn, op: FloatBinaryOp) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let float_ty = Self::float_type_from_vn(out_vn)?;
        let result = self.builder.build_float_binary_op(lhs, rhs, op, float_ty)?;
        self.write_float_to_vn(out_vn, result)
    }

    /// Translates a float unary p-code instruction into an IR float unary node.
    fn process_float_unary_op(&mut self, insn: &rsleigh::Insn, op: FloatUnaryOp) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let float_ty = Self::float_type_from_vn(out_vn)?;
        let result = self.builder.build_float_unary_op(input, op, float_ty)?;
        self.write_float_to_vn(out_vn, result)
    }

    /// Translates a float comparison p-code instruction into an IR float cmp node.
    fn process_float_cmp_op(&mut self, insn: &rsleigh::Insn, op: FloatCmpOp) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let result = self.builder.build_float_cmp_op(lhs, rhs, op)?;
        self.write_vn(out_vn, result)
    }

    /// Translates a single p-code instruction `insn` from `region_id` into
    /// one or more IR nodes.
    ///
    /// Matches on the opcode and delegates to the appropriate `process_*`
    /// helper or inline logic.  `region_lookup` resolves a CFG region id to its
    /// IR counterpart; it is called only for branch and conditional-branch
    /// opcodes.  Unimplemented opcodes return an error.
    fn process_insn<F>(
        &mut self,
        region_id: cfg::RegionId,
        insn: &rsleigh::Insn,
        region_lookup: F,
    ) -> Result<()>
    where
        F: Fn(cfg::RegionId) -> Result<ir::RegionId>,
    {
        match insn.opcode {
            Opcode::Nop => {}
            Opcode::BoolNeg => self.process_bool_unary_op(insn, BoolUnaryOp::Neg)?,
            Opcode::BoolAnd => self.process_bool_binary_op(insn, BoolBinaryOp::And)?,
            Opcode::BoolOr => self.process_bool_binary_op(insn, BoolBinaryOp::Or)?,
            Opcode::BoolXor => self.process_bool_binary_op(insn, BoolBinaryOp::Xor)?,
            Opcode::Int2Comp => self.process_int_unary_op(insn, IntUnaryOp::Not)?,
            Opcode::IntNeg => self.process_int_unary_op(insn, IntUnaryOp::Neg)?,
            Opcode::IntAdd => self.process_int_binary_op(insn, IntBinaryOp::Add)?,
            Opcode::IntAnd => self.process_int_binary_op(insn, IntBinaryOp::And)?,
            Opcode::IntXor => self.process_int_binary_op(insn, IntBinaryOp::Xor)?,
            Opcode::IntOr => self.process_int_binary_op(insn, IntBinaryOp::Or)?,
            Opcode::IntDiv => self.process_int_binary_op(insn, IntBinaryOp::Div)?,
            Opcode::IntSdiv => self.process_int_binary_op(insn, IntBinaryOp::Sdiv)?,
            Opcode::IntMul => self.process_int_binary_op(insn, IntBinaryOp::Mul)?,
            Opcode::IntRight => self.process_int_binary_op(insn, IntBinaryOp::ShiftRight)?,
            Opcode::IntSright => self.process_int_binary_op(insn, IntBinaryOp::SShiftRight)?,
            Opcode::IntLeft => self.process_int_binary_op(insn, IntBinaryOp::ShiftLeft)?,
            Opcode::IntCarry => self.process_int_cmp_op(insn, IntCmpOp::Carry)?,
            Opcode::IntEqual => self.process_int_cmp_op(insn, IntCmpOp::Equal)?,
            Opcode::IntLess => self.process_int_cmp_op(insn, IntCmpOp::Less)?,
            Opcode::IntSless => self.process_int_cmp_op(insn, IntCmpOp::Sless)?,
            Opcode::IntLessEqual => self.process_int_cmp_op(insn, IntCmpOp::LessEqual)?,
            Opcode::IntRem => self.process_int_binary_op(insn, IntBinaryOp::Rem)?,
            Opcode::IntSrem => self.process_int_binary_op(insn, IntBinaryOp::Srem)?,
            Opcode::IntScarry => self.process_int_cmp_op(insn, IntCmpOp::Scarry)?,
            Opcode::IntSborrow => self.process_int_cmp_op(insn, IntCmpOp::Sborrow)?,
            Opcode::IntSlessEqual => self.process_int_cmp_op(insn, IntCmpOp::SlessEqual)?,
            Opcode::IntSub => self.process_int_binary_op(insn, IntBinaryOp::Sub)?,
            Opcode::IntSext => self.process_extend(insn, ExtendOp::SignExtend)?,
            Opcode::IntZext => self.process_extend(insn, ExtendOp::ZeroExtend)?,
            Opcode::IntNotEqual => {
                // We treat not equal as neg(equal) for deterministic results
                let lhs = self.read_vn(&insn.inputs[0])?;
                let rhs = self.read_vn(&insn.inputs[1])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let and_out = self.builder.build_int_cmp_operation(
                    lhs,
                    rhs,
                    IntCmpOp::Equal,
                    out_vn.size.try_into()?,
                )?;
                let out = self
                    .builder
                    .build_boolean_unary_operation(and_out, BoolUnaryOp::Neg)?;
                self.write_vn(out_vn, out)?;
            }
            Opcode::Branch => {
                let branch_region = self
                    .cfg
                    .region_branch(region_id)?
                    .ok_or(cfg::Error::from(cfg::ErrorKind::InvalidRegion(region_id)))?;
                let dest_block = region_lookup(branch_region)?;
                self.builder.build_branch(dest_block)?;
            }
            Opcode::CondBranch => {
                let cond = self.read_vn(&insn.inputs[1])?;
                let res = self.cfg.region_if(region_id)?;
                let if_true_region = res
                    .if_true_region
                    .ok_or(cfg::Error::from(cfg::ErrorKind::InvalidRegion(region_id)))?;
                let if_false_region = res
                    .if_false_region
                    .ok_or(cfg::Error::from(cfg::ErrorKind::InvalidRegion(region_id)))?;
                let true_block = region_lookup(if_true_region)?;
                let false_block = region_lookup(if_false_region)?;
                self.builder.build_if(cond, true_block, false_block)?;
            }
            Opcode::Copy => {
                let input = self.read_vn(&insn.inputs[0])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                self.write_vn(out_vn, input)?;
            }
            Opcode::Load => {
                let space = insn.inputs[0].addr.space;
                let addr = self.read_vn(&insn.inputs[1])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out = self
                    .builder
                    .build_load(addr, space, out_vn.size.try_into()?)?;
                self.write_vn(out_vn, out)?;
            }
            Opcode::Store => {
                let space = insn.inputs[0].addr.space;
                let addr = self.read_vn(&insn.inputs[1])?;
                let data = self.read_vn(&insn.inputs[2])?;
                self.builder.build_store(addr, data, space)?;
            }
            Opcode::Return => {
                let regs: Vec<_> = self
                    .builder
                    .variables()
                    .copied()
                    .filter(|vn| vn.addr.space == rsleigh::VnSpace::REGISTER)
                    .collect();
                if !insn.inputs.is_empty() {
                    let ret_val = self.read_vn(&insn.inputs[0])?;
                    self.builder.build_return(Some(ret_val), &regs)?;
                } else {
                    self.builder.build_return(None, &regs)?;
                }
            }
            Opcode::Call => {
                // Direct call: the target varnode is in the code space and its offset
                // *is* the target address — it's not a pointer to dereference.
                let target_vn = &insn.inputs[0];
                let space_info = self.cfg.sleigh.space_info(target_vn.addr.space);
                let call_address = self
                    .builder
                    .build_int_const(target_vn.addr.off, space_info.addr_size().try_into()?);
                self.builder.build_call(call_address)?;
            }
            Opcode::CallIndirect => {
                // Indirect call: target is a register/memory value holding the address.
                let call_address = self.read_vn(&insn.inputs[0])?;
                self.builder.build_call(call_address)?;
            }
            Opcode::Subpiece => {
                // `Subpiece(value, byte_offset, out_size)`: extracts `out_size` bytes
                // starting at byte `byte_offset` from `value`.
                // Implemented as: right-shift by (byte_offset * 8) bits, then truncate.
                let input = self.read_vn(&insn.inputs[0])?;
                let byte_offset = insn.inputs[1].addr.off;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let shifted = if byte_offset == 0 {
                    input
                } else {
                    let bit_shift = byte_offset * 8;
                    let shift_const = self
                        .builder
                        .build_int_const(bit_shift, insn.inputs[0].size.try_into()?);
                    self.builder.build_int_binary_operation(
                        input,
                        shift_const,
                        IntBinaryOp::ShiftRight,
                        insn.inputs[0].size.try_into()?,
                    )?
                };
                let out = self
                    .builder
                    .truncate_if_needed(shifted, out_vn.size.try_into()?)?;
                self.write_vn(out_vn, out)?;
            }
            Opcode::Popcount => {
                let input = self.read_vn(&insn.inputs[0])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out = self
                    .builder
                    .build_popcount(input, out_vn.size.try_into()?)?;
                self.write_vn(out_vn, out)?;
            }
            Opcode::Lzcount => {
                let input = self.read_vn(&insn.inputs[0])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out = self.builder.build_lzcount(input, out_vn.size.try_into()?)?;
                self.write_vn(out_vn, out)?;
            }
            Opcode::Piece => {
                // inputs[0] = hi (most significant), inputs[1] = lo (least significant).
                // Lowered to: Or(ShiftLeft(ZeroExtend(hi), lo_bits), ZeroExtend(lo)).
                let hi = self.read_vn(&insn.inputs[0])?;
                let lo = self.read_vn(&insn.inputs[1])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out_ty: NodeOutputType = out_vn.size.try_into()?;
                let hi_ty = self.builder.get_output_type(hi)?.to_natural_int_type();
                let hi_int = self.builder.convert_to_int_if_needed(hi, hi_ty)?;
                let lo_ty = self.builder.get_output_type(lo)?.to_natural_int_type();
                let lo_int = self.builder.convert_to_int_if_needed(lo, lo_ty)?;
                let lo_bits = lo_ty.bit_width() as u64;
                let hi_wide = self.builder.convert_to_int_if_needed(hi_int, out_ty)?;
                let lo_wide = self.builder.convert_to_int_if_needed(lo_int, out_ty)?;
                let shift_amt = self.builder.build_int_const(lo_bits, out_ty);
                let hi_shifted = self.builder.build_int_binary_operation(
                    hi_wide,
                    shift_amt,
                    IntBinaryOp::ShiftLeft,
                    out_ty,
                )?;
                let out = self.builder.build_int_binary_operation(
                    hi_shifted,
                    lo_wide,
                    IntBinaryOp::Or,
                    out_ty,
                )?;
                self.write_vn(out_vn, out)?;
            }
            Opcode::Extract => {
                // inputs[0] = value, inputs[1] = lsb (CONST), inputs[2] = bit_count (CONST)
                // Lowered to: Truncate(ShiftRight(x, lsb), narrow_ty), with an extra
                // And mask when len < narrow_ty.bit_width() to preserve "upper bits zero".
                let input = self.read_vn(&insn.inputs[0])?;
                let lsb = insn.inputs[1].addr.off as u8;
                let len = insn.inputs[2].addr.off as u8;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let narrow_ty: NodeOutputType = out_vn.size.try_into()?;
                let x_nat_ty = self.builder.get_output_type(input)?.to_natural_int_type();
                let x_int = self.builder.convert_to_int_if_needed(input, x_nat_ty)?;
                let shifted = if lsb == 0 {
                    x_int
                } else {
                    let lsb_const = self.builder.build_int_const(lsb as u64, x_nat_ty);
                    self.builder.build_int_binary_operation(
                        x_int,
                        lsb_const,
                        IntBinaryOp::ShiftRight,
                        x_nat_ty,
                    )?
                };
                let narrowed = self.builder.truncate_if_needed(shifted, narrow_ty)?;
                let out = if (len as usize) < narrow_ty.bit_width() {
                    let mask_val = if len >= 64 {
                        u64::MAX
                    } else {
                        (1u64 << len) - 1
                    };
                    let mask = self.builder.build_int_const(mask_val, narrow_ty);
                    self.builder.build_int_binary_operation(
                        narrowed,
                        mask,
                        IntBinaryOp::And,
                        narrow_ty,
                    )?
                } else {
                    narrowed
                };
                self.write_vn(out_vn, out)?;
            }
            Opcode::Insert => {
                // inputs[0] = dest, inputs[1] = src, inputs[2] = lsb (CONST), inputs[3] = bit_count (CONST)
                let dest = self.read_vn(&insn.inputs[0])?;
                let src = self.read_vn(&insn.inputs[1])?;
                let lsb = insn.inputs[2].addr.off as u8;
                let len = insn.inputs[3].addr.off as u8;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out =
                    self.builder
                        .build_insert(dest, src, lsb, len, out_vn.size.try_into()?)?;
                self.write_vn(out_vn, out)?;
            }
            // PtrAdd: out = base + index * elem_size  (elem_size is a CONST input)
            Opcode::PtrAdd => {
                let base = self.read_vn(&insn.inputs[0])?;
                let index = self.read_vn(&insn.inputs[1])?;
                let elem_size = insn.inputs[2].addr.off;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out_ty: ir::ValueType = out_vn.size.try_into()?;
                let elem_const = self.builder.build_int_const(elem_size, out_ty);
                let scaled = self.builder.build_int_binary_operation(
                    index,
                    elem_const,
                    IntBinaryOp::Mul,
                    out_ty,
                )?;
                let out = self.builder.build_int_binary_operation(
                    base,
                    scaled,
                    IntBinaryOp::Add,
                    out_ty,
                )?;
                self.write_vn(out_vn, out)?;
            }
            // PtrSub: out = base - index
            Opcode::PtrSub => {
                let base = self.read_vn(&insn.inputs[0])?;
                let index = self.read_vn(&insn.inputs[1])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out = self.builder.build_int_binary_operation(
                    base,
                    index,
                    IntBinaryOp::Sub,
                    out_vn.size.try_into()?,
                )?;
                self.write_vn(out_vn, out)?;
            }
            // ── Float arithmetic ──────────────────────────────────────────────
            Opcode::FloatAdd => self.process_float_binary_op(insn, FloatBinaryOp::Add)?,
            Opcode::FloatSub => self.process_float_binary_op(insn, FloatBinaryOp::Sub)?,
            Opcode::FloatMul => self.process_float_binary_op(insn, FloatBinaryOp::Mul)?,
            Opcode::FloatDiv => self.process_float_binary_op(insn, FloatBinaryOp::Div)?,

            // ── Float unary (float → float) ───────────────────────────────────
            Opcode::FloatNeg => self.process_float_unary_op(insn, FloatUnaryOp::Neg)?,
            Opcode::FloatAbs => self.process_float_unary_op(insn, FloatUnaryOp::Abs)?,
            Opcode::FloatSqrt => self.process_float_unary_op(insn, FloatUnaryOp::Sqrt)?,
            Opcode::FloatCeil => self.process_float_unary_op(insn, FloatUnaryOp::Ceil)?,
            Opcode::FloatFloor => self.process_float_unary_op(insn, FloatUnaryOp::Floor)?,
            Opcode::FloatRound => self.process_float_unary_op(insn, FloatUnaryOp::Round)?,

            // ── Float comparisons (→ bool) ────────────────────────────────────
            Opcode::FloatEqual => self.process_float_cmp_op(insn, FloatCmpOp::Equal)?,
            Opcode::FloatNotEqual => self.process_float_cmp_op(insn, FloatCmpOp::NotEqual)?,
            Opcode::FloatLess => self.process_float_cmp_op(insn, FloatCmpOp::Less)?,
            Opcode::FloatLessEqual => self.process_float_cmp_op(insn, FloatCmpOp::LessEqual)?,

            // FloatNan: tests whether input is NaN (unary, → bool).
            // Emitted as FloatCmpOp::NotEqual(x, x) since IEEE 754 guarantees
            // NaN != NaN == true (and x != x is false for all non-NaN x).
            Opcode::FloatNan => {
                let input = self.read_vn(&insn.inputs[0])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let result = self
                    .builder
                    .build_float_cmp_op(input, input, FloatCmpOp::NotEqual)?;
                self.write_vn(out_vn, result)?;
            }

            // ── Float / integer conversions ───────────────────────────────────

            // FloatInt2Float: convert integer value to float (e.g. (float)42)
            Opcode::FloatInt2Float => {
                let int_input = self.read_vn(&insn.inputs[0])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let float_ty = Self::float_type_from_vn(out_vn)?;
                let float_result = self.builder.build_int_to_float(int_input, float_ty)?;
                self.write_float_to_vn(out_vn, float_result)?;
            }

            // FloatFloat2Float: change float precision (F32 ↔ F64)
            Opcode::FloatFloat2Float => {
                let float_input = self.read_vn(&insn.inputs[0])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out_float_ty = Self::float_type_from_vn(out_vn)?;
                let float_result = self
                    .builder
                    .build_float_to_float(float_input, out_float_ty)?;
                self.write_float_to_vn(out_vn, float_result)?;
            }

            // FloatTrunc: truncate float toward zero to integer (e.g. (int)f)
            Opcode::FloatTrunc => {
                let float_input = self.read_vn(&insn.inputs[0])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let int_ty: NodeOutputType = out_vn.size.try_into()?;
                let int_result = self.builder.build_float_to_int(float_input, int_ty)?;
                self.write_vn(out_vn, int_result)?;
            }

            // ── remaining Sleigh opcodes ──────────────────────────────────────

            // Cast: apply a data-type to the output varnode.  GHIDRA docs:
            // "semantically equivalent to a COPY operation".
            Opcode::Cast => {
                let input = self.read_vn(&insn.inputs[0])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                self.write_vn(out_vn, input)?;
            }

            // MultiEqual is a decompiler-internal phi; raw p-code should not
            // contain it.  Report instead of guessing semantics.
            Opcode::MultiEqual => {
                return Err(ErrorKind::UnexpectedDecompilerOpcode(insn.opcode).into());
            }

            // CallOther: user-defined CPU intrinsic (cpuid, rdtsc, syscall, …).
            // inputs[0] is a CONST user-op id; remaining inputs are arguments.
            // Clobbers memory.  The instruction's output varnode, if present,
            // receives the intrinsic's result value.
            Opcode::CallOther => {
                if insn.inputs.is_empty() {
                    return Err(ErrorKind::TooFewInputs(insn.opcode, 1, 0).into());
                }
                let id_vn = &insn.inputs[0];
                if id_vn.addr.space != rsleigh::VnSpace::CONST {
                    return Err(ErrorKind::ExpectedConstInput(insn.opcode, 0).into());
                }
                let user_op_id = id_vn.addr.off;
                let args: Vec<ir::Value> = insn.inputs[1..]
                    .iter()
                    .map(|vn| self.read_vn(vn))
                    .collect::<Result<_>>()?;
                let output_ty: Option<NodeOutputType> = match insn.output.as_ref() {
                    Some(out_vn) => Some(out_vn.size.try_into()?),
                    None => None,
                };
                let result = self
                    .builder
                    .build_call_other(user_op_id, &args, output_ty)?;
                if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), result) {
                    self.write_vn(out_vn, val)?;
                }
            }

            // SegmentOp: segmented-address lookup.
            // inputs[0] = CONST op id, inputs[1] = segment, inputs[2] = offset.
            Opcode::SegmentOp => {
                if insn.inputs.len() < 3 {
                    return Err(ErrorKind::TooFewInputs(insn.opcode, 3, insn.inputs.len()).into());
                }
                let id_vn = &insn.inputs[0];
                if id_vn.addr.space != rsleigh::VnSpace::CONST {
                    return Err(ErrorKind::ExpectedConstInput(insn.opcode, 0).into());
                }
                let op_id = id_vn.addr.off;
                let segment = self.read_vn(&insn.inputs[1])?;
                let offset = self.read_vn(&insn.inputs[2])?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out = self.builder.build_segment_op(
                    op_id,
                    segment,
                    offset,
                    out_vn.size.try_into()?,
                )?;
                self.write_vn(out_vn, out)?;
            }

            // CPoolRef: JVM constant-pool lookup.  Opaque, variadic refs.
            Opcode::CPoolRef => {
                let refs: Vec<ir::Value> = insn
                    .inputs
                    .iter()
                    .map(|vn| self.read_vn(vn))
                    .collect::<Result<_>>()?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out = self
                    .builder
                    .build_cpool_ref(&refs, out_vn.size.try_into()?)?;
                self.write_vn(out_vn, out)?;
            }

            // New: JVM object allocation.  Opaque.
            Opcode::New => {
                let args: Vec<ir::Value> = insn
                    .inputs
                    .iter()
                    .map(|vn| self.read_vn(vn))
                    .collect::<Result<_>>()?;
                let out_vn = insn
                    .output
                    .as_ref()
                    .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
                let out = self.builder.build_new(&args, out_vn.size.try_into()?)?;
                self.write_vn(out_vn, out)?;
            }

            _ => return Err(ErrorKind::UnimplementedOpcode(insn.opcode).into()),
        }
        Ok(())
    }
}

/// Architecture-level binary analyser that lifts a [`cfg::Cfg`] to an IR
/// function graph.
///
/// Holds the target architecture description and the resolved calling
/// convention.  Create one `Analyzer` per architecture/ABI combination and
/// reuse it to analyse multiple functions.
pub struct Analyzer {
    calling_convention: crate::calling_convention::BuiltCallingConvention,
    arch: crate::SleighArch,
}

impl Analyzer {
    /// Creates a new `Analyzer` for `arch` with the given Sleigh register list
    /// and calling convention.
    ///
    /// Resolves all register names in `calling_convention` against
    /// `sleigh_regs`.  Returns an error if any name is unknown.
    pub fn new(
        arch: crate::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: crate::CallingConvention,
    ) -> Result<Self> {
        let built_calling_convention = calling_convention.build(&sleigh_regs)?;
        Ok(Self {
            arch,
            calling_convention: built_calling_convention,
        })
    }

    /// Returns the resolved calling convention this analyzer was built with.
    pub fn calling_convention(&self) -> &crate::calling_convention::BuiltCallingConvention {
        &self.calling_convention
    }

    /// Builds an optimizer pipeline containing the default passes plus the
    /// convention-aware stack-argument passes:
    ///
    /// 1. All passes from [`opt::default_pipeline`] (constant folding,
    ///    known-bits, redundant-phi, dead-branch).
    /// 2. [`opt::StackStoreDetect`] inside the fixed-point loop, using the
    ///    convention's stack-pointer varnode.
    /// 3. [`opt::CallStackArgCollect`] as a post-pass (runs once after
    ///    convergence), using the convention's positional stack-arg offsets.
    pub fn build_optimizer_pipeline(&self) -> opt::OptimizerPipeline {
        let mut p = opt::default_pipeline();
        p.add(opt::StackStoreDetect::new(
            self.calling_convention.stack_ptr_vn,
        ));
        p.add_post_pass(opt::CallStackArgCollect::new(
            self.calling_convention.stack_arg_offsets.clone(),
        ));
        p
    }

    /// Collects the set of all distinct varnodes referenced by any instruction
    /// across all regions of `cfg`.
    ///
    /// The result is used to pre-declare every variable the IR builder must
    /// track.
    fn find_all_unique_vns<R: rsleigh::MemReader>(&self, cfg: &cfg::Cfg<R>) -> Vec<rsleigh::Vn> {
        let mut all_vns: std::collections::HashSet<rsleigh::Vn> = std::collections::HashSet::new();
        for region in cfg.regions() {
            for wrapped in region.insns.iter() {
                for vn in wrapped.insn.all_vns() {
                    all_vns.insert(vn);
                }
            }
        }
        all_vns.iter().copied().collect()
    }

    /// Translates a complete control-flow graph into an [`ir::BuiltFunctionGraph`].
    ///
    /// The pipeline:
    /// 1. Build the function entry node.
    /// 2. Map the CFG graph: each node stores `Option<ir::RegionId>` (None first,
    ///    then filled in fallibly via `node_weight_mut`).
    /// 3. Set the entry region.
    /// 4. Translate instructions in each region, resolving branch targets via
    ///    the mapped graph.
    /// 5. Link fallthrough edges by iterating `cfg_ir_graph`'s edges.
    pub fn analyze_cfg<R: rsleigh::MemReader>(
        &self,
        cfg: &cfg::Cfg<R>,
    ) -> Result<ir::BuiltFunctionGraph> {
        let mut ir_analyzer = IrAnalyzer::new(self, cfg)?;

        ir_analyzer.build_entry()?;

        // Step 1: create the structural clone of the CFG graph with None placeholders.
        // The map closure is infallible; IR region creation happens below.
        let mut cfg_ir_graph = cfg.graph.map(|_, _| None::<ir::RegionId>, |_, e| *e);

        // Step 2: fill in IR regions (fallible).
        for region_id in cfg.region_ids() {
            let ir_region = ir_analyzer.builder.create_region()?;
            *cfg_ir_graph
                .node_weight_mut(region_id)
                .ok_or(ErrorKind::CfgNoRegion(region_id))? = Some(ir_region);
        }

        // Helper closure: map a CFG region id to its IR region id via the graph.
        let ir_region_of = |region_id: cfg::RegionId| -> Result<ir::RegionId> {
            cfg_ir_graph
                .node_weight(region_id)
                .copied()
                .flatten()
                .ok_or_else(|| ErrorKind::CfgNoRegion(region_id).into())
        };

        // Set entry region.
        ir_analyzer
            .builder
            .set_entry_region(ir_region_of(cfg.entry)?)?;

        // Translate instructions for each region.
        for node_idx in cfg_ir_graph.node_indices() {
            let ir_region = ir_region_of(node_idx)?;
            ir_analyzer.builder.set_region(ir_region);
            let region = cfg
                .graph
                .node_weight(node_idx)
                .ok_or(ErrorKind::CfgNoRegion(node_idx))?;
            for wrapped_insn in &region.insns {
                ir_analyzer.process_insn(node_idx, &wrapped_insn.insn, ir_region_of)?;
            }
        }

        // Link fallthrough edges by inspecting cfg_ir_graph's edges directly.
        for edge_idx in cfg_ir_graph.edge_indices() {
            let Some(weight) = cfg_ir_graph.edge_weight(edge_idx) else {
                continue;
            };
            if *weight != cfg::RegionEdgeKind::Fallthrough {
                continue;
            }
            let Some((src, tgt)) = cfg_ir_graph.edge_endpoints(edge_idx) else {
                continue;
            };
            ir_analyzer
                .builder
                .link_regions(ir_region_of(src)?, ir_region_of(tgt)?)?;
        }

        Ok(ir_analyzer.builder.build()?)
    }
}

use std::fmt;

use rsleigh::{Insn, MemReader, Opcode, Sleigh, SleighRegs, SpaceIds, Vn, VnSpace};

/// `insn` in rsleigh's contextual spelling (`Store ram, RSP, RAX`), a LOAD /
/// STORE space id rendered as its space's name rather than the host address of
/// the engine's `AddrSpace`. `space_ids` must come from the engine that decoded
/// `insn`; `sleigh` and `regs` only name things, so any engine of the arch does.
pub fn insn_text<'a, R: MemReader>(
    insn: &'a Insn,
    space_ids: &'a SpaceIds,
    sleigh: &'a Sleigh<R>,
    regs: &'a SleighRegs,
) -> impl fmt::Display + 'a {
    CtxInsnText {
        insn,
        space_ids,
        sleigh,
        regs,
    }
}

/// [`insn_text`] in rsleigh's plain `Display` spelling (`Store r, %[0x20]:8, %[0x0]:8`),
/// the space id rendered as the space's shortcut.
pub fn insn_plain_text<'a>(insn: &'a Insn, space_ids: &'a SpaceIds) -> impl fmt::Display + 'a {
    PlainInsnText { insn, space_ids }
}

struct CtxInsnText<'a, R: MemReader> {
    insn: &'a Insn,
    space_ids: &'a SpaceIds,
    sleigh: &'a Sleigh<R>,
    regs: &'a SleighRegs,
}

impl<R: MemReader> fmt::Display for CtxInsnText<'_, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_insn(
            f,
            self.insn,
            self.space_ids,
            |f, space| write!(f, "{}", space.ctx_fmt(self.sleigh)),
            |f, vn| write!(f, "{}", vn.ctx_fmt(self.sleigh, self.regs)),
        )
    }
}

struct PlainInsnText<'a> {
    insn: &'a Insn,
    space_ids: &'a SpaceIds,
}

impl fmt::Display for PlainInsnText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_insn(
            f,
            self.insn,
            self.space_ids,
            |f, space| write!(f, "{space}"),
            |f, vn| write!(f, "{vn}"),
        )
    }
}

fn write_insn(
    f: &mut fmt::Formatter<'_>,
    insn: &Insn,
    space_ids: &SpaceIds,
    space: impl Fn(&mut fmt::Formatter<'_>, VnSpace) -> fmt::Result,
    operand: impl Fn(&mut fmt::Formatter<'_>, Vn) -> fmt::Result,
) -> fmt::Result {
    write!(f, "{}", insn.opcode)?;
    let space_id = matches!(insn.opcode, Opcode::Load | Opcode::Store)
        .then(|| insn.inputs.first())
        .flatten();
    let mut sep = " ";
    for vn in insn.output.iter().chain(&insn.inputs) {
        f.write_str(sep)?;
        sep = ", ";
        if space_id.is_some_and(|id| std::ptr::eq(id, vn)) {
            match space_ids.resolve(*vn) {
                Some(s) => space(f, s)?,
                // Deterministic, where the id itself is a host address.
                None => f.write_str("<foreign space>")?,
            }
        } else {
            operand(f, *vn)?;
        }
    }
    Ok(())
}

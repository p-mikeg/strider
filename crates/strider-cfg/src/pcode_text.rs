use std::fmt;

use rsleigh::{Insn, Opcode, SpaceIds};
use strider_target::ArchNames;

/// `insn` in rsleigh's contextual spelling (`Store ram, RSP, RAX`), a LOAD /
/// STORE space id spelled as its space's name. `space_ids` and `names` must be
/// those of the engine and arch that decoded `insn`.
pub fn insn_text<'a>(
    insn: &'a Insn,
    space_ids: &'a SpaceIds,
    names: &'a ArchNames,
) -> impl fmt::Display + 'a {
    InsnText {
        insn,
        space_ids,
        names: Some(names),
    }
}

/// [`insn_text`] in rsleigh's plain `Display` spelling (`Store r, %[0x20]:8, %[0x0]:8`),
/// the space id rendered as the space's shortcut.
pub fn insn_plain_text<'a>(insn: &'a Insn, space_ids: &'a SpaceIds) -> impl fmt::Display + 'a {
    InsnText {
        insn,
        space_ids,
        names: None,
    }
}

struct InsnText<'a> {
    insn: &'a Insn,
    space_ids: &'a SpaceIds,
    /// `None` is rsleigh's plain spelling.
    names: Option<&'a ArchNames>,
}

impl fmt::Display for InsnText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let insn = self.insn;
        write!(f, "{}", insn.opcode)?;
        let space_id = matches!(insn.opcode, Opcode::Load | Opcode::Store)
            .then(|| insn.inputs.first())
            .flatten();
        let mut sep = " ";
        for vn in insn.output.iter().chain(&insn.inputs) {
            f.write_str(sep)?;
            sep = ", ";
            if !space_id.is_some_and(|id| std::ptr::eq(id, vn)) {
                match self.names {
                    Some(names) => write!(f, "{}", names.vn(*vn))?,
                    None => write!(f, "{vn}")?,
                }
                continue;
            }
            match (self.space_ids.resolve(*vn), self.names) {
                (Some(space), Some(names)) => write!(f, "{}", names.space(space))?,
                (Some(space), None) => write!(f, "{space}")?,
                // Deterministic, where the id itself is a host address.
                (None, _) => f.write_str("<foreign space>")?,
            }
        }
        Ok(())
    }
}

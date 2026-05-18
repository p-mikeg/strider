//! `StriderLang` — `egg::Language` impl for the acyclic value-slice egraph.
//!
//! Phase 1 Task 1.5 spike. Stub — populated in step 2 of the task.

use egg::Id;

/// Stub `Language` enum.
///
/// Populated in step 2 of the spike; the scaffold here is committed first so
/// the module structure compiles before the actual variants land.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StriderLang {
    /// Placeholder variant — replaced in step 2 by the real opaque-leaf /
    /// internal-op variants.
    Placeholder(Id),
}

impl egg::Language for StriderLang {
    type Discriminant = std::mem::Discriminant<Self>;

    fn discriminant(&self) -> Self::Discriminant {
        std::mem::discriminant(self)
    }

    fn matches(&self, other: &Self) -> bool {
        // Compare by discriminant + payload (NOT children — egg's contract).
        // Step 2 expands this to match on the real variants.
        match (self, other) {
            (Self::Placeholder(_), Self::Placeholder(_)) => true,
        }
    }

    fn children(&self) -> &[Id] {
        match self {
            Self::Placeholder(_) => &[],
        }
    }

    fn children_mut(&mut self) -> &mut [Id] {
        match self {
            Self::Placeholder(_) => &mut [],
        }
    }
}

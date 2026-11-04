use super::value_type::ValueType;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    Typed(ValueType),
    /// One edge per region predecessor, one per branch successor.
    Control,
    /// Dataless edge from a `Region` to its `Phi`s / `MemPhi`s.
    PhiToken,
    Memory,
}

// Fits the slot the 16-byte value record leaves.
const _: () = assert!(size_of::<ValueKind>() <= 4);

/// A typed value prints as its type (`f64`), the others by name.
impl std::fmt::Display for ValueKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Typed(ty) => write!(f, "{ty}"),
            Self::Control | Self::PhiToken | Self::Memory => write!(f, "{self:?}"),
        }
    }
}

impl ValueKind {
    #[inline]
    pub fn is_value(self) -> bool {
        matches!(self, Self::Typed(..))
    }

    #[inline]
    pub fn as_value(self) -> Option<ValueType> {
        match self {
            Self::Typed(v) => Some(v),
            Self::Control | Self::PhiToken | Self::Memory => None,
        }
    }

    #[inline]
    pub fn is_control(self) -> bool {
        self == Self::Control
    }

    #[inline]
    pub(crate) fn is_memory(self) -> bool {
        self == Self::Memory
    }

    #[inline]
    pub fn is_bool(self) -> bool {
        self.as_value().is_some_and(ValueType::is_bool)
    }

    #[inline]
    pub fn is_integer(self) -> bool {
        self.as_value().is_some_and(ValueType::is_integer)
    }
}

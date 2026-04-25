use crate::error::{ErrorKind, Result};

/// Returns a bitmask that covers all bits for a varnode's width in bytes.
///
/// This is used when reading or writing a sub-register inside a larger
/// container register.  The mask selects only the bits belonging to the
/// sub-register so they can be merged with the surrounding bits of the
/// container.
///
/// Supported sizes are 1, 2, 4, and 8 bytes.
pub fn vn_mask(reg: &rsleigh::Vn) -> Result<u64> {
    match reg.size {
        1 => Ok(u8::MAX as u64),
        2 => Ok(u16::MAX as u64),
        4 => Ok(u32::MAX as u64),
        8 => Ok(u64::MAX),
        _ => Err(ErrorKind::UnsupportedRegSize(reg.size).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(size: u32) -> rsleigh::Vn {
        rsleigh::Vn {
            size,
            addr: rsleigh::VnAddr {
                off: 0,
                space: rsleigh::VnSpace::REGISTER,
            },
        }
    }

    /// Masks must exactly cover each supported byte width with no extra bits.
    #[test]
    fn mask_covers_only_the_declared_width() -> Result<()> {
        assert_eq!(vn_mask(&reg(1))?, u8::MAX as u64);
        assert_eq!(vn_mask(&reg(2))?, u16::MAX as u64);
        assert_eq!(vn_mask(&reg(4))?, u32::MAX as u64);
        assert_eq!(vn_mask(&reg(8))?, u64::MAX);
        Ok(())
    }

    /// Wider mask must always be a superset of all narrower masks — a
    /// sub-register's bits always fit inside the container's mask.
    #[test]
    fn narrower_mask_is_subset_of_wider_mask() -> Result<()> {
        let m1 = vn_mask(&reg(1))?;
        let m2 = vn_mask(&reg(2))?;
        let m4 = vn_mask(&reg(4))?;
        let m8 = vn_mask(&reg(8))?;
        assert_eq!(m1 & m2, m1);
        assert_eq!(m2 & m4, m2);
        assert_eq!(m4 & m8, m4);
        Ok(())
    }

    /// Every unsupported size (0, 3, 5, 6, 7, 9, 16, 32, 64, MAX) must
    /// produce `UnsupportedRegSize`, never a panic or a silently-wrong mask.
    /// Pins that this function is the single source of truth for the
    /// 1/2/4/8 contract.
    #[test]
    fn unsupported_sizes_return_unsupported_reg_size_error() {
        for &bad in &[0u32, 3, 5, 6, 7, 9, 16, 32, 64, u32::MAX] {
            let r = vn_mask(&reg(bad));
            match r {
                Err(e) => match e.kind() {
                    ErrorKind::UnsupportedRegSize(s) => assert_eq!(*s, bad),
                    other => panic!("size {bad}: expected UnsupportedRegSize, got {other:?}"),
                },
                Ok(_) => panic!("size {bad}: expected error, got Ok"),
            }
        }
    }
}

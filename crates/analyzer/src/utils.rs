use crate::error::{ErrorKind, Result};

/// Returns a bitmask that covers all bits for a varnode's width in bytes.
///
/// This is used when reading or writing a sub-register inside a larger
/// container register.  The mask selects only the bits belonging to the
/// sub-register so they can be merged with the surrounding bits of the
/// container.
///
/// Supported sizes:
/// * 1, 2, 4, 8 bytes — standard integer-register widths.
/// * 10 bytes — x87 ST0/STn 80-bit FPU stack registers.  Models the
///   80-bit extended-precision width via `(1u128 << 80) - 1`.
/// * 16 bytes — wider sub-register writes through 16-byte SIMD container
///   registers (XMM0 on x86_64, q0 on aarch64).
pub fn vn_mask(reg: &rsleigh::Vn) -> Result<u128> {
    match reg.size {
        1 => Ok(u128::from(u8::MAX)),
        2 => Ok(u128::from(u16::MAX)),
        4 => Ok(u128::from(u32::MAX)),
        8 => Ok(u128::from(u64::MAX)),
        10 => Ok((1u128 << 80) - 1),
        16 => Ok(u128::MAX),
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
        assert_eq!(vn_mask(&reg(1))?, u128::from(u8::MAX));
        assert_eq!(vn_mask(&reg(2))?, u128::from(u16::MAX));
        assert_eq!(vn_mask(&reg(4))?, u128::from(u32::MAX));
        assert_eq!(vn_mask(&reg(8))?, u128::from(u64::MAX));
        assert_eq!(vn_mask(&reg(10))?, (1u128 << 80) - 1);
        assert_eq!(vn_mask(&reg(16))?, u128::MAX);
        Ok(())
    }

    /// Pinned: 10-byte mask is exactly the low 80 bits — used by
    /// `write_reg_vn` for x87 ST0/STn sub-register writes.  Off-by-one
    /// errors here would silently corrupt bits during register-merge.
    #[test]
    fn vn_mask_for_10_bytes_is_low_80_bits() -> Result<()> {
        let mask = vn_mask(&reg(10))?;
        // Low 80 bits all set.
        assert_eq!(mask & ((1u128 << 80) - 1), (1u128 << 80) - 1);
        // Bits 80..128 all clear.
        assert_eq!(mask >> 80, 0);
        // Direct equality.
        assert_eq!(mask, 0x_0000_FFFF_FFFF_FFFF_FFFF_FFFFu128);
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
        let m16 = vn_mask(&reg(16))?;
        assert_eq!(m1 & m2, m1);
        assert_eq!(m2 & m4, m2);
        assert_eq!(m4 & m8, m4);
        assert_eq!(m8 & m16, m8);
        Ok(())
    }

    /// Every unsupported size (0, 3, 5, 6, 7, 9, 32, 64, MAX) must
    /// produce `UnsupportedRegSize`, never a panic or a silently-wrong mask.
    /// Pins that this function is the single source of truth for the
    /// 1/2/4/8/16 contract.  Note: 16 is now supported and excluded from
    /// the bad-sizes list.
    #[test]
    fn unsupported_sizes_return_unsupported_reg_size_error() {
        for &bad in &[0u32, 3, 5, 6, 7, 9, 32, 64, u32::MAX] {
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

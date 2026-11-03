#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Endianness {
    /// Least-significant byte at the lowest address (x86, AArch64 LE).
    #[default]
    Little,
    /// Most-significant byte at the lowest address (MIPS BE, AArch64 BE).
    Big,
}

impl Endianness {
    /// Decodes an N-byte word into a `u128`: `[0x01,0x02,0x03,0x04]` gives
    /// `0x0403_0201` little-endian, `0x0102_0304` big-endian.
    ///
    /// # Panics
    ///
    /// Panics if `bytes.len() > 16`.
    pub fn read_uint(self, bytes: &[u8]) -> u128 {
        let n = bytes.len();
        assert!(n <= 16, "read_uint supports at most 16 bytes, got {n}");
        let mut buf = [0u8; 16];
        match self {
            Self::Little => {
                buf[..n].copy_from_slice(bytes);
                u128::from_le_bytes(buf)
            }
            // BE bytes go in the high slots so the widened word still reads
            // as an N-byte big-endian value.
            Self::Big => {
                buf[16 - n..].copy_from_slice(bytes);
                u128::from_be_bytes(buf)
            }
        }
    }

    /// Bit distance between `reg`'s slot and `container`'s LSB in this order,
    /// or `None` when `reg` does not sit inside `container`.
    ///
    /// Little-endian: the LSB byte is at offset 0, so the byte distance from
    /// the container's start is the shift. Big-endian: the MSB byte is at
    /// offset 0, so the slice sits `container.size - reg.size - (reg.off -
    /// container.off)` bytes above the LSB.
    #[must_use]
    pub fn slice_shift_bits(self, reg: &rsleigh::Vn, container: &rsleigh::Vn) -> Option<u64> {
        if !vn_container::vn_contains(container, reg) {
            return None;
        }
        let within = reg.addr_off - container.addr_off;
        let bytes = match self {
            Self::Little => within,
            Self::Big => u64::from(container.size) - u64::from(reg.size) - within,
        };
        bytes.checked_mul(8)
    }
}

/// The byte order of each address space: the data order, and the Sleigh
/// REGISTER space's, which differ only on ARM BE8
/// ([`SleighArch::register_endianness`](super::SleighArch::register_endianness)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ByteOrder {
    pub data: Endianness,
    pub register: Endianness,
}

impl ByteOrder {
    /// The order of a load or store in `space`.
    #[must_use]
    pub fn of(self, space: rsleigh::VnSpace) -> Endianness {
        if space == rsleigh::VnSpace::REGISTER {
            self.register
        } else {
            self.data
        }
    }
}

/// One order for every space.
impl From<Endianness> for ByteOrder {
    fn from(order: Endianness) -> Self {
        Self {
            data: order,
            register: order,
        }
    }
}

#[cfg(test)]
mod endianness_tests {
    use super::Endianness;
    use super::Endianness::{Big, Little};
    use rsleigh::{Vn, VnSpace};

    fn reg(off: u64, size: u32) -> Vn {
        Vn {
            addr_off: off,
            addr_space: VnSpace::REGISTER,
            size,
        }
    }

    #[test]
    fn a_reg_outside_its_container_is_none_not_a_panic() {
        // Starting below the container: both arms subtract below zero on the
        // offset.
        let high = reg(4, 4);
        assert_eq!(Little.slice_shift_bits(&reg(0, 1), &high), None);
        assert_eq!(Big.slice_shift_bits(&reg(0, 1), &high), None);
        // Starting inside but overhanging the end.
        let container = reg(0, 4);
        assert_eq!(Big.slice_shift_bits(&reg(0, 8), &container), None);
        assert_eq!(Big.slice_shift_bits(&reg(2, 4), &container), None);
        assert_eq!(Little.slice_shift_bits(&reg(0, 8), &container), None);
        assert_eq!(Little.slice_shift_bits(&reg(2, 4), &container), None);
        // Same offsets in another space are not the same bytes.
        let unique = Vn {
            addr_off: 0,
            addr_space: VnSpace::UNIQUE,
            size: 1,
        };
        assert_eq!(Little.slice_shift_bits(&unique, &container), None);
        assert_eq!(Big.slice_shift_bits(&unique, &container), None);
    }

    #[test]
    fn a_contained_reg_shifts_by_its_distance_from_the_lsb() {
        let container = reg(0, 4);
        assert_eq!(Little.slice_shift_bits(&reg(3, 1), &container), Some(24));
        assert_eq!(Big.slice_shift_bits(&reg(3, 1), &container), Some(0));
        assert_eq!(Big.slice_shift_bits(&reg(0, 1), &container), Some(24));
    }

    #[test]
    fn read_uint_decodes_n_byte_word_per_endianness() {
        let bytes = [0x01, 0x02, 0x03, 0x04];
        assert_eq!(Endianness::Little.read_uint(&bytes), 0x0403_0201);
        assert_eq!(Endianness::Big.read_uint(&bytes), 0x0102_0304);
    }

    #[test]
    fn read_uint_single_byte_is_endianness_invariant() {
        assert_eq!(Endianness::Little.read_uint(&[0xab]), 0xab);
        assert_eq!(Endianness::Big.read_uint(&[0xab]), 0xab);
    }

    #[test]
    fn read_uint_full_16_bytes_decodes_full_u128() {
        let bytes: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        assert_eq!(
            Endianness::Little.read_uint(&bytes),
            u128::from_le_bytes(bytes),
        );
        assert_eq!(
            Endianness::Big.read_uint(&bytes),
            u128::from_be_bytes(bytes)
        );
        // The high byte participates; a u64-width decode would lose it.
        assert_eq!(
            Endianness::Big.read_uint(&bytes) >> 120,
            0x00,
            "BE high byte is first source byte",
        );
        assert_eq!(
            Endianness::Little.read_uint(&bytes) >> 120,
            0xff,
            "LE high byte is last source byte",
        );
    }

    #[test]
    fn read_uint_full_u64_matches_native_u64_decode() {
        let bytes = [0x78, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x89];
        assert_eq!(
            Endianness::Little.read_uint(&bytes),
            u128::from(u64::from_le_bytes(bytes)),
        );
        assert_eq!(
            Endianness::Big.read_uint(&bytes),
            u128::from(u64::from_be_bytes(bytes)),
        );
    }
}

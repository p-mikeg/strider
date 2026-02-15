pub fn vn_mask(reg: &rsleigh::Vn) -> u64 {
    match reg.size {
        1 => u8::MAX as u64,
        2 => u16::MAX as u64,
        4 => u32::MAX as u64,
        8 => u64::MAX,
        _ => unreachable!()
    }
}
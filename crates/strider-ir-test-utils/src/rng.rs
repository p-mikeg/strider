//! A seeded generator for hand-rolled random tests.

/// xorshift64 over an explicit state, so a failing fixture replays from its
/// seed alone. State `0` is a fixed point: seeds are forced odd.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn u128(&mut self) -> u128 {
        (u128::from(self.next_u64()) << 64) | u128::from(self.next_u64())
    }

    pub fn chance(&mut self, pct: u64) -> bool {
        self.next_u64() % 100 < pct
    }
}

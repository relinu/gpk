//! Deterministischer Zufallsgenerator für Tests (xorshift64*).
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Die in `legacy-c/gpk/tests/sample_jpegs.h` eingebetteten Beispiel-JPEGs.
pub fn jpeg_samples() -> Vec<Vec<u8>> {
    let h = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/legacy-c/gpk/tests/sample_jpegs.h")).unwrap();
    h.lines()
        .filter(|l| l.starts_with("static const unsigned char SAMPLE_JPEG"))
        .map(|l| {
            let body = &l[l.find('{').unwrap() + 1..l.rfind('}').unwrap()];
            body.split(',').map(|v| v.trim().parse::<u8>().unwrap()).collect()
        })
        .collect()
}

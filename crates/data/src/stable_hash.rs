//! Platform-independent FNV-1a. Explicit little-endian encoding, never Rust's Hash.
use tr_core::Bar;
#[derive(Clone)]
pub struct Fnv64(u64);
impl Default for Fnv64 {
    fn default() -> Self {
        Self(0xcbf29ce484222325)
    }
}
impl Fnv64 {
    pub fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x100000001b3);
        }
    }
    pub fn write_u64(&mut self, n: u64) {
        self.write(&n.to_le_bytes());
    }
    pub fn write_i64(&mut self, n: i64) {
        self.write(&n.to_le_bytes());
    }
    pub fn write_f64_bits(&mut self, n: f64) {
        self.write_u64(n.to_bits());
    }
    pub fn write_str(&mut self, s: &str) {
        self.write_u64(s.len() as u64);
        self.write(s.as_bytes());
    }
    pub fn finish(&self) -> u64 {
        self.0
    }
}
/// Call with the closed prefix. Real and cached are the same verified class.
pub fn bars_digest(bars: &[Bar]) -> u64 {
    let mut h = Fnv64::default();
    h.write_u64(bars.len() as u64);
    for b in bars {
        h.write_i64(b.ts);
        for v in [b.open, b.high, b.low, b.close, b.volume] {
            h.write_f64_bits(v);
        }
        h.write_u64(u64::from(b.provenance.is_verified()));
    }
    h.finish()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fnv_digest_matches_pinned_vectors() {
        let mut h = Fnv64::default();
        assert_eq!(h.finish(), 0xcbf29ce484222325);
        h.write(b"hello");
        assert_eq!(h.finish(), 0xa430d84680aabd0b);
    }
    #[test]
    fn closed_fields_and_provenance_invalidate() {
        let b = Bar::new(1, 10., 11., 9., 10., 5.).with_provenance(tr_core::Provenance::Real);
        let digest = bars_digest(&[b]);
        for field in 0..6 {
            let mut c = b;
            match field {
                0 => c.ts += 1,
                1 => c.open += 1.,
                2 => c.high += 1.,
                3 => c.low += 1.,
                4 => c.close += 1.,
                _ => c.volume += 1.,
            };
            assert_ne!(digest, bars_digest(&[c]));
        }
        assert_eq!(digest, bars_digest(&[b.with_provenance(tr_core::Provenance::Cached)]));
        assert_ne!(digest, bars_digest(&[b.with_provenance(tr_core::Provenance::Sample)]));
    }
}

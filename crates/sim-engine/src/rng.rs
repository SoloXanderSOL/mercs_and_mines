// Mulberry32 seeded PRNG — the single source of randomness for the simulation engine.
// Output sequence is bit-for-bit identical to src/engine/rng.ts (TypeScript implementation).
//
// FINANCIAL REQUIREMENT: Do not alter this algorithm without bumping build_version and
// marking all older session replays as unsupported. The input log is the Prize Pool audit
// trail; any drift breaks dispute resolution. See Technical_Architecture_Deterministic_Simulation.md.

/// The simulation PRNG. Pass by `&mut` to every system that needs randomness.
/// Do NOT derive Clone or Copy — accidental state duplication silently breaks determinism.
pub struct Rng {
    state: u32,
}

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    /// Mulberry32 core. Advances state and returns a u32 in [0, 2^32).
    pub fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_add(0x6d2b79f5);
        let s = self.state;
        let mut t = (s ^ (s >> 15)).wrapping_mul(1 | s);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t));
        t ^ (t >> 14)
    }

    /// Returns a float in [0.0, 1.0).
    pub fn next_f64(&mut self) -> f64 {
        self.next_u32() as f64 / 4294967296.0
    }

    /// Returns an integer in [1, 100] inclusive.
    pub fn roll_d100(&mut self) -> u32 {
        self.roll_int(1, 100)
    }

    /// Returns an integer in [min, max] inclusive.
    pub fn roll_int(&mut self, min: u32, max: u32) -> u32 {
        (self.next_f64() * (max - min + 1) as f64) as u32 + min
    }

    /// Returns true if the next float draw is less than `probability` (range 0.0–1.0).
    pub fn chance(&mut self, probability: f64) -> bool {
        self.next_f64() < probability
    }
}

/// Generates a unique report ID by consuming two draws from the PRNG.
/// Produces a 16-char lowercase hex string. Advances RNG state by exactly 2 steps.
pub fn generate_report_id(rng: &mut Rng) -> String {
    format!("{:08x}{:08x}", rng.next_u32(), rng.next_u32())
}

/// Generate a session seed from OS entropy. Call once at session start; hand the seed to
/// `Rng::new`. This function must never touch the Mulberry32 algorithm.
pub fn generate_seed() -> u32 {
    let mut bytes = [0u8; 4];
    getrandom::getrandom(&mut bytes).expect("OS RNG unavailable");
    u32::from_ne_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden-value parity test: asserts bit-for-bit match with rng.ts output for seed 12345.
    /// Captured by running rng.ts with seed 12345 and recording each method's output in order.
    /// If this test fails the Rust and TypeScript PRNG sequences have diverged and replay
    /// compatibility — including Prize Pool dispute resolution — is broken.
    #[test]
    fn mulberry32_golden_values() {
        let mut rng = Rng::new(12345);
        assert_eq!(rng.next_u32(),      4207900869);
        assert_eq!(rng.next_u32(),      1317490944);
        assert_eq!(rng.next_f64(),      f64::from_bits(0x3fdefd38bc800000)); // 0.484205421525985
        assert_eq!(rng.roll_d100(),     82);
        assert_eq!(rng.roll_int(1, 6),  4);
        assert_eq!(rng.chance(0.5),     true);
    }
}

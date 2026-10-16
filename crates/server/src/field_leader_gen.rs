//! Generation helpers shared by both Field Leader classes (brick 7a-1): the Grade-band
//! weights, the uniform and weighted picks, the Faction Bias roll and the name tables.
//! Pure: no I/O. Every draw comes from the caller's seeded `Rng`.

use sim_engine::rng::Rng;
use sim_engine::roster::{CommanderGrade, CommanderOrigin, FactionBias};

/// Spawn-Grade weights in `CommanderGrade::ALL` order (Standard Issue … Heroic): the
/// Commander_Grade_System §2 frequency targets, ~50/30/15/4/1, for both Field Leader
/// classes. §3e's flat distribution cannot hit them, so the generators draw the band first.
/// Pool Degradation (Commander_Anti_Exploit_Systems Mechanic 4) will pass its own vector,
/// and validates it where it builds it (Reviewer 4d-2 CF5).
pub const DEFAULT_GRADE_WEIGHTS: [u32; 5] = [50, 30, 15, 4, 1];

const HUMAN_FIRST_NAMES: &[&str] = &["Vance", "Kira", "Dolan", "Metz", "Shen", "Yula"];
const SHARED_SURNAMES: &[&str] = &["Ashmore", "Drek", "Volkov", "Jin", "Osei", "Colt"];

/// One uniform pick from `pool`: exactly one draw, including for a one-element pool.
pub(crate) fn pick<T: Copy>(pool: &[T], rng: &mut Rng) -> T {
    assert!(!pool.is_empty(), "pick from an empty pool");
    pool[rng.roll_int(0, (pool.len() - 1) as u32) as usize]
}

/// Name pools are provisional and playtest-flagged (Director-approved 2026-07-19). Science
/// Ape names fill the surname slot, after the Pemberton-Simian precedent. Two draws.
pub(crate) fn pick_name(origin: CommanderOrigin, rng: &mut Rng) -> String {
    let (first_names, surnames): (&[&str], &[&str]) = match origin {
        CommanderOrigin::HumanCorporate | CommanderOrigin::HumanUnderhive => (HUMAN_FIRST_NAMES, SHARED_SURNAMES),
        CommanderOrigin::Duck => (
            &["Mallardus", "Drakewell", "Pondsworth", "Aylesbury", "Canardo", "Widgeon", "Brantley", "Scaup"],
            SHARED_SURNAMES,
        ),
        CommanderOrigin::Bearocrat => (
            &["Ursinus", "Kodiakov", "Bruinsworth", "Grizelda", "Paddingsley", "Ursaline", "Bearnard", "Clawford"],
            SHARED_SURNAMES,
        ),
        CommanderOrigin::ScienceApe => (
            HUMAN_FIRST_NAMES,
            &["Darwin-Macaque", "Newton-Gibbon", "Tesla-Tamarin", "Hawking-Howler", "Curie-Capuchin", "Faraday-Baboon", "Kepler-Colobus", "Lovelace-Langur"],
        ),
    };
    let first = pick(first_names, rng);
    let last = pick(surnames, rng);
    format!("{} {}", first, last)
}

/// The hidden Faction Bias: the parent from `FactionBias::GROUPS`, then the sect within it.
/// Two draws for every parent, so the count is fixed (a one-value group still draws).
pub(crate) fn pick_faction_bias(rng: &mut Rng) -> FactionBias {
    let group = pick(&FactionBias::GROUPS, rng);
    pick(group, rng)
}

/// One weighted pick of a Grade band: exactly one draw. `band_sizes` are the class's
/// combination counts per band, in `CommanderGrade::ALL` order. A zero-weight band is never
/// chosen.
///
/// Refused before any draw, so a refused vector consumes no RNG (Reviewer 4d-2 F1/F2): all
/// weights zero; a total past `u32::MAX` (`Rng::roll_int` is `u32`); a non-zero weight on
/// an empty band. A zero weight on an empty band is legal.
pub(crate) fn pick_band(weights: &[u32; 5], band_sizes: [usize; 5], rng: &mut Rng) -> CommanderGrade {
    let total = weights
        .iter()
        .try_fold(0u32, |acc, &w| acc.checked_add(w))
        .expect("grade weights overflow u32");
    assert!(total > 0, "grade weights must not all be zero");
    assert!(
        weights.iter().zip(band_sizes).all(|(&w, size)| w == 0 || size > 0),
        "grade weight on an empty band"
    );
    let roll = rng.roll_int(0, total - 1);
    let mut cumulative = 0;
    for (grade, &w) in CommanderGrade::ALL.iter().zip(weights) {
        cumulative += w;
        if roll < cumulative {
            return *grade;
        }
    }
    unreachable!("roll {roll} below total {total} always lands in a band")
}

/// Asserts that over 10 000 seeds each band's count lies within 4σ of its binomial
/// expectation under the canon targets (Commander_Grade_System §2). The targets are a
/// literal here, independent of `DEFAULT_GRADE_WEIGHTS`, so a changed default fails it.
#[cfg(test)]
pub(crate) fn assert_band_frequencies(class: &str, grade_for_seed: impl Fn(u32) -> CommanderGrade) {
    const CANON_TARGETS: [u32; 5] = [50, 30, 15, 4, 1];
    assert_eq!(CommanderGrade::ALL[0], CommanderGrade::StandardIssue);
    assert_eq!(CommanderGrade::ALL[4], CommanderGrade::Heroic);
    const N: u32 = 10_000;
    let mut counts = [0u32; 5];
    for seed in 0..N {
        let grade = grade_for_seed(seed);
        counts[CommanderGrade::ALL.iter().position(|&g| g == grade).unwrap()] += 1;
    }
    println!("{class} Grade counts over {N} seeds (Standard Issue … Heroic): {counts:?}");
    let total: u32 = CANON_TARGETS.iter().sum();
    for (i, &observed) in counts.iter().enumerate() {
        let p = CANON_TARGETS[i] as f64 / total as f64;
        let expected = N as f64 * p;
        let tolerance = 4.0 * (N as f64 * p * (1.0 - p)).sqrt();
        assert!(
            (observed as f64 - expected).abs() <= tolerance,
            "{class} {:?}: observed {observed}, expected {expected} ± {tolerance:.0}",
            CommanderGrade::ALL[i]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: [usize; 5] = [1, 1, 1, 1, 1];

    #[test]
    #[should_panic(expected = "grade weights must not all be zero")]
    fn pick_band_refuses_all_zero_weights() {
        pick_band(&[0, 0, 0, 0, 0], SIZES, &mut Rng::new(1));
    }

    #[test]
    #[should_panic(expected = "grade weights overflow u32")]
    fn pick_band_refuses_overflowing_weights() {
        pick_band(&[u32::MAX, 1, 0, 0, 0], SIZES, &mut Rng::new(1));
    }

    #[test]
    #[should_panic(expected = "grade weight on an empty band")]
    fn pick_band_refuses_a_weight_on_an_empty_band() {
        pick_band(&[1, 0, 0, 0, 1], [5, 0, 0, 0, 0], &mut Rng::new(1));
    }

    /// An empty band with weight 0 is legal, and the pick lands only on non-zero bands.
    #[test]
    fn pick_band_allows_zero_weight_on_an_empty_band() {
        for seed in 0..200u32 {
            let grade = pick_band(&[1, 0, 0, 0, 0], [5, 0, 0, 0, 0], &mut Rng::new(seed));
            assert_eq!(grade, CommanderGrade::StandardIssue, "seed {seed}");
        }
    }

    #[test]
    #[should_panic(expected = "pick from an empty pool")]
    fn pick_refuses_an_empty_pool() {
        let empty: [u8; 0] = [];
        pick(&empty, &mut Rng::new(1));
    }
}

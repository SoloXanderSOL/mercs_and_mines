// Hex geometry utilities — no game-logic knowledge.
// Ref: Hex_Map_and_Travel.md §6 (Unit Collision Rule)

use crate::Coordinates;

/// Returns the six axial neighbours of a hex coordinate.
/// Order per Hex_Map_and_Travel.md §6:
///   (q+1,r), (q-1,r), (q,r+1), (q,r-1), (q+1,r-1), (q-1,r+1)
pub fn get_neighbors(hex: &Coordinates) -> Vec<Coordinates> {
    let (q, r) = (hex.q, hex.r);
    vec![
        Coordinates { q: q + 1, r },
        Coordinates { q: q - 1, r },
        Coordinates { q,        r: r + 1 },
        Coordinates { q,        r: r - 1 },
        Coordinates { q: q + 1, r: r - 1 },
        Coordinates { q: q - 1, r: r + 1 },
    ]
}

/// Returns the neighbour of `from` that minimises hex distance to `to`.
/// Ties broken by neighbour order (deterministic). Returns a copy of `from` if already at `to`.
pub fn step_toward(from: &Coordinates, to: &Coordinates) -> Coordinates {
    if from.q == to.q && from.r == to.r {
        return Coordinates { q: from.q, r: from.r };
    }
    let neighbors = get_neighbors(from);
    let mut best = neighbors[0].clone();
    let mut best_dist = hex_distance(&neighbors[0], to);
    for n in &neighbors[1..] {
        let d = hex_distance(n, to);
        if d < best_dist {
            best_dist = d;
            best = n.clone();
        }
    }
    best
}

// Private helper — same formula as resolver::calculate_hex_distance.
// Kept local so shared never imports sim-engine.
fn hex_distance(a: &Coordinates, b: &Coordinates) -> u32 {
    let dq = (a.q - b.q).abs() as i64;
    let ds = ((a.q + a.r) - (b.q + b.r)).abs() as i64;
    let dr = (a.r - b.r).abs() as i64;
    ((dq + ds + dr) / 2) as u32
}

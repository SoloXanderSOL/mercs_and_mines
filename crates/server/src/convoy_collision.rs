use crate::repository::{ConvoyDbRecord, HexOccupant};

#[derive(Debug)]
pub struct CollisionPair {
    pub a: HexOccupant,
    pub b: HexOccupant,
}

/// Returns the first pair of occupants in the same hex from different owners.
pub fn check_same_hex_collision(occupants: &[HexOccupant]) -> Option<CollisionPair> {
    for i in 0..occupants.len() {
        for j in (i + 1)..occupants.len() {
            if occupants[i].owner_wallet != occupants[j].owner_wallet {
                return Some(CollisionPair {
                    a: occupants[i].clone(),
                    b: occupants[j].clone(),
                });
            }
        }
    }
    None
}

/// Returns true if convoy a and convoy b are crossing:
/// a travels A→B while b travels B→A simultaneously.
pub fn check_crossing_collision(a: &ConvoyDbRecord, b: &ConvoyDbRecord) -> bool {
    a.origin_q == b.destination_q
        && a.origin_r == b.destination_r
        && b.origin_q == a.destination_q
        && b.origin_r == a.destination_r
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn make_occupant(owner: Vec<u8>) -> HexOccupant {
        HexOccupant { id: Uuid::new_v4(), owner_wallet: owner, unit_type: "truck".to_string() }
    }

    fn make_convoy(oq: i32, or_: i32, dq: i32, dr: i32) -> ConvoyDbRecord {
        ConvoyDbRecord {
            convoy_id:     Uuid::new_v4(),
            sector_id:     Uuid::new_v4(),
            owner_wallet:  vec![1u8; 32],
            origin_q:      oq,
            origin_r:      or_,
            destination_q: dq,
            destination_r: dr,
            route:         serde_json::Value::Null,
            arrival_time:  Utc::now(),
            vehicle_class: "truck".to_string(),
            cargo:         serde_json::Value::Null,
            in_transit:    true,
            created_at:    Utc::now(),
        }
    }

    #[test]
    fn same_hex_same_owner_no_collision() {
        let wallet = vec![1u8; 32];
        let occupants = vec![make_occupant(wallet.clone()), make_occupant(wallet)];
        assert!(check_same_hex_collision(&occupants).is_none());
    }

    #[test]
    fn same_hex_different_owner_collision() {
        let a = make_occupant(vec![1u8; 32]);
        let b = make_occupant(vec![2u8; 32]);
        let occupants = vec![a.clone(), b.clone()];
        let pair = check_same_hex_collision(&occupants).expect("expected collision");
        assert_eq!(pair.a.owner_wallet, vec![1u8; 32]);
        assert_eq!(pair.b.owner_wallet, vec![2u8; 32]);
    }

    #[test]
    fn crossing_collision_true() {
        let a = make_convoy(0, 0, 1, 0);
        let b = make_convoy(1, 0, 0, 0);
        assert!(check_crossing_collision(&a, &b));
    }

    #[test]
    fn crossing_collision_false_parallel() {
        let a = make_convoy(0, 0, 1, 0);
        let b = make_convoy(0, 0, 1, 0);
        assert!(!check_crossing_collision(&a, &b));
    }
}

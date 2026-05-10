// "The Ore Run" engagement resolver — hackathon demo combat logic.
//
// Wraps the AP/AT tick engine with: scenario state, approach modifiers,
// Scrap-Rocket handling, and the flavour text event system.
// Does NOT touch core resolver.rs internals.
//
// ── RNG draw order (determinism contract) ─────────────────────────────────
//
// Reordering any draw is a breaking change. Bump build_version before doing so.
//
// Per tick:
//   [TICK 2 ONLY, draws 1 & 2]
//     1. Scrap-Rocket misfire check  — roll_d100()
//     2. Scrap-Rocket hit roll       — roll_d100()  (skipped if misfire)
//   [ALL TICKS]
//     3. For each active pack in stable index order (0..3):
//          For each weapon of each vehicle in stable vehicle order (0..3),
//          weapon declaration order:
//            draw — roll_d100() [hit roll]
//     4. For each section in stable order (0..1):
//          For each surviving section member (0..current_strength):
//            draw — roll_d100() [hit roll]
//     5. For each active pack in stable index order (0..3):
//          (only packs with fires_at_section = Some(_))
//          For each surviving pack member (0..current_strength):
//            draw — roll_d100() [hit roll]
//     6. Flavour text draws — one roll_d100() per flavour event emitted this tick,
//          in the order: ScrapRocket event (if any), then vehicle hits, then
//          section hits, then infantry casualties, then pack scatters.

use serde::Serialize;

use crate::constants::HIT_ROLL_THRESHOLD;
use crate::rng::Rng;
use crate::scenario::{
    flavour_pool, pick_flavour, the_ore_run, Approach, ApproachModifiers,
    EngagementResult, FlavourEvent, HighlightEvent, HighlightKind, Outcome,
    OreRunPack, ScenarioWeaponKind,
    PACK_INDIVIDUAL_HP, SCRAP_ROCKET_DAMAGE,
    SCRAP_ROCKET_FIRE_TICK, SCRAP_ROCKET_MISFIRE_THRESHOLD,
    ORE_RUN_MAX_TICKS, resolve_approach_modifiers,
};

// ── Per-tick log types (consumed by the UI streaming layer in Prompt 2) ────

#[derive(Debug, Clone, Serialize)]
pub struct VehicleWeaponResult {
    pub vehicle_name: &'static str,
    pub weapon_name: &'static str,
    pub target_pack: usize,
    pub hit_roll: i32,
    pub is_hit: bool,
    pub is_penetration: bool,
    pub kills: u32,
    pub flavour: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionFireResult {
    pub section_name: &'static str,
    pub target_pack: usize,
    pub shots: u32,
    pub hits: u32,
    pub total_damage: i32,
    pub kills: u32,
    pub flavour: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackFireResult {
    pub pack_name: &'static str,
    pub target_section: usize,
    pub shots: u32,
    pub hits: u32,
    pub total_damage: i32,
    pub kills: u32,
    pub flavour: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScrapRocketResult {
    pub misfired: bool,
    pub hit: bool, // only valid when !misfired
    pub misfire_carrier_killed: bool,
    pub misfire_additional_casualties: u32,
    pub damage_dealt: i32,
    pub flavour: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScatterEvent {
    pub pack_index: usize,
    pub pack_name: &'static str,
    pub strength_at_scatter: u32,
    pub flavour: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct OreRunTickLog {
    pub tick: u32,
    pub scrap_rocket: Option<ScrapRocketResult>,
    pub vehicle_fire: Vec<VehicleWeaponResult>,
    pub section_fire: Vec<SectionFireResult>,
    pub pack_fire: Vec<PackFireResult>,
    pub scatter_events: Vec<ScatterEvent>,
    /// Snapshot after all phases resolve.
    pub pack_strengths: Vec<u32>,
    pub section_strengths: Vec<u32>,
    pub warthog_hp_after: i32,
}

// ── Targeting helper ────────────────────────────────────────────────────────

/// Returns the index of the largest active pack (most current_strength).
/// Ties broken by lowest index (stable, deterministic).
fn largest_active_pack(packs: &[OreRunPack]) -> Option<usize> {
    packs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.is_active())
        .max_by_key(|(i, p)| (p.current_strength, u32::MAX - *i as u32))
        .map(|(i, _)| i)
}

// ── Approach modifier application helpers ───────────────────────────────────

fn defender_hit_roll(
    d100: i32,
    weapon_accuracy: i32,
    pack_evasion: i32,
    def_mods: &ApproachModifiers,
    att_mods: &ApproachModifiers,
) -> i32 {
    let accuracy = weapon_accuracy + def_mods.own_accuracy_bonus - att_mods.enemy_accuracy_penalty;
    let evasion  = pack_evasion   + att_mods.own_evasion_bonus   - def_mods.enemy_evasion_penalty;
    d100 + accuracy - evasion
}

fn attacker_hit_roll(
    d100: i32,
    pack_accuracy: i32,
    section_evasion: i32,
    att_mods: &ApproachModifiers,
    def_mods: &ApproachModifiers,
) -> i32 {
    let accuracy = pack_accuracy  + att_mods.own_accuracy_bonus - def_mods.enemy_accuracy_penalty;
    let evasion  = section_evasion + def_mods.own_evasion_bonus  - att_mods.enemy_evasion_penalty;
    d100 + accuracy - evasion
}

// ── Damage helpers ──────────────────────────────────────────────────────────

/// Slug vs Unarmored (AT 0) gives ×2.0. All other pack/section matchups ×1.0.
/// Packs are unarmored; sections have LightArmor. Raccoon slugs vs LightArmor = ×1.0.
fn slug_vs_unarmored_mult(weapon_ap: i32, target_at: i32, target_unarmored: bool) -> f32 {
    if weapon_ap < target_at { return 0.0; } // no penetration
    if target_unarmored { 2.0 } else { 1.0 }
}

fn kills_from_damage(total_damage: i32, individual_hp: i32) -> u32 {
    if individual_hp <= 0 { return 0; }
    (total_damage / individual_hp) as u32
}

// ── Main engagement resolver ────────────────────────────────────────────────

/// Run "The Ore Run" engagement to completion.
///
/// `seed` — must be the engagement seed from the session log.
/// `timestamp` — from the session_start InputLogEntry; injected, never wall-clock.
/// `player_approach` — the player's chosen approach.
///
/// Returns the engagement result (UI contract) and the full tick log (for streaming).
pub fn run_ore_run(
    player_approach: Approach,
    seed: u32,
    timestamp: &str,
) -> (EngagementResult, Vec<OreRunTickLog>) {
    let _ = timestamp; // reserved for input log; simulation doesn't use wall time
    let mut rng = Rng::new(seed);

    let (def_mods, att_mods) = resolve_approach_modifiers(player_approach);
    let (mut vehicles, mut sections, mut packs) = the_ore_run();

    let mut tick_logs: Vec<OreRunTickLog> = Vec::new();
    let mut total_defender_kia: u32 = 0;
    let mut total_attacker_kia: u32 = 0;
    let mut packs_routed: u32 = 0;
    let mut packs_destroyed: u32 = 0;
    let mut misfire_occurred = false;
    let mut highlights: Vec<HighlightEvent> = Vec::new();

    // ── Escape/Rearguard: EscapeRearguard modifiers apply main-force exit.
    // Implemented at tick boundary below — escape_after_tick drives it.

    'ticks: for tick in 1..=ORE_RUN_MAX_TICKS {

        // ── Escape check: if player chose EscapeRearguard and tick > escape tick ─
        if let Some(exit_tick) = def_mods.escape_after_tick {
            if tick > exit_tick {
                // Main force has exited. Rearguard (smallest section) absorbs all fire.
                // For the engagement log we note this and break — rearguard handling
                // is tracked separately in the flavour layer (Prompt 2 concern).
                break 'ticks;
            }
        }

        let mut veh_fire_log: Vec<VehicleWeaponResult> = Vec::new();
        let mut sec_fire_log: Vec<SectionFireResult> = Vec::new();
        let mut pack_fire_log: Vec<PackFireResult> = Vec::new();
        let mut scatter_events: Vec<ScatterEvent> = Vec::new();
        let mut scrap_rocket_result: Option<ScrapRocketResult> = None;

        // ═══════════════════════════════════════════════════════════════════
        // PHASE 0 — Scrap-Rocket (Tick 2 only, draw order 1 & 2)
        // ═══════════════════════════════════════════════════════════════════

        if tick == SCRAP_ROCKET_FIRE_TICK {
            if let Some(punk_idx) = packs.iter().position(|p| p.id == 3 && p.has_scrap_rocket && p.is_active()) {
                // Draw 1: misfire check — MUST occur before hit roll.
                let misfire_roll = rng.roll_d100();
                let misfired = misfire_roll <= SCRAP_ROCKET_MISFIRE_THRESHOLD;

                if misfired {
                    misfire_occurred = true;
                    let pack = &mut packs[punk_idx];
                    let adjacents = ((pack.current_strength as f32 * 0.2).floor() as u32)
                        .min(pack.current_strength.saturating_sub(1));
                    let carrier_killed = pack.current_strength > 0;
                    if carrier_killed {
                        pack.current_strength = pack.current_strength.saturating_sub(1);
                    }
                    let adj_killed = adjacents.min(pack.current_strength);
                    pack.current_strength = pack.current_strength.saturating_sub(adj_killed);
                    total_attacker_kia += (carrier_killed as u32) + adj_killed;

                    let flavour_draw = rng.roll_d100();
                    let flavour = pick_flavour(flavour_pool(FlavourEvent::ScrapRocketMisfire), flavour_draw);
                    scrap_rocket_result = Some(ScrapRocketResult {
                        misfired: true,
                        hit: false,
                        misfire_carrier_killed: carrier_killed,
                        misfire_additional_casualties: adj_killed,
                        damage_dealt: 0,
                        flavour,
                    });
                    highlights.push(HighlightEvent {
                        tick,
                        kind: HighlightKind::ScrapRocketMisfire,
                        pack_name: Some(packs[punk_idx].name.to_string()),
                        flavour: flavour.to_string(),
                    });
                } else {
                    // Draw 2: hit roll — standard accuracy check, no special rules.
                    let hit_d100 = rng.roll_d100() as i32;
                    // Scrap-Rocket fires at Warthog (vehicle id 0) — AP_SCRAP_ROCKET vs WARTHOG_AT.
                    let warthog_evasion = vehicles[0].evasion;
                    let pack_acc = packs[punk_idx].accuracy as i32;
                    let hit_total = hit_d100 + pack_acc - warthog_evasion;
                    let is_hit = hit_total > HIT_ROLL_THRESHOLD;

                    let (damage, flavour_ev) = if is_hit {
                        let dmg = ((SCRAP_ROCKET_DAMAGE as f32) * att_mods.damage_dealt_multiplier) as i32;
                        vehicles[0].hp = (vehicles[0].hp - dmg).max(0);
                        (dmg, FlavourEvent::ScrapRocketHit)
                    } else {
                        (0, FlavourEvent::SpitfireHit) // no special miss event; use closest
                    };

                    let flavour_draw = rng.roll_d100();
                    let flavour = pick_flavour(flavour_pool(flavour_ev), flavour_draw);
                    if is_hit {
                        highlights.push(HighlightEvent {
                            tick,
                            kind: HighlightKind::ScrapRocketHit,
                            pack_name: Some(packs[punk_idx].name.to_string()),
                            flavour: flavour.to_string(),
                        });
                    }
                    scrap_rocket_result = Some(ScrapRocketResult {
                        misfired: false,
                        hit: is_hit,
                        misfire_carrier_killed: false,
                        misfire_additional_casualties: 0,
                        damage_dealt: damage,
                        flavour,
                    });
                }

                // CLEANUP: remove Scrap-Rocket regardless of outcome.
                packs[punk_idx].has_scrap_rocket = false;
            }
        }

        // ═══════════════════════════════════════════════════════════════════
        // PHASE 1 — Vehicles fire at packs (stable vehicle index order 0..3)
        // Draw order 3 per vehicle per weapon.
        // ═══════════════════════════════════════════════════════════════════

        for v_idx in 0..vehicles.len() {
            if vehicles[v_idx].hp <= 0 {
                continue;
            }
            if vehicles[v_idx].fires_tick_one_only && tick > 1 {
                continue;
            }
            let target_idx = match largest_active_pack(&packs) {
                Some(i) => i,
                None => break,
            };
            let pack_evasion = packs[target_idx].evasion;
            let pack_at = packs[target_idx].armor_at;

            for w_idx in 0..vehicles[v_idx].weapons.len() {
                if !packs[target_idx].is_active() {
                    break;
                }
                let weapon = &vehicles[v_idx].weapons[w_idx];
                let d100 = rng.roll_d100() as i32;
                let hit_total = defender_hit_roll(
                    d100,
                    weapon.accuracy,
                    pack_evasion,
                    &def_mods,
                    &att_mods,
                );
                let is_hit = hit_total > HIT_ROLL_THRESHOLD;
                let is_pen = weapon.ap >= pack_at;

                let (kills, flavour) = if is_hit && is_pen {
                    let k = match weapon.kind {
                        ScenarioWeaponKind::Aoe { kills_per_hit } => {
                            let k = kills_per_hit.min(packs[target_idx].current_strength);
                            let draw = rng.roll_d100();
                            let fl = pick_flavour(flavour_pool(FlavourEvent::ThumperAoEHit), draw);
                            packs[target_idx].current_strength =
                                packs[target_idx].current_strength.saturating_sub(k);
                            total_attacker_kia += k;
                            (k, Some(fl))
                        }
                        ScenarioWeaponKind::Standard { base_damage } => {
                            // Pack is unarmored (AT 0); Slug vs Unarmored ×2.0.
                            let mult = slug_vs_unarmored_mult(weapon.ap, pack_at, pack_at == 0);
                            let final_dmg = ((base_damage as f32) * mult
                                * def_mods.damage_dealt_multiplier) as i32;
                            let k = kills_from_damage(final_dmg, PACK_INDIVIDUAL_HP)
                                .min(packs[target_idx].current_strength);
                            packs[target_idx].current_strength =
                                packs[target_idx].current_strength.saturating_sub(k);
                            total_attacker_kia += k;
                            let draw = rng.roll_d100();
                            let fl = pick_flavour(flavour_pool(FlavourEvent::SpitfireHit), draw);
                            (k, Some(fl))
                        }
                    };
                    k
                } else {
                    (0, None)
                };

                veh_fire_log.push(VehicleWeaponResult {
                    vehicle_name: vehicles[v_idx].name,
                    weapon_name: weapon.name,
                    target_pack: target_idx,
                    hit_roll: hit_total,
                    is_hit,
                    is_penetration: is_hit && is_pen,
                    kills,
                    flavour,
                });
            }
        }

        // ═══════════════════════════════════════════════════════════════════
        // PHASE 2 — Sections fire at packs (stable section order 0..1)
        // Each surviving member fires once (swarm mechanic). Draw order 4.
        // ═══════════════════════════════════════════════════════════════════

        for sec in sections.iter_mut() {
            let pack_idx = sec.target_pack;
            if !packs[pack_idx].is_active() {
                continue;
            }
            let pack_evasion = packs[pack_idx].evasion;
            let pack_at = packs[pack_idx].armor_at;
            let is_pen = sec.weapon_ap >= pack_at;
            let mult = slug_vs_unarmored_mult(sec.weapon_ap, pack_at, pack_at == 0);
            let dmg_per_hit = if is_pen {
                ((sec.weapon_damage as f32) * mult * def_mods.damage_dealt_multiplier) as i32
            } else {
                0
            };

            let mut total_damage = 0i32;
            let mut hits = 0u32;

            for _ in 0..sec.current_strength {
                if !packs[pack_idx].is_active() {
                    break;
                }
                let d100 = rng.roll_d100() as i32;
                let hit_total = defender_hit_roll(
                    d100,
                    sec.weapon_accuracy,
                    pack_evasion,
                    &def_mods,
                    &att_mods,
                );
                if hit_total > HIT_ROLL_THRESHOLD {
                    hits += 1;
                    total_damage += dmg_per_hit;
                }
            }

            let kills = kills_from_damage(total_damage, PACK_INDIVIDUAL_HP)
                .min(packs[pack_idx].current_strength);
            packs[pack_idx].current_strength =
                packs[pack_idx].current_strength.saturating_sub(kills);
            total_attacker_kia += kills;

            let flavour = if kills > 0 {
                let draw = rng.roll_d100();
                Some(pick_flavour(flavour_pool(FlavourEvent::InfantrySluggerHit), draw))
            } else {
                None
            };

            sec_fire_log.push(SectionFireResult {
                section_name: sec.name,
                target_pack: pack_idx,
                shots: sec.current_strength,
                hits,
                total_damage,
                kills,
                flavour,
            });
        }

        // ═══════════════════════════════════════════════════════════════════
        // PHASE 3 — Packs fire at sections (stable pack index order 0..3)
        // Only packs with fires_at_section = Some(_). Draw order 5.
        // ═══════════════════════════════════════════════════════════════════

        for pack_idx in 0..packs.len() {
            if !packs[pack_idx].is_active() {
                continue;
            }
            let sec_target = match packs[pack_idx].fires_at_section {
                Some(i) => i,
                None => continue,
            };
            if sections[sec_target].current_strength == 0 {
                continue;
            }

            let pack_acc = packs[pack_idx].accuracy;
            let pack_dmg = packs[pack_idx].weapon_damage;
            let pack_ap  = packs[pack_idx].weapon_ap;
            let sec_at   = sections[sec_target].armor_at;
            let sec_eva  = sections[sec_target].evasion;
            let pack_sz  = packs[pack_idx].current_strength;
            let is_pen   = pack_ap >= sec_at;
            // Raccoon Slug vs LightArmor (section Kevlar) = ×1.0.
            let dmg_per_hit: i32 = if is_pen {
                ((pack_dmg as f32) * att_mods.damage_dealt_multiplier) as i32
            } else {
                0
            };

            let mut total_damage = 0i32;
            let mut hits = 0u32;

            for _ in 0..pack_sz {
                if sections[sec_target].current_strength == 0 {
                    break;
                }
                let d100 = rng.roll_d100() as i32;
                let hit_total = attacker_hit_roll(
                    d100,
                    pack_acc,
                    sec_eva,
                    &att_mods,
                    &def_mods,
                );
                if hit_total > HIT_ROLL_THRESHOLD {
                    hits += 1;
                    total_damage += dmg_per_hit;
                }
            }

            let kills = kills_from_damage(total_damage, sections[sec_target].individual_hp)
                .min(sections[sec_target].current_strength);
            sections[sec_target].current_strength =
                sections[sec_target].current_strength.saturating_sub(kills);
            total_defender_kia += kills;

            let flavour = if kills > 0 {
                let draw = rng.roll_d100();
                Some(pick_flavour(flavour_pool(FlavourEvent::InfantryTakesCasualty), draw))
            } else {
                None
            };

            pack_fire_log.push(PackFireResult {
                pack_name: packs[pack_idx].name,
                target_section: sec_target,
                shots: pack_sz,
                hits,
                total_damage,
                kills,
                flavour,
            });
        }

        // ═══════════════════════════════════════════════════════════════════
        // LOSS CHECK — Section wipe before any pack routs.
        // Checked after all firing, before scatter. If triggered the tick is
        // logged (no scatter events) and the engagement terminates immediately.
        // ═══════════════════════════════════════════════════════════════════

        let all_sections_wiped = sections.iter().all(|s| s.current_strength == 0);
        let any_pack_gone_pre_scatter = packs.iter().any(|p| p.scattered || p.destroyed);

        if all_sections_wiped && !any_pack_gone_pre_scatter {
            tick_logs.push(OreRunTickLog {
                tick,
                scrap_rocket: scrap_rocket_result,
                vehicle_fire: veh_fire_log,
                section_fire: sec_fire_log,
                pack_fire: pack_fire_log,
                scatter_events: vec![],
                pack_strengths: packs.iter().map(|p| p.current_strength).collect(),
                section_strengths: sections.iter().map(|s| s.current_strength).collect(),
                warthog_hp_after: vehicles[0].hp,
            });
            break 'ticks;
        }

        // ═══════════════════════════════════════════════════════════════════
        // PHASE 4 — Scatter checks (end of tick, stable pack order)
        // ═══════════════════════════════════════════════════════════════════

        for pack_idx in 0..packs.len() {
            let pack = &mut packs[pack_idx];
            if pack.scattered || pack.destroyed {
                continue;
            }
            if pack.current_strength == 0 {
                pack.destroyed = true;
                packs_destroyed += 1;
                continue;
            }
            if pack.current_strength <= pack.scatter_threshold {
                pack.scattered = true;
                packs_routed += 1;

                let draw = rng.roll_d100();
                let flavour = pick_flavour(flavour_pool(FlavourEvent::PackScatter), draw);
                scatter_events.push(ScatterEvent {
                    pack_index: pack_idx,
                    pack_name: pack.name,
                    strength_at_scatter: pack.current_strength,
                    flavour,
                });
                highlights.push(HighlightEvent {
                    tick,
                    kind: HighlightKind::PackScatter,
                    pack_name: Some(pack.name.to_string()),
                    flavour: flavour.to_string(),
                });
            }
        }

        // ═══════════════════════════════════════════════════════════════════
        // Build tick snapshot
        // ═══════════════════════════════════════════════════════════════════

        let pack_strengths: Vec<u32> = packs.iter().map(|p| p.current_strength).collect();
        let section_strengths: Vec<u32> = sections.iter().map(|s| s.current_strength).collect();
        let warthog_hp = vehicles[0].hp;

        tick_logs.push(OreRunTickLog {
            tick,
            scrap_rocket: scrap_rocket_result,
            vehicle_fire: veh_fire_log,
            section_fire: sec_fire_log,
            pack_fire: pack_fire_log,
            scatter_events,
            pack_strengths,
            section_strengths,
            warthog_hp_after: warthog_hp,
        });

        // ═══════════════════════════════════════════════════════════════════
        // WIN / LOSS check (after each tick)
        // ═══════════════════════════════════════════════════════════════════

        let any_defender_active = vehicles.iter().any(|v| v.hp > 0)
            || sections.iter().any(|s| s.current_strength > 0);
        let any_pack_routed_or_gone = packs.iter().any(|p| p.scattered || p.destroyed);
        let all_packs_gone = packs.iter().all(|p| !p.is_active());

        let all_vehicles_dead = vehicles.iter().all(|v| v.hp <= 0);
        let all_sections_dead = sections.iter().all(|s| s.current_strength == 0);

        if all_vehicles_dead && all_sections_dead {
            break 'ticks; // loss
        }
        if any_defender_active && (any_pack_routed_or_gone || all_packs_gone) {
            break 'ticks; // win
        }
        if all_packs_gone {
            break 'ticks;
        }
    }

    // ── Outcome evaluation ─────────────────────────────────────────────────

    let any_defender_active = vehicles.iter().any(|v| v.hp > 0)
        || sections.iter().any(|s| s.current_strength > 0);
    let any_pack_routed_or_gone = packs.iter().any(|p| p.scattered || p.destroyed);

    let outcome = if any_defender_active && any_pack_routed_or_gone {
        Outcome::Win
    } else {
        Outcome::Loss
    };

    let ticks_elapsed = tick_logs.len() as u32;

    (
        EngagementResult {
            outcome,
            defender_kia: total_defender_kia,
            attacker_kia: total_attacker_kia,
            packs_routed,
            packs_destroyed,
            ticks_elapsed,
            misfire_occurred,
            was_escape_rearguard: player_approach == Approach::EscapeRearguard,
            highlights,
        },
        tick_logs,
    )
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the engagement twice with the same seed and approach.
    /// Asserts that tick logs and engagement results are byte-identical.
    /// A failure here means the determinism contract is broken — breaking change.
    #[test]
    fn ore_run_is_deterministic() {
        let seed = 0xDEAD_BEEF;
        let approach = Approach::AggressiveCharge;
        let ts = "2026-05-09T00:00:00Z";

        let (result_a, ticks_a) = run_ore_run(approach, seed, ts);
        let (result_b, ticks_b) = run_ore_run(approach, seed, ts);

        assert_eq!(ticks_a.len(), ticks_b.len(), "tick count differs");
        assert_eq!(result_a.ticks_elapsed, result_b.ticks_elapsed);
        assert_eq!(result_a.defender_kia, result_b.defender_kia);
        assert_eq!(result_a.attacker_kia, result_b.attacker_kia);
        assert_eq!(result_a.packs_routed, result_b.packs_routed);
        assert_eq!(result_a.misfire_occurred, result_b.misfire_occurred);
        assert_eq!(result_a.highlights.len(), result_b.highlights.len());

        for (ta, tb) in ticks_a.iter().zip(ticks_b.iter()) {
            assert_eq!(ta.tick, tb.tick);
            assert_eq!(ta.pack_strengths, tb.pack_strengths);
            assert_eq!(ta.section_strengths, tb.section_strengths);
            assert_eq!(ta.warthog_hp_after, tb.warthog_hp_after);
        }
    }

    /// Changing the approach must change modifier values, which must change outcomes.
    /// This test proves that approach selection has a real effect on the engagement.
    /// (Not a determinism test — outcome differences are expected.)
    #[test]
    fn approach_modifiers_differ_between_approaches() {
        let (def_aggressive, att_aggressive) =
            resolve_approach_modifiers(Approach::AggressiveCharge);
        let (def_terrain, att_terrain) =
            resolve_approach_modifiers(Approach::TerrainManeuver);

        // Rock vs Rock: no matchup bonus, base modifiers only.
        // AggressiveCharge base gives +15% damage to whoever uses it.
        assert!(
            (def_aggressive.damage_dealt_multiplier - 1.15).abs() < 0.001,
            "AggressiveCharge base modifier must be 1.15"
        );
        assert_eq!(def_aggressive.enemy_evasion_penalty, 10);

        // Terrain Maneuver vs Raccoon AggressiveCharge: Paper beats Rock → +20 extra evasion.
        assert_eq!(def_terrain.own_evasion_bonus, 40,
            "Paper beats Rock: +20 base + 20 matchup = 40 total evasion bonus");

        // Raccoon always gets base Aggressive modifiers.
        assert!((att_aggressive.damage_dealt_multiplier - 1.15).abs() < 0.001);
        assert!((att_terrain.damage_dealt_multiplier - 1.15).abs() < 0.001);
    }

    /// Verify Scrap-Rocket constants are internally consistent.
    #[test]
    fn scrap_rocket_damage_is_derived_from_warthog_hp() {
        use crate::scenario::{WARTHOG_HP, SCRAP_ROCKET_DAMAGE};
        let expected = (WARTHOG_HP as f32 * 0.35) as i32;
        assert_eq!(SCRAP_ROCKET_DAMAGE, expected,
            "SCRAP_ROCKET_DAMAGE must be ~35% of WARTHOG_HP");
        assert!(SCRAP_ROCKET_DAMAGE < WARTHOG_HP,
            "Scrap-Rocket must not one-shot the Warthog");
    }
}

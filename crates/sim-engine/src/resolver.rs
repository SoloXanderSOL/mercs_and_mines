// AP/AT combat, mission, pack assault, commander stress, and travel/logistics resolvers.
// Source: src/engine/resolver.ts — all formulas preserved for input-log replay parity.
//
// Outcome mapping (Director ruling 2026-04-21 removed Critical variants from OutcomeType):
//   TS "Critical Success" (margin ≥ 25) → FullSuccess
//   TS "Success"          (margin < 25) → PartialSuccess
//   TS "Failure"          (|margin| < 25) → TacticalRetreat
//   TS "Critical Failure" (|margin| ≥ 25) → Wipeout
//
// Type gaps vs. the TS source:
//   UnitDefinition has no `species` field — Hamster/Duck/Boar branches are unreachable.
//   UnitArchetype::Valkyrie and Prospector exist; their ability branches are pending.
//   Terrain has no Underground/Industrial/Urban — those mission-type bonuses are omitted.
//   MissionCategory has no Intel or Mining — those bonuses are omitted.
//   lootRoller.ts is a separate module; loot_drop always returns None until it is ported.

use crate::game_types::{
    AdvisorBoard, BattleReport, Commander, Coordinates, ConvoyRecord,
    DepartureRejected, MissionCategory, MissionDefinition, OutcomeType,
    Rewards, ScoreBreakdown, Squad, StressTier, Unit, UnitBattleResult,
    UnitArchetype, UnitStatus,
};
use crate::rng::{generate_report_id, Rng};
use crate::types::{
    ArmorTag, CombatInitiationType, CombatOutcome, CombatReport, ConvoyVehicle,
    Pack, PackAssaultOutcome, PackAssaultReport, PackFireEvent, PackTickLog,
    Section, SectionFireEvent, TickLog, Vehicle, VehicleWeaponEvent, WeaponTag,
};

// ── Constants ──────────────────────────────────────────────────────────────

const MIN_CHANCE: f64 = 5.0;
const MAX_CHANCE: f64 = 95.0;
const TRAVEL_MINUTES_PER_HEX: i64 = 20;

// ── Private helpers ────────────────────────────────────────────────────────

/// FNV-1a u32 hash of a string — produces a deterministic seed from mission ID + timestamp.
/// Matches the TypeScript generateSeed(id, timestamp) calling convention.
fn seed_from_str(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

/// Damage multiplier table.
/// Ref GDD §3: Laser×HeavyArmor=2.0, Slug×Unarmored=2.0, Missile×Building=2.0.
fn tag_multiplier(weapon: &WeaponTag, armor: &ArmorTag) -> f32 {
    match (weapon, armor) {
        (WeaponTag::Laser,   ArmorTag::HeavyArmor) => 2.0,
        (WeaponTag::Slug,    ArmorTag::Unarmored)   => 2.0,
        (WeaponTag::Missile, ArmorTag::Building)    => 2.0,
        _                                           => 1.0,
    }
}

// ── Score calculators ──────────────────────────────────────────────────────

fn calc_base_skill_score(squad: &Squad) -> f64 {
    if squad.units.is_empty() {
        return 0.0;
    }
    // Hamster Synaptic Overload (use max skill) is preserved as dead code: UnitDefinition
    // has no species field in the current type system. Standard average path runs always.
    let avg = squad.units.iter().map(|u| u.skill as f64).sum::<f64>()
        / squad.units.len() as f64;
    (avg / 10.0) * 50.0
}

fn calc_squad_size_bonus(squad: &Squad) -> i32 {
    let bonus = (squad.units.len() as i32 - 1) * 3;
    bonus.min(4 * 3) // (MAX_SQUAD_SIZE - 1) * SQUAD_SIZE_BONUS_PER_UNIT
}

fn calc_gear_bonus(squad: &Squad) -> i32 {
    squad
        .units
        .iter()
        .flat_map(|u| &u.equipment)
        .map(|e| e.success_bonus)
        .sum()
}

fn calc_total_damage_shield(squad: &Squad) -> f64 {
    if squad.units.is_empty() {
        return 0.0;
    }
    let total: f64 = squad
        .units
        .iter()
        .map(|u| {
            let gear: i32 = u.equipment.iter().map(|e| e.damage_shield).sum();
            (u.definition.damage_shield_mod + gear) as f64
        })
        .sum();
    total / squad.units.len() as f64
}

fn calc_ability_bonus(squad: &Squad) -> i32 {
    let vanguard_count = squad
        .units
        .iter()
        .filter(|u| matches!(u.definition.archetype, UnitArchetype::Vanguard))
        .count() as i32;

    squad.units.iter().fold(0i32, |acc, unit| {
        let mut bonus = acc + unit.definition.success_mod;
        if matches!(unit.definition.archetype, UnitArchetype::Vanguard) {
            // +2 per additional Vanguard beyond the first, capped at +6 total extra.
            bonus += ((vanguard_count - 1) * 2).min(6);
        }
        bonus
    })
}

fn calc_mission_type_modifier(squad: &Squad, mission: &MissionDefinition) -> i32 {
    squad.units.iter().fold(0i32, |acc, unit| {
        let bonus = match unit.definition.archetype {
            // GhostWire: Extraction treated as one tier lower → +7% effective.
            // Intel category not yet in canonical MissionCategory enum.
            UnitArchetype::GhostWire
                if matches!(mission.category, MissionCategory::Extraction) =>
            {
                7
            }
            // TunnelRunner: +8% on Underground terrain.
            // Underground not yet in canonical Terrain enum — unreachable until extended.
            UnitArchetype::TunnelRunner => 0,
            _ => 0,
        };
        acc + bonus
    })
}

fn calc_difficulty_penalty(difficulty: u8) -> i32 {
    difficulty as i32 * 5
}

// ── Damage resolution ──────────────────────────────────────────────────────

fn resolve_unit_damage(
    unit: &Unit,
    mission: &MissionDefinition,
    is_success: bool,
    avg_damage_shield: f64,
    has_sawbones: bool,
    rng: &mut Rng,
) -> UnitBattleResult {
    let success_mult = if is_success { 0.4_f64 } else { 1.5_f64 };
    let raw_hp_loss_chance = mission.base_hp_loss_chance as f64 * success_mult;
    let effective_hp_loss_chance = (raw_hp_loss_chance - avg_damage_shield).clamp(2.0, 95.0);
    let unit_type = format!("{:?}", unit.definition.archetype);

    if !rng.chance(effective_hp_loss_chance / 100.0) {
        return UnitBattleResult {
            unit_id: unit.id.clone(),
            unit_name: unit.name.clone(),
            unit_type,
            emoji: unit.definition.emoji.clone(),
            hp_lost: 0,
            hp_remaining: unit.current_hp,
            final_status: UnitStatus::Ready,
            status_note: "Unscathed".into(),
        };
    }

    let hp_lost: i32 = if is_success {
        if rng.roll_d100() <= 60 { 1 } else { 2 }
    } else {
        if rng.roll_d100() <= 40 { 2 } else { 3 }
    };
    let hp_remaining = (unit.current_hp - hp_lost).max(0);

    let kia_base = if is_success { 10.0_f64 } else { 35.0_f64 };
    let kia_chance = kia_base * mission.base_kia_multiplier as f64;

    if rng.chance(kia_chance / 100.0) {
        // Sawbones TRAUMA PROTOCOL — 30% chance to convert KIA → Wounded.
        if has_sawbones && rng.chance(0.30) {
            return UnitBattleResult {
                unit_id: unit.id.clone(),
                unit_name: unit.name.clone(),
                unit_type,
                emoji: unit.definition.emoji.clone(),
                hp_lost,
                hp_remaining: hp_remaining.max(1),
                final_status: UnitStatus::Wounded,
                status_note: "KIA → Injured (Sawbones: Trauma Protocol)".into(),
            };
        }
        // Valkyrie RAPID EXTRACTION: guaranteed 50%+ survival on failure.
        // Full implementation pending — no KIA path wired here yet.
        return UnitBattleResult {
            unit_id: unit.id.clone(),
            unit_name: unit.name.clone(),
            unit_type,
            emoji: unit.definition.emoji.clone(),
            hp_lost: unit.current_hp,
            hp_remaining: 0,
            final_status: UnitStatus::Kia,
            status_note: format!("KIA — lost in action on {}", mission.name),
        };
    }

    UnitBattleResult {
        unit_id: unit.id.clone(),
        unit_name: unit.name.clone(),
        unit_type,
        emoji: unit.definition.emoji.clone(),
        hp_lost,
        hp_remaining,
        final_status: UnitStatus::Wounded,
        status_note: format!("Injured — {} HP lost", hp_lost),
    }
}

// ── Reward calculation ─────────────────────────────────────────────────────

fn calc_rewards(
    mission: &MissionDefinition,
    outcome: &OutcomeType,
    _squad: &Squad,
    _rng: &mut Rng,
) -> Rewards {
    if matches!(outcome, OutcomeType::TacticalRetreat | OutcomeType::Wipeout) {
        return Rewards { credits: 0, ore: 0, loot_drop: None };
    }
    let multiplier: f64 = if matches!(outcome, OutcomeType::FullSuccess) { 1.5 } else { 1.0 };
    let credits = (mission.credit_reward as f64 * multiplier).round() as u32;
    let ore     = (mission.ore_reward     as f64 * multiplier).round() as u32;
    // rollLoot (lootRoller.ts) not yet ported — loot_drop is None until that module lands.
    Rewards { credits, ore, loot_drop: None }
}

// ── Mission resolver ───────────────────────────────────────────────────────

pub fn resolve_mission(
    squad: &Squad,
    mission: &MissionDefinition,
    timestamp: &str,
    seed_override: Option<u32>,
) -> BattleReport {
    let seed = seed_override
        .unwrap_or_else(|| seed_from_str(&format!("{}{}", mission.id, timestamp)));
    let mut rng = Rng::new(seed);

    let base_skill_score_f    = (calc_base_skill_score(squad) * 10.0).round() / 10.0;
    let squad_size_bonus      = calc_squad_size_bonus(squad);
    let gear_bonus            = calc_gear_bonus(squad);
    let ability_bonus         = calc_ability_bonus(squad);
    let commander_bonus       = squad.commander.as_ref().map_or(0, |c| c.success_aura);
    // Biscuit Coefficient requires a Hamster unit — no species field on UnitDefinition yet.
    let biscuit_coefficient   = 0i32;
    let mission_type_modifier = calc_mission_type_modifier(squad, mission);
    let difficulty_penalty    = calc_difficulty_penalty(mission.difficulty);

    let raw_total = base_skill_score_f
        + squad_size_bonus as f64
        + gear_bonus as f64
        + ability_bonus as f64
        + commander_bonus as f64
        + biscuit_coefficient as f64
        + mission_type_modifier as f64
        - difficulty_penalty as f64;

    let success_probability = raw_total.clamp(MIN_CHANCE, MAX_CHANCE);
    let raw_roll            = rng.roll_d100() as i32;
    let is_success          = (raw_roll as f64) <= success_probability;
    let margin              = success_probability - raw_roll as f64;

    let outcome = if is_success {
        if margin >= 25.0 { OutcomeType::FullSuccess } else { OutcomeType::PartialSuccess }
    } else {
        if margin.abs() >= 25.0 { OutcomeType::Wipeout } else { OutcomeType::TacticalRetreat }
    };

    let avg_damage_shield = calc_total_damage_shield(squad);
    let has_sawbones = squad
        .units
        .iter()
        .any(|u| matches!(u.definition.archetype, UnitArchetype::Sawbones));

    let unit_results: Vec<UnitBattleResult> = squad
        .units
        .iter()
        .map(|unit| {
            resolve_unit_damage(unit, mission, is_success, avg_damage_shield, has_sawbones, &mut rng)
        })
        .collect();

    let rewards = calc_rewards(mission, &outcome, squad, &mut rng);

    let narrative_tag = format!("{:?}_{:?}_{:?}", outcome, mission.category, mission.terrain);

    // successThreshold = successProbability - margin = rawRoll (TS formula preserved exactly).
    let score_breakdown = ScoreBreakdown {
        base_skill_score:     base_skill_score_f.round() as i32,
        squad_size_bonus,
        gear_bonus,
        ability_bonus,
        commander_bonus,
        biscuit_coefficient,
        mission_type_modifier,
        difficulty_penalty:   -difficulty_penalty,
        total_score:          success_probability.round() as i32,
        success_threshold:    raw_roll,
        raw_roll,
        margin:               margin.round() as i32,
    };

    BattleReport {
        report_id:        generate_report_id(&mut rng),
        timestamp:        timestamp.to_owned(),
        mission_id:       mission.id.clone(),
        mission_name:     mission.name.clone(),
        mission_category: mission.category.clone(),
        difficulty:       mission.difficulty,
        terrain:          mission.terrain.clone(),
        commander_name:   squad.commander.as_ref().map(|c| c.name.clone()),
        outcome,
        score_breakdown,
        unit_results,
        rewards,
        narrative_tag,
    }
}

// ── AP vs AT combat engine ─────────────────────────────────────────────────

pub fn resolve_combat(
    section: &Section,
    vehicle: &Vehicle,
    timestamp: &str,
    max_ticks: u32,
    seed_override: Option<u32>,
    combat_initiation_type: CombatInitiationType,
    defending_convoy_vehicles: Vec<ConvoyVehicle>,
) -> CombatReport {
    let seed = seed_override.unwrap_or_else(|| {
        seed_from_str(&format!("{}{}{}", section.id, vehicle.id, timestamp))
    });
    let mut rng = Rng::new(seed);

    let mut current_strength = section.current_strength;
    let mut vehicle_hp       = vehicle.hp;
    let mut ticks: Vec<TickLog> = Vec::new();
    let mut outcome = CombatOutcome::MaxTicksReached;

    for tick in 1..=max_ticks {

        // ── Phase 1: Vehicle fires each weapon at the Section ──────────────

        let mut vehicle_events: Vec<VehicleWeaponEvent> = Vec::new();

        for weapon in &vehicle.weapons {
            if current_strength == 0 {
                break;
            }
            let dice_roll      = rng.roll_d100() as i32;
            let hit_roll_total = dice_roll + weapon.accuracy - section.evasion;
            let is_hit         = hit_roll_total > 50;
            let hit_breakdown  = format!(
                "D100({}) + {} - {} = {}",
                dice_roll, weapon.accuracy, section.evasion, hit_roll_total
            );
            let ap_vs_at = format!("AP {} vs AT {}", weapon.ap, section.armor_at);

            if !is_hit {
                vehicle_events.push(VehicleWeaponEvent {
                    weapon_name: weapon.name.clone(),
                    hit_roll_breakdown: hit_breakdown,
                    is_hit: false,
                    ap_vs_at,
                    is_penetration: false,
                    raw_damage: 0,
                    tag_multiplier: 1.0,
                    final_damage: 0,
                    kill_count: 0,
                });
                continue;
            }

            let is_penetration = weapon.ap >= section.armor_at;
            if !is_penetration {
                vehicle_events.push(VehicleWeaponEvent {
                    weapon_name: weapon.name.clone(),
                    hit_roll_breakdown: hit_breakdown,
                    is_hit: true,
                    ap_vs_at,
                    is_penetration: false,
                    raw_damage: weapon.base_damage,
                    tag_multiplier: 1.0,
                    final_damage: 0,
                    kill_count: 0,
                });
                continue;
            }

            let mult        = tag_multiplier(&weapon.tag, &section.armor_tag);
            let final_damage = (weapon.base_damage as f32 * mult).floor() as i32;
            let kill_count  = ((final_damage as f32 / section.individual_hp as f32).ceil() as u32)
                .min(current_strength);
            current_strength -= kill_count;

            vehicle_events.push(VehicleWeaponEvent {
                weapon_name: weapon.name.clone(),
                hit_roll_breakdown: hit_breakdown,
                is_hit: true,
                ap_vs_at,
                is_penetration: true,
                raw_damage: weapon.base_damage,
                tag_multiplier: mult,
                final_damage,
                kill_count,
            });
        }

        // ── Phase 2: Section swarm fires at the Vehicle ────────────────────
        //    Suppressed on Tick 1 of an AMBUSH engagement.

        let is_ambush_tick1 =
            matches!(combat_initiation_type, CombatInitiationType::Ambush) && tick == 1;

        let section_event = if !is_ambush_tick1 {
            let shots_total  = current_strength;
            let sw           = &section.weapon;
            let is_pen       = sw.ap >= vehicle.at;
            let mult         = tag_multiplier(&sw.tag, &vehicle.armor_tag);
            let dmg_per_shot = if is_pen { (sw.base_damage as f32 * mult).floor() as i32 } else { 0 };
            let tag_note     = format!("{:?} vs {:?}", sw.tag, vehicle.armor_tag);
            let mut hits_total   = 0u32;
            let mut total_damage = 0i32;

            for _ in 0..shots_total {
                if vehicle_hp <= 0 {
                    break;
                }
                let dice      = rng.roll_d100() as i32;
                let hit_total = dice + sw.accuracy - vehicle.evasion;
                if hit_total > 50 {
                    hits_total += 1;
                    if is_pen {
                        vehicle_hp   = (vehicle_hp - dmg_per_shot).max(0);
                        total_damage += dmg_per_shot;
                    }
                }
            }

            Some(SectionFireEvent {
                shots_total,
                hits_total,
                ap_vs_at:             format!("AP {} vs AT {}", sw.ap, vehicle.at),
                is_penetration:       is_pen,
                raw_damage_per_shot:  sw.base_damage,
                tag_multiplier:       mult,
                tag_note,
                final_damage_per_shot: dmg_per_shot,
                total_damage,
                kill_count: None, // section fires at a Vehicle — no infantry kill count
            })
        } else {
            None
        };

        ticks.push(TickLog {
            tick,
            vehicle_events,
            section_event,
            defender_suppressed:   is_ambush_tick1,
            section_strength_after: current_strength,
            vehicle_hp_after:       vehicle_hp,
        });

        if current_strength == 0 && vehicle_hp <= 0 { outcome = CombatOutcome::Draw;           break; }
        if vehicle_hp <= 0                           { outcome = CombatOutcome::SectionVictory;  break; }
        if current_strength == 0                     { outcome = CombatOutcome::VehicleVictory;  break; }
    }

    let casualties = section.current_strength - current_strength;
    let narrative_summary = match &outcome {
        CombatOutcome::SectionVictory => format!(
            "{} destroyed {} in {} tick(s). Section at {}/{} strength. \
             {} KIA — a tactical victory, an attritional nightmare.",
            section.name, vehicle.name, ticks.len(),
            current_strength, section.max_strength, casualties
        ),
        CombatOutcome::VehicleVictory => format!(
            "{} eliminated {} in {} tick(s). Section wiped. Vehicle at {}/{} HP.",
            vehicle.name, section.name, ticks.len(), vehicle_hp, vehicle.max_hp
        ),
        CombatOutcome::Draw => format!(
            "Mutual destruction in {} tick(s). Both units destroyed simultaneously.",
            ticks.len()
        ),
        CombatOutcome::MaxTicksReached => format!(
            "Engagement inconclusive after {} ticks. Section: {}/{} | Vehicle: {}/{} HP.",
            max_ticks, current_strength, section.max_strength, vehicle_hp, vehicle.max_hp
        ),
    };

    CombatReport {
        report_id:                generate_report_id(&mut rng),
        timestamp:                timestamp.to_owned(),
        section_name:             section.name.clone(),
        vehicle_name:             vehicle.name.clone(),
        combat_initiation_type,
        ticks,
        outcome,
        section_final_strength:   current_strength,
        section_max_strength:     section.max_strength,
        vehicle_final_hp:         vehicle_hp,
        vehicle_max_hp:           vehicle.max_hp,
        narrative_summary,
        defending_convoy_vehicles,
    }
}

// ── Pack assault resolver ──────────────────────────────────────────────────

pub fn resolve_pack_assault(
    section: &Section,
    pack: &Pack,
    timestamp: &str,
    combat_initiation_type: CombatInitiationType,
    defending_convoy_vehicles: Vec<ConvoyVehicle>,
    max_ticks: u32,
    seed_override: Option<u32>,
) -> PackAssaultReport {
    let seed = seed_override.unwrap_or_else(|| {
        seed_from_str(&format!("{}{}{}", section.id, pack.id, timestamp))
    });
    let mut rng = Rng::new(seed);

    let mut sec_strength  = section.current_strength;
    let mut pack_strength = pack.current_strength;
    let mut ticks: Vec<PackTickLog> = Vec::new();
    let mut outcome = PackAssaultOutcome::MaxTicksReached;

    // Pre-compute weapon values — these don't change per tick.
    let pw          = &pack.weapon;
    let pw_pen      = pw.ap >= section.armor_at;
    let pw_mult     = tag_multiplier(&pw.tag, &section.armor_tag);
    let pw_dmg      = if pw_pen { (pw.base_damage as f32 * pw_mult).floor() as i32 } else { 0 };
    let pw_tag_note = format!("{:?} vs {:?}", pw.tag, section.armor_tag);

    let sw          = &section.weapon;
    let sw_pen      = sw.ap >= pack.armor_at;
    let sw_mult     = tag_multiplier(&sw.tag, &pack.armor_tag);
    let sw_dmg      = if sw_pen { (sw.base_damage as f32 * sw_mult).floor() as i32 } else { 0 };
    let sw_tag_note = format!("{:?} vs {:?}", sw.tag, pack.armor_tag);

    for tick in 1..=max_ticks {

        // ── Phase 1: Pack fires at Section (swarm) ─────────────────────────

        let pack_shots = pack_strength;
        let mut pack_hits   = 0u32;
        let mut pack_damage = 0i32;

        for _ in 0..pack_shots {
            if sec_strength == 0 {
                break;
            }
            let roll      = rng.roll_d100() as i32;
            let hit_total = roll + pw.accuracy - section.evasion;
            if hit_total > 50 {
                pack_hits   += 1;
                pack_damage += pw_dmg;
            }
        }

        let pack_kills = ((pack_damage as f32 / section.individual_hp as f32).floor() as u32)
            .min(sec_strength);
        sec_strength -= pack_kills;

        let pack_fire_event = PackFireEvent {
            shots_total:        pack_shots,
            hits_total:         pack_hits,
            ap_vs_at:           format!("AP {} vs AT {}", pw.ap, section.armor_at),
            is_penetration:     pw_pen,
            raw_damage_per_shot: pw.base_damage,
            tag_multiplier:     pw_mult,
            tag_note:           pw_tag_note.clone(),
            final_damage_per_shot: pw_dmg,
            total_damage:       pack_damage,
            kill_count:         pack_kills,
        };

        // ── Phase 2: Section fires at Pack ─────────────────────────────────
        //    Suppressed on Tick 1 of an AMBUSH engagement.

        let is_ambush_tick1 =
            matches!(combat_initiation_type, CombatInitiationType::Ambush) && tick == 1;

        let section_fire_event = if !is_ambush_tick1 {
            let sec_shots    = sec_strength; // survivors fire (post-pack-fire count)
            let mut sec_hits   = 0u32;
            let mut sec_damage = 0i32;

            for _ in 0..sec_shots {
                if pack_strength == 0 {
                    break;
                }
                let roll      = rng.roll_d100() as i32;
                let hit_total = roll + sw.accuracy - pack.evasion;
                if hit_total > 50 {
                    sec_hits   += 1;
                    sec_damage += sw_dmg;
                }
            }

            let sec_kills = ((sec_damage as f32 / pack.individual_hp as f32).floor() as u32)
                .min(pack_strength);
            pack_strength -= sec_kills;

            Some(SectionFireEvent {
                shots_total:        sec_shots,
                hits_total:         sec_hits,
                ap_vs_at:           format!("AP {} vs AT {}", sw.ap, pack.armor_at),
                is_penetration:     sw_pen,
                raw_damage_per_shot: sw.base_damage,
                tag_multiplier:     sw_mult,
                tag_note:           sw_tag_note.clone(),
                final_damage_per_shot: sw_dmg,
                total_damage:       sec_damage,
                kill_count:         Some(sec_kills),
            })
        } else {
            None
        };

        // Scatter check — pack breaks when at or below threshold (still alive).
        let pack_scattered = pack_strength > 0 && pack_strength <= pack.scatter_threshold;

        ticks.push(PackTickLog {
            tick,
            pack_fire_event,
            section_fire_event,
            defender_suppressed:    is_ambush_tick1,
            section_strength_after: sec_strength,
            pack_strength_after:    pack_strength,
            pack_scattered,
        });

        if pack_scattered                          { outcome = PackAssaultOutcome::PackScattered;  break; }
        if sec_strength == 0 && pack_strength == 0 { outcome = PackAssaultOutcome::Draw;           break; }
        if pack_strength == 0                      { outcome = PackAssaultOutcome::SectionVictory;  break; }
        if sec_strength == 0                       { outcome = PackAssaultOutcome::PackVictory;     break; }
    }

    let sec_losses  = section.current_strength - sec_strength;
    let pack_losses = pack.current_strength    - pack_strength;
    let narrative_summary = match &outcome {
        PackAssaultOutcome::SectionVictory => format!(
            "{} eliminated the {} in {} tick(s). Pack wiped. Section at {}/{}. \
             {} KIA — a brutal engagement.",
            section.name, pack.name, ticks.len(),
            sec_strength, section.max_strength, sec_losses
        ),
        PackAssaultOutcome::PackVictory => format!(
            "{} overwhelmed {} in {} tick(s). Section wiped. Pack at {}/{} ({} down). {}",
            pack.name, section.name, ticks.len(),
            pack_strength, pack.max_strength, pack_losses,
            if matches!(combat_initiation_type, CombatInitiationType::Ambush) {
                "Ambush conditions gave them the decisive advantage."
            } else {
                "Weight of numbers decided it."
            }
        ),
        PackAssaultOutcome::PackScattered => format!(
            "{} broke and scattered at {}/{} headcount. {} stands at {}/{}. \
             Pack threshold ({}) reached — survivors flee.",
            pack.name, pack_strength, pack.max_strength,
            section.name, sec_strength, section.max_strength,
            pack.scatter_threshold
        ),
        PackAssaultOutcome::Draw => format!(
            "Mutual destruction in {} tick(s). Both sides wiped simultaneously.",
            ticks.len()
        ),
        PackAssaultOutcome::MaxTicksReached => format!(
            "Engagement inconclusive after {} ticks. Section: {}/{} | Pack: {}/{}.",
            max_ticks, sec_strength, section.max_strength,
            pack_strength, pack.max_strength
        ),
    };

    PackAssaultReport {
        report_id:            generate_report_id(&mut rng),
        timestamp:            timestamp.to_owned(),
        section_name:         section.name.clone(),
        pack_name:            pack.name.clone(),
        combat_initiation_type,
        ticks,
        outcome,
        section_final_strength: sec_strength,
        section_max_strength:   section.max_strength,
        pack_final_strength:    pack_strength,
        pack_max_strength:      pack.max_strength,
        narrative_summary,
        defending_convoy_vehicles,
    }
}

// ── Commander stress system ────────────────────────────────────────────────

/// Returns the StressTier for a Commander based on their current stress_level.
/// Thresholds: 0–30 RESTED | 31–70 STRAINED | 71–99 BREAKING_POINT | 100 SHATTERED
pub fn get_stress_tier(commander: &Commander) -> StressTier {
    match commander.stress_level {
        100     => StressTier::Shattered,
        71..=99 => StressTier::BreakingPoint,
        31..=70 => StressTier::Strained,
        _       => StressTier::Rested,
    }
}

fn clamp_and_check_shattered(commander: &mut Commander) {
    commander.stress_level = commander.stress_level.min(100);
    if matches!(get_stress_tier(commander), StressTier::Shattered) {
        commander.is_shattered = true;
    }
}

/// Applies a Commander's passive buffs (or debuffs) to their attached Section.
///
/// Scaling by stress tier (Ref: Commander_Stress_System.md §2):
///   RESTED        → full buff  (×1.0)
///   STRAINED      → half buff  (×0.5)
///   BREAKING_POINT → inverted debuff (×−1.0); locks can_retreat = false
///   SHATTERED     → no effect
pub fn apply_commander_buffs(commander: &mut Commander, section: &mut Section) {
    let multiplier = match get_stress_tier(commander) {
        StressTier::Shattered     => return,
        StressTier::Rested        => 1.0_f32,
        StressTier::Strained      => 0.5_f32,
        StressTier::BreakingPoint => { commander.can_retreat = false; -1.0_f32 }
    };
    let b = &commander.passive_buffs;
    section.accuracy += (b.accuracy        as f32 * multiplier) as i32;
    section.evasion  += (b.evasion         as f32 * multiplier) as i32;
    section.armor_at += (b.damage_reduction as f32 * multiplier) as i32;
}

/// Variant of `apply_commander_buffs` for Vehicle targets (evasion + AT only).
pub fn apply_commander_buffs_to_vehicle(commander: &mut Commander, vehicle: &mut Vehicle) {
    let multiplier = match get_stress_tier(commander) {
        StressTier::Shattered     => return,
        StressTier::Rested        => 1.0_f32,
        StressTier::Strained      => 0.5_f32,
        StressTier::BreakingPoint => { commander.can_retreat = false; -1.0_f32 }
    };
    let b = &commander.passive_buffs;
    vehicle.evasion += (b.evasion         as f32 * multiplier) as i32;
    vehicle.at      += (b.damage_reduction as f32 * multiplier) as i32;
}

/// Applies the one-time deployment stress penalty at mission start (+10).
/// Call exactly once per deployment before combat begins.
/// NOTE: is_kia is never touched here.
pub fn apply_deployment_penalty(commander: &mut Commander) {
    commander.stress_level =
        (commander.stress_level as u32 + 10).min(100) as u8;
    clamp_and_check_shattered(commander);
}

/// Applies mid-battle casualty stress (+5 per casualty).
/// Call once per casualty event. Removing from the active roster is the caller's responsibility.
/// NOTE: is_kia is never touched here.
pub fn resolve_commander_stress(commander: &mut Commander, casualties: u32) {
    commander.stress_level =
        (commander.stress_level as u32 + casualties * 5).min(100) as u8;
    clamp_and_check_shattered(commander);
}

/// Checks whether a Commander suffers Permadeath due to a total unit wipeout.
/// Sets is_kia = true if unit_remaining_count == 0.
/// NOTE: is_shattered is never touched here. These flags are fully independent.
pub fn check_commander_permadeath(commander: &mut Commander, unit_remaining_count: u32) {
    if unit_remaining_count == 0 {
        commander.is_kia = true;
    }
}

/// Retires a Rank 5 Commander to the AdvisorBoard.
/// Returns Err if the Commander has not reached Rank 5.
pub fn retire_commander(
    commander: Commander,
    advisor_board: &mut AdvisorBoard,
) -> Result<(), String> {
    if commander.rank < 5 {
        return Err("Commander has not reached the rank required for retirement.".into());
    }
    advisor_board.push(commander);
    Ok(())
}

// ── Travel & fuel logistics ────────────────────────────────────────────────

/// Canonical axial hex distance formula.
/// Returns the number of hexes between two axial coordinates.
pub fn calculate_hex_distance(a: &Coordinates, b: &Coordinates) -> u32 {
    let dq = (a.q - b.q).abs() as i64;
    let ds = ((a.q + a.r) - (b.q + b.r)).abs() as i64;
    let dr = (a.r - b.r).abs() as i64;
    ((dq + ds + dr) / 2) as u32
}

/// Total He3 required for a convoy to travel `distance` hexes.
/// Each vehicle contributes its own fuel_cost_per_hex; the fleet total is multiplied by distance.
pub fn calculate_fuel_cost(distance: u32, vehicles: &[ConvoyVehicle]) -> u32 {
    vehicles.iter().map(|v| v.fuel_cost_per_hex).sum::<u32>() * distance
}

/// Attempts to deploy a convoy from origin to destination.
///
/// If fuel is insufficient returns Err(DepartureRejected) — no record is created.
/// If fuel is sufficient returns Ok(ConvoyRecord) with arrival_time computed as:
///   departure_time + (distance × 20 min × 60 s/min)
pub fn deploy_convoy(
    origin: Coordinates,
    destination: Coordinates,
    departure_time: i64,
    fuel_loaded: u32,
    vehicles: Vec<ConvoyVehicle>,
) -> Result<ConvoyRecord, DepartureRejected> {
    let distance       = calculate_hex_distance(&origin, &destination);
    let total_fuel_cost = calculate_fuel_cost(distance, &vehicles);

    if fuel_loaded < total_fuel_cost {
        return Err(DepartureRejected {
            fuel_loaded,
            total_fuel_cost,
            shortfall: total_fuel_cost - fuel_loaded,
        });
    }

    let arrival_time = departure_time + distance as i64 * TRAVEL_MINUTES_PER_HEX * 60;

    Ok(ConvoyRecord {
        origin,
        destination,
        departure_time,
        arrival_time,
        fuel_loaded,
        vehicles,
        is_dead_duck: false,
    })
}

/// Returns true if the convoy is currently vulnerable to attack.
///
/// Vulnerable when in transit (current_time < arrival_time) OR in Dead Duck state.
/// The Dead Duck flag is checked independently because a future event can freeze
/// the arrival timer, making the time check alone return false while the convoy
/// remains permanently vulnerable.
pub fn is_vulnerable(current_time: i64, convoy: &ConvoyRecord) -> bool {
    current_time < convoy.arrival_time || convoy.is_dead_duck
}

// ── Detection & Fog of War ─────────────────────────────────────────────────

/// Returns the total noise radius projected by a convoy.
/// Any observer whose detection_radius overlaps this value will detect the convoy.
pub fn calculate_convoy_noise(convoy: &ConvoyRecord) -> u32 {
    convoy.vehicles.iter().map(|v| v.radar_signature).sum()
}

/// Returns true if the observer detects the target at the given distance.
///
/// The effective detection range is whichever is larger: the observer's vision radius
/// or the noise the target is broadcasting.
/// Formula: detection_range = max(observer_vision_radius, target_noise_radius)
///          detected = distance_in_hexes <= detection_range
pub fn check_detection(
    observer_vision_radius: u32,
    target_noise_radius: u32,
    distance_in_hexes: u32,
) -> bool {
    let detection_range = observer_vision_radius.max(target_noise_radius);
    distance_in_hexes <= detection_range
}

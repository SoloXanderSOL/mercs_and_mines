// The battle loop (BP-3b-2, demo plan §4a screen 6): Missions → briefing → one mission POST →
// a presentation of the response → the after-action report → the hub, or the end of the run.
//
// It presents the mission route's report; nothing is replayed and nothing is recomputed: the
// outcome is `report.outcome`, the chance is `raw_roll + margin`. The squad is one Section of
// eight identical Rookies (ODQ-63); the archetype standing in for them is never put on screen,
// so the AAR rows are built here from `final_status` and `hp_lost` only. Every server string
// goes in through textContent. Pacing is CSS transitions plus requestAnimationFrame: no timers.

import { ApiError, store, counters, getMissions, getUnits, postMission, postAbandon } from './api.js';

const STAND_IN = 'GHOST_WIRE'; // ODQ-63 stand-in, never shown
const SQUAD_SIZE = 8;          // one Section: at most 8 units (MAX_UNITS_PER_SECTION)
const ORDER = ['GUARD_DUTY', 'ORE_EXTRACTION', 'SUPPLY_RAID', 'BLACK_SITE_RAID'];
const GLYPH = '🪖';            // the same for every Rookie

const ART = {
  briefing: 'assets/art/pre_battle.png',
  battle: 'assets/art/mid_battle.png',
  Success: 'assets/art/victory_player.png',
  Defeat: 'assets/art/escaped_sorry_tim.png',
  Wipeout: 'assets/art/mid_battle.png', // shown greyscale (.still-wipeout)
};
const MUSIC = {
  hub: 'assets/music/pre_battle.mp3',
  battle: 'assets/music/battle_loop.mp3',
  Success: 'assets/music/victory.mp3',
  Defeat: 'assets/music/defeat.mp3',
  Wipeout: 'assets/music/defeat.mp3',
};

// Booth copy, verbatim from the BP-3b-2 prompt.
const COPY = {
  missionsTitle: 'Contracts on offer',
  missionsBack: 'Back',
  missionDifficulty: 'Difficulty',
  missionReward: '{credits} credits · {ore} ore',
  missionRewardNoOre: '{credits} credits',
  briefingSquad: '1st Section, 8 Rookies',
  briefingCommander: 'Commanding: {name}, stress {stress}',
  briefingDeploy: 'Deploy',
  briefingBack: 'Back',
  debugOneRookie: 'Send 1 Rookie (debug)',
  battleWorking: 'Deploying the 1st Section...',
  battleScore: { base: 'Squad skill', squad: 'Squad size', ability: 'Training', gear: 'Gear', mission: 'Mission fit', difficulty: 'Difficulty' },
  battleChance: 'Chance of success: {n}%',
  battleRoll: 'Rolled {n}',
  battleSkip: 'Skip',
  battleContinue: 'Continue',
  outcome: { Success: 'Mission accomplished', Defeat: 'Mission failed', Wipeout: 'No survivors' },
  aarTitle: 'After-action report',
  aarUnits: '1st Section',
  unitName: 'Rookie {n}',
  unitStatus: { Ready: 'Unscathed', Wounded: 'Wounded, {n} HP lost', KIA: 'Killed in action' },
  aarRewards: 'Pay',
  aarNoRewards: 'No pay. Command does not reward effort.',
  aarLoot: 'Recovered: {item}',
  aarStress: 'Stress {n} (+{d})',
  aarXp: '+{n} XP',
  aarRankUp: 'Promoted: rank {a} to {b}',
  aarContinue: 'Continue',
  endTitle: 'Killed in action',
  endShattered: 'Shattered',
  endBodyKia: 'The Company thanks {name} for their service, and has already reassigned their desk.',
  endBodyShattered: '{name} has stopped answering to their name. Command has stopped asking.',
  endGlorious: 'It was, by every account, glorious.',
  endNewRun: 'Hire a new Commander',
  newRunWorking: 'Filing the paperwork...',
  missionBusy: 'Your Section is already in the field. Wait for its report.',
  missionLost: 'Lost contact mid-mission. It may have gone ahead without you. Tap to check in with Command.',
  missionSlowDown: 'Command is still processing your last deployment. Try again in {n} s.',
  missionRefused: 'Command rejected those orders. Tap to go back.',
};

// The six score lines, in presentation order.
const SCORE_LINES = [
  ['base', 'base_skill_score'],
  ['squad', 'squad_size_bonus'],
  ['ability', 'ability_bonus'],
  ['gear', 'gear_bonus'],
  ['mission', 'mission_type_modifier'],
  ['difficulty', 'difficulty_penalty'], // already negative: shown as is
];

const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

// What runs between the first non-death AAR and the checkout offer. By default, nothing.
let beforeOffer = (ctx, next) => next();
export function setBeforeOffer(fn) { beforeOffer = fn; }

// `deps` is main.js's side of the client: screens, the error screen, the roster it keeps,
// the one Audio element, and the flows the battle hands back to (hub, reveal, join).
export function createBattle(deps) {
  const { $ } = deps;
  let busy = false;          // one Deploy or New run at a time
  let missions = null;       // GET /api/missions
  let units = null;          // GET /api/units
  let mission = null;        // the contract on the briefing
  let result = null;         // the last 200 body this tab holds
  let resultFor = null;      // the Commander that response belongs to
  let run = 0;               // the presentation in progress; Skip and a new run bump it
  let oneRookie = null;      // the debug checkbox, created only under DEBUG

  function music(src, loop = true) {
    deps.music.pause();
    deps.music.src = src;
    deps.music.loop = loop; // victory and defeat play once, then silence
    deps.music.play().catch(() => { /* audio blocked or failed: play on without it */ });
  }

  // A still loads only when its screen opens (no preload).
  function still(img, src, wipeout) {
    img.classList.toggle('still-wipeout', Boolean(wipeout));
    if (img.getAttribute('src') !== src) img.src = src;
  }

  function fillCopy() {
    $('missions-title').textContent = COPY.missionsTitle;
    $('btn-missions-back').textContent = COPY.missionsBack;
    $('briefing-squad').textContent = COPY.briefingSquad;
    $('btn-deploy').textContent = COPY.briefingDeploy;
    $('btn-briefing-back').textContent = COPY.briefingBack;
    $('btn-skip').textContent = COPY.battleSkip;
    $('aar-title').textContent = COPY.aarTitle;
    $('aar-units-heading').textContent = COPY.aarUnits;
    $('aar-pay-heading').textContent = COPY.aarRewards;
    $('btn-aar-continue').textContent = COPY.aarContinue;
    $('btn-new-run').textContent = COPY.endNewRun;
    if (deps.DEBUG) { // the 1-Rookie toggle exists only under ?debug=1
      const label = el('label', 'debug-toggle', COPY.debugOneRookie);
      oneRookie = el('input');
      oneRookie.type = 'checkbox';
      oneRookie.id = 'debug-one-rookie';
      label.prepend(oneRookie);
      $('briefing-actions').prepend(label);
    }
  }

  function wire() {
    $('btn-missions-back').addEventListener('click', deps.enterHub);
    $('btn-briefing-back').addEventListener('click', showMissionList);
    $('btn-deploy').addEventListener('click', deploy);
    $('btn-skip').addEventListener('click', () => { if (result) showAar(); });
    $('btn-aar-continue').addEventListener('click', afterAar);
    $('btn-new-run').addEventListener('click', newRun);
  }

  // ── Missions ─────────────────────────────────────────────────────────────
  async function openMissions() {
    if (!missions || !units) {
      $('working-text').textContent = '';
      deps.show('screen-working');
      try {
        [missions, units] = await Promise.all([getMissions(), getUnits()]);
      } catch (e) {
        return deps.handleError(e, openMissions); // two static GETs: a retry is safe
      }
    }
    showMissionList();
  }

  function pips(difficulty) {
    const d = Math.max(0, Math.min(10, Number(difficulty) || 0));
    return '●'.repeat(d) + '○'.repeat(10 - d);
  }

  function reward(m) {
    return m.ore_reward > 0
      ? fill(COPY.missionReward, { credits: m.credit_reward, ore: m.ore_reward })
      : fill(COPY.missionRewardNoOre, { credits: m.credit_reward });
  }

  function showMissionList() {
    const list = $('missions-list');
    list.replaceChildren();
    for (const id of ORDER) {
      const m = missions && missions[id];
      if (!m) continue; // a contract the server no longer offers is skipped
      const row = el('button', 'mission-row');
      row.type = 'button';
      const diff = el('span', 'mission-diff');
      diff.append(el('span', 'mission-diff-label', COPY.missionDifficulty), el('span', 'mission-pips', pips(m.difficulty)));
      row.append(
        el('span', 'mission-name', m.name),
        diff,
        el('span', 'mission-meta', `${m.environment} · ${reward(m)}`),
      );
      row.addEventListener('click', () => showBriefing(m));
      const item = el('li', null);
      item.append(row);
      list.append(item);
    }
    deps.show('screen-missions');
  }

  // ── Briefing ─────────────────────────────────────────────────────────────
  function deployed() {
    const camp = store.campaign();
    const roster = deps.getRoster();
    if (!camp || !roster) return null;
    const commander = (roster.commanders || []).find(c => c.commander_id === camp.commander_id);
    const section = (roster.sections || [])[0];
    return commander && section ? { camp, roster, commander, section } : null;
  }

  function showBriefing(m) {
    mission = m;
    const d = deployed();
    if (!d) return deps.enterCampaign(0);
    still($('briefing-still'), ART.briefing, false);
    $('briefing-name').textContent = m.name;
    $('briefing-text').textContent = m.flavor_text;
    $('briefing-commander').textContent = fill(COPY.briefingCommander, { name: d.commander.name, stress: d.commander.stress });
    $('btn-deploy').disabled = false;
    deps.show('screen-briefing');
  }

  // ── Deploy ───────────────────────────────────────────────────────────────
  function squad() {
    const def = units && units[STAND_IN];
    if (!def) return null;
    const n = oneRookie && oneRookie.checked ? 1 : SQUAD_SIZE; // debug toggle only
    return Array.from({ length: n }, (_, i) => ({
      id: `r${i + 1}`,
      name: fill(COPY.unitName, { n: i + 1 }),
      definition: def,
      skill: def.base_skill,
      xp: 0,
      current_hp: 10,
      max_hp: 10,
      status: 'Ready',
      equipment: [],
    }));
  }

  async function deploy() {
    if (busy || !mission) return;
    const d = deployed();
    const rookies = squad();
    if (!d || !rookies) return deps.showError(deps.genericText, () => deps.enterCampaign(0));
    busy = true;
    $('btn-deploy').disabled = true;
    $('working-text').textContent = COPY.battleWorking;
    deps.show('screen-working');
    counters.mission += 1;
    const body = {
      squad: { units: rookies },
      mission_id: mission.id,
      commander_id: d.commander.commander_id,
      section_ids: [d.section.section_id],
    };
    let res;
    try {
      res = await postMission(body);
    } catch (e) {
      busy = false;
      return missionError(e, d);
    }
    busy = false;
    result = res;
    resultFor = d.commander.commander_id;
    patchRoster(d, res.commander); // at once: the server has already applied it
    present();
  }

  // The mission's own error map: nothing here ever sends the mission again.
  function missionError(e, d) {
    deps.note(e);
    if (!(e instanceof ApiError)) return deps.showError(COPY.missionLost, deps.reloadRoster);
    const back = () => showBriefing(mission);
    switch (e.kind) {
      case 'offline':
      case 'server':
        return deps.showError(COPY.missionLost, deps.reloadRoster); // one roster fetch, never a re-POST
      case 'rate':
        return deps.showError(fill(COPY.missionSlowDown, { n: e.retryAfter }), back);
      case 'unauth':
        return deps.handleError(e, null);
      case 'conflict':
        if (e.message === 'Commander is KIA') return deathFromRefusal(d, { is_kia: true });
        if (/Shattered/.test(e.message)) return deathFromRefusal(d, { is_shattered: true });
        if (/already deployed/.test(e.message)) return deps.showError(COPY.missionBusy, back);
        if (e.message === 'campaign is not active') return deps.staleCampaign(true);
        return deps.showError(COPY.missionRefused, back);
      case 'notfound':
        return deps.staleCampaign(); // the stored Commander or Section is not this wallet's
      default:
        return deps.showError(COPY.missionRefused, back);
    }
  }

  // The server refused the deploy because the Commander is already dead or Shattered.
  function deathFromRefusal(d, flags) {
    Object.assign(d.commander, flags);
    store.setRoster(d.camp.campaign_id, d.roster);
    showEndOfRun(d.commander.commander_id);
  }

  // The roster patch (spec check §7): the deployed Commander only; never `sections`.
  function patchRoster(d, c) {
    const cmd = d.commander;
    cmd.stress = c.stress;
    cmd.xp = c.xp;
    cmd.rank = c.new_rank;
    cmd.is_kia = c.kia;
    cmd.is_shattered = cmd.is_shattered || c.stress >= 100;
    store.setRoster(d.camp.campaign_id, d.roster);
    deps.setDebugExtra(`stress=${c.stress} xp=${c.xp}`);
  }

  // ── Presentation ─────────────────────────────────────────────────────────
  // Shows `node` through its CSS transition, then runs `next` on its transitionend. With no
  // transition to wait for (reduced motion sets none), `next` runs at once.
  function revealThen(node, next, id) {
    node.getBoundingClientRect(); // style is computed before the class lands, so the transition runs
    if (!(parseFloat(getComputedStyle(node).transitionDuration) > 0)) {
      node.classList.add('shown');
      return next();
    }
    node.addEventListener('transitionend', function done(ev) {
      if (ev.target !== node || ev.propertyName !== 'opacity') return;
      node.removeEventListener('transitionend', done);
      if (id === run) next();
    });
    node.classList.add('shown');
  }

  function countUp(valueNode, target, next, id) {
    const ms = 450;
    let start = null;
    const step = now => {
      if (id !== run) return;
      if (start === null) start = now;
      const t = Math.min(1, (now - start) / ms);
      valueNode.textContent = String(Math.round(target * t));
      if (t < 1) requestAnimationFrame(step);
      else next();
    };
    requestAnimationFrame(step);
  }

  function present() {
    const id = ++run;
    const sb = result.report.score_breakdown;
    still($('battle-still'), ART.battle, false);
    music(MUSIC.battle);
    const lines = $('battle-lines');
    lines.replaceChildren();
    const rows = SCORE_LINES.map(([key, field]) => {
      const row = el('li', 'score-row');
      const value = el('span', 'score-value', '0');
      row.append(el('span', 'score-label', COPY.battleScore[key]), value);
      lines.append(row);
      return { row, value, target: Number(sb[field]) || 0 };
    });
    const chance = $('battle-chance');
    const roll = $('battle-roll');
    const banner = $('battle-outcome');
    for (const n of [chance, roll, banner]) { n.classList.remove('shown'); n.textContent = ''; }
    banner.classList.remove(...[...banner.classList].filter(c => c.startsWith('outcome-')));
    $('btn-skip').textContent = COPY.battleSkip;
    deps.show('screen-battle');

    const steps = [];
    rows.forEach(r => steps.push(next => revealThen(r.row, () => countUp(r.value, r.target, next, id), id)));
    steps.push(next => {
      chance.textContent = fill(COPY.battleChance, { n: sb.raw_roll + sb.margin });
      revealThen(chance, next, id);
    });
    steps.push(next => {
      roll.textContent = fill(COPY.battleRoll, { n: sb.raw_roll });
      revealThen(roll, next, id);
    });
    steps.push(next => {
      const outcome = result.report.outcome;
      banner.textContent = COPY.outcome[outcome] || outcome;
      banner.classList.add(`outcome-${outcome}`);
      revealThen(banner, next, id);
    });
    const go = i => {
      if (id === run && i < steps.length) steps[i](() => go(i + 1));
      else if (id === run) $('btn-skip').textContent = COPY.battleContinue; // the last step has run
    };
    // The first reveal waits one frame so the screen is laid out before the transition starts.
    requestAnimationFrame(() => go(0));
  }

  // ── After-action report ──────────────────────────────────────────────────
  function statusLine(u) {
    if (u.final_status === 'KIA') return COPY.unitStatus.KIA;
    if (u.final_status === 'Wounded') return fill(COPY.unitStatus.Wounded, { n: u.hp_lost });
    if (u.final_status === 'Ready') return COPY.unitStatus.Ready;
    return u.final_status; // MIA is never emitted today
  }

  function showAar() {
    run += 1; // stops the presentation, whichever step it is on
    const { report, commander: c } = result;
    const outcome = report.outcome;
    still($('aar-still'), ART[outcome] || ART.Defeat, outcome === 'Wipeout');
    music(MUSIC[outcome] || MUSIC.Defeat, false);
    const banner = $('aar-outcome');
    banner.className = `aar-outcome outcome-${outcome}`;
    banner.textContent = COPY.outcome[outcome] || outcome;

    const list = $('aar-units');
    list.replaceChildren();
    for (const u of report.unit_results || []) {
      const row = el('li', `unit-row unit-${String(u.final_status).toLowerCase()}`);
      row.append(el('span', 'unit-glyph', GLYPH), el('span', 'unit-name', u.unit_name), el('span', 'unit-status', statusLine(u)));
      list.append(row);
    }

    const pay = $('aar-pay');
    pay.replaceChildren();
    const r = report.rewards || {};
    if (r.credits > 0 || r.ore > 0) {
      pay.append(el('p', 'pay-line', r.ore > 0
        ? fill(COPY.missionReward, { credits: r.credits, ore: r.ore })
        : fill(COPY.missionRewardNoOre, { credits: r.credits })));
    }
    if (r.loot_drop) pay.append(el('p', 'pay-line', fill(COPY.aarLoot, { item: r.loot_drop.item_name })));
    if (!pay.childElementCount) pay.append(el('p', 'pay-line pay-none', COPY.aarNoRewards));

    const card = $('aar-commander');
    card.replaceChildren(
      el('span', 'aar-commander-name', report.commander_name),
      el('span', 'aar-stat', fill(COPY.aarStress, { n: c.stress, d: c.stress_change })),
      el('span', 'aar-stat', fill(COPY.aarXp, { n: c.xp_gained })),
    );
    if (c.new_rank > c.old_rank) card.append(el('span', 'aar-stat aar-rankup', fill(COPY.aarRankUp, { a: c.old_rank, b: c.new_rank })));
    deps.show('screen-aar');
  }

  function afterAar() {
    const d = deployed();
    if (result.commander.kia || (d && d.commander.is_shattered)) {
      return showEndOfRun(d ? d.commander.commander_id : store.campaign()?.commander_id);
    }
    const camp = store.campaign();
    if (camp && !store.offered(camp.campaign_id)) { // the checkout, once per campaign (BP-3c-2b)
      music(MUSIC.hub);
      return beforeOffer({ commander: result.report.commander_name }, () => deps.offerCheckout(() => deps.enterHub()));
    }
    music(MUSIC.hub);
    deps.enterHub();
  }

  // ── End of run ───────────────────────────────────────────────────────────
  // Built from the kept roster; never refetched once the run has ended.
  function showEndOfRun(commanderId) {
    run += 1;
    const roster = deps.getRoster();
    const c = roster && (roster.commanders || []).find(x => x.commander_id === commanderId);
    if (!c) return deps.showError(deps.genericText, () => deps.enterCampaign(0));
    const shattered = !c.is_kia && c.is_shattered;
    $('end-title').textContent = shattered ? COPY.endShattered : COPY.endTitle;
    $('end-name').textContent = c.name;
    $('end-grade').textContent = c.commander_grade ?? '';
    $('end-flaw').textContent = c.fatal_flaw;
    $('end-body').textContent = fill(shattered ? COPY.endBodyShattered : COPY.endBodyKia, { name: c.name });
    const glorious = Boolean(result && resultFor === c.commander_id && result.commander.glorious && c.is_kia);
    $('end-glorious').textContent = glorious ? COPY.endGlorious : '';
    $('end-glorious').hidden = !glorious;
    $('btn-new-run').disabled = false;
    deps.show('screen-end');
  }

  // New run: the checkout first (BP-3c-2b), then retire this campaign and join a fresh one (its
  // reveal follows the join).
  function newRun() {
    if (busy) return;
    deps.offerCheckout(abandonAndRejoin);
  }

  async function abandonAndRejoin() {
    if (busy) return;
    const camp = store.campaign();
    if (!camp) return deps.startFresh();
    busy = true;
    $('btn-new-run').disabled = true;
    $('working-text').textContent = COPY.newRunWorking;
    deps.show('screen-working');
    try {
      await postAbandon(camp.campaign_id);
    } catch (e) {
      busy = false;
      // Already retired, gone (404) or not this wallet's (403): a fresh join, never a Retry loop.
      const over = e instanceof ApiError
        && ((e.kind === 'conflict' && e.message === 'campaign is not active') || e.kind === 'notfound' || e.kind === 'forbidden');
      if (!over) return deps.handleError(e, abandonAndRejoin);
    }
    busy = false;
    result = null;
    resultFor = null;
    music(MUSIC.hub);
    deps.startFresh();
  }

  fillCopy();
  wire();
  return { openMissions, showEndOfRun };
}

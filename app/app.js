'use strict';

// ── Auth state ────────────────────────────────────────────────────────────────
let pubkey = null;
let token  = null;

// ── Utilities ─────────────────────────────────────────────────────────────────
function isMobile() {
  return /Android|iPhone/i.test(navigator.userAgent);
}

function b58enc(bytes) {
  const ALPHA = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
  const d = [0];
  for (const b of bytes) {
    let c = b;
    for (let i = 0; i < d.length; i++) { c += d[i] << 8; d[i] = c % 58; c = (c / 58) | 0; }
    while (c > 0) { d.push(c % 58); c = (c / 58) | 0; }
  }
  let s = '';
  for (let i = 0; i < bytes.length && bytes[i] === 0; i++) s += '1';
  for (let i = d.length - 1; i >= 0; i--) s += ALPHA[d[i]];
  return s;
}

function qs(id) { return document.getElementById(id); }

function showOnly(id) {
  document.querySelectorAll('.screen, .battle-screen').forEach(el => {
    el.classList.toggle('hidden', el.id !== id);
  });
}

function setStatus(id, msg) { qs(id).textContent = msg; }

async function apiPost(url, body) {
  const headers = { 'Content-Type': 'application/json' };
  if (token) headers['Authorization'] = 'Bearer ' + token;
  const r = await fetch(url, { method: 'POST', headers, body: JSON.stringify(body) });
  if (!r.ok) { const t = await r.text(); throw new Error(t); }
  return r.json();
}

function delay(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

// ── Audio management ──────────────────────────────────────────────────────────
const music = {
  preBattle:  new Audio('assets/music/pre_battle.mp3'),
  battleLoop: new Audio('assets/music/battle_loop.mp3'),
  victory:    new Audio('assets/music/victory.mp3'),
  defeat:     new Audio('assets/music/defeat.mp3'),
};
music.battleLoop.loop = true;

let activeMusic = null;

function playMusic(track) {
  if (activeMusic && activeMusic !== track) {
    activeMusic.pause();
    activeMusic.currentTime = 0;
  }
  activeMusic = track;
  track.currentTime = 0;
  track.play().catch(() => {});
}

function stopMusic() {
  if (activeMusic) {
    activeMusic.pause();
    activeMusic.currentTime = 0;
    activeMusic = null;
  }
}

function playSfx(name) {
  const src = name === 'gunfire'   ? 'assets/sfx/gunfire.wav'
            : name === 'explosion' ? 'assets/sfx/explosion.wav'
                                   : 'assets/sfx/scatter.wav';
  const a = new Audio(src);
  a.play().catch(() => {});
}

// ── Scenario constants (mirrors scenario.rs static data) ─────────────────────
const PACK_NAMES  = ['Mod Squad', 'Rocker Boyz', 'Goth Collective', 'Punk Agenda'];
const PACK_MAX    = [14, 13, 12, 15];
const SEC_NAMES   = ['Section 1 "Scrap Dogs"', 'Section 2 "Wage Slaves"'];
const SEC_SHORT   = ['Scrap Dogs', 'Wage Slaves'];
const SEC_MAX     = [8, 8];
const VEH_SHORT   = ['Dustbreaker', 'Gadfly', 'Iron Coffin', 'Paid in Full'];
const VEH_MAX_HP  = [800, 350, 600, 600];

// ── Battle state (reset each run) ────────────────────────────────────────────
let bs = null;  // battle state object
let retreated = false;

function resetBattleState() {
  bs = {
    vehHp:         [...VEH_MAX_HP],
    secStr:        [...SEC_MAX],
    packStr:       [...PACK_MAX],
    packScattered: [false, false, false, false],
    packDestroyed: [false, false, false, false],
  };
  retreated = false;
}

// ── Login screen ──────────────────────────────────────────────────────────────
qs('btn-connect').addEventListener('click', async () => {
  if (!window.solana?.isPhantom) {
    if (isMobile()) {
      setStatus('login-status', 'On mobile? Open this page in the Phantom app browser.');
      qs('btn-phantom-browser').classList.remove('hidden');
    } else {
      setStatus('login-status', 'Phantom not detected. Install the Phantom browser extension.');
    }
    return;
  }
  try {
    const resp = await window.solana.connect();
    pubkey = resp.publicKey.toBase58();
    setStatus('login-status', 'Wallet connected: ' + pubkey.slice(0, 8) + '…');
    qs('btn-sign').disabled = false;
  } catch {
    setStatus('login-status', 'Wallet connection rejected.');
  }
});

qs('btn-sign').addEventListener('click', async () => {
  setStatus('login-status', 'Requesting challenge…');
  try {
    const { challenge } = await apiPost('/api/auth/challenge', { wallet_address: pubkey });
    setStatus('login-status', 'Sign the message in Phantom…');
    const { signature } = await window.solana.signMessage(
      new TextEncoder().encode(challenge), 'utf8'
    );
    setStatus('login-status', 'Verifying…');
    const result = await apiPost('/api/auth/verify', {
      wallet_address: pubkey, challenge, signature: b58enc(signature),
    });
    token = result.token_id;
    sessionStorage.setItem('token', token);
    showOnly('screen-hub');
  } catch (e) {
    setStatus('login-status', 'Auth failed: ' + e.message);
  }
});

// ── Command Hub ───────────────────────────────────────────────────────────────
qs('btn-ore-run').addEventListener('click', () => {
  document.querySelectorAll('.btn-approach').forEach(b => { b.disabled = false; });
  showOnly('screen-pre-battle');
  playMusic(music.preBattle);
});

// ── Screen 1: Approach selection ──────────────────────────────────────────────
document.querySelectorAll('.btn-approach').forEach(btn => {
  btn.addEventListener('click', async () => {
    document.querySelectorAll('.btn-approach').forEach(b => { b.disabled = true; });
    const approach = btn.dataset.approach;
    try {
      const { result, ticks } = await apiPost('/api/ore-run/start', { approach });
      stopMusic();
      await startBattleTicker(result, ticks);
    } catch (e) {
      document.querySelectorAll('.btn-approach').forEach(b => { b.disabled = false; });
      alert('Engagement failed: ' + e.message);
    }
  });
});

// ── Screen 2: Battle Ticker ───────────────────────────────────────────────────
function buildStatusStrip() {
  const strip = qs('bt-status');
  strip.innerHTML = '';

  const mkSep = text => {
    const s = document.createElement('span');
    s.className = 'bt-sep';
    s.textContent = text;
    strip.appendChild(s);
  };

  mkSep('DEFENDERS:');

  for (let i = 0; i < VEH_SHORT.length; i++) {
    const su = document.createElement('div');
    su.className = 'su';
    su.innerHTML =
      `<span class="su-name active" id="sv-n${i}">${VEH_SHORT[i]}</span>` +
      `<span class="su-bar"><span class="su-fill" id="sv-f${i}" style="width:100%"></span></span>` +
      `<span class="su-val" id="sv-v${i}">${VEH_MAX_HP[i]}/${VEH_MAX_HP[i]}</span>`;
    strip.appendChild(su);
  }

  for (let i = 0; i < SEC_SHORT.length; i++) {
    const su = document.createElement('div');
    su.className = 'su';
    su.innerHTML =
      `<span class="su-name active" id="ss-n${i}">${SEC_SHORT[i]}</span>` +
      `<span class="su-val" id="ss-v${i}">${SEC_MAX[i]}/${SEC_MAX[i]}</span>`;
    strip.appendChild(su);
  }

  mkSep('| ATTACKERS:');

  for (let i = 0; i < PACK_NAMES.length; i++) {
    const su = document.createElement('div');
    su.className = 'su';
    su.innerHTML =
      `<span class="su-name active" id="sp-n${i}">${PACK_NAMES[i]}</span>` +
      `<span class="su-val" id="sp-v${i}">${PACK_MAX[i]}/${PACK_MAX[i]}</span>`;
    strip.appendChild(su);
  }
}

function updateStatusStrip(tickLog) {
  // Warthog HP (only vehicle that can take damage in this scenario)
  const wHp = tickLog.warthog_hp_after;
  bs.vehHp[0] = wHp;
  const pct = Math.max(0, wHp) / VEH_MAX_HP[0] * 100;
  const fill = qs('sv-f0');
  fill.style.width = pct + '%';
  fill.className = 'su-fill' + (pct < 30 ? ' red' : pct < 60 ? ' amber' : '');
  qs('sv-v0').textContent = Math.max(0, wHp) + '/' + VEH_MAX_HP[0];

  // Section strengths
  (tickLog.section_strengths || []).forEach((str, i) => {
    bs.secStr[i] = str;
    qs(`ss-v${i}`).textContent = str + '/' + SEC_MAX[i];
    if (str === 0) qs(`ss-n${i}`).className = 'su-name destroyed';
  });

  // Pack scatter events this tick
  (tickLog.scatter_events || []).forEach(se => {
    const i = se.pack_index;
    bs.packScattered[i] = true;
    qs(`sp-n${i}`).className = 'su-name routed';
    qs(`sp-v${i}`).className = 'su-val routed';
  });

  // Pack strengths
  (tickLog.pack_strengths || []).forEach((str, i) => {
    bs.packStr[i] = str;
    if (bs.packScattered[i]) {
      qs(`sp-v${i}`).textContent = 'ROUTED';
    } else if (str === 0) {
      bs.packDestroyed[i] = true;
      qs(`sp-n${i}`).className = 'su-name destroyed';
      qs(`sp-v${i}`).textContent = 'DESTROYED';
    } else {
      qs(`sp-v${i}`).textContent = str + '/' + PACK_MAX[i];
    }
  });
}

// Ticker line helpers
function addLine(text, cls) {
  const panel = qs('ticker-panel');
  const el = document.createElement('div');
  el.className = cls;
  el.textContent = text;
  panel.appendChild(el);
  panel.scrollTop = panel.scrollHeight;
}

function mechLine(tick, unit, weapon, result, target, detail) {
  const parts = [unit, weapon, result, target];
  if (detail) parts.push(detail);
  addLine(`[TICK ${tick}] ${parts.join(' — ')}`, 't-mech');
}

function flavLine(text) {
  addLine(`"${text}"`, 't-flav');
}

async function processTickEvents(tickLog) {
  const t = tickLog.tick;

  // ── Scrap-Rocket (fires on Tick 2, always shown) ──
  if (tickLog.scrap_rocket) {
    const sr = tickLog.scrap_rocket;
    if (sr.misfired) {
      const kia = (sr.misfire_carrier_killed ? 1 : 0) + (sr.misfire_additional_casualties || 0);
      mechLine(t, 'Punk Agenda', 'Scrap-Rocket', 'MISFIRE', 'self', kia + ' KIA (self-inflicted)');
      playSfx('explosion'); // fires BEFORE the flavour line per spec
      await delay(90);
      flavLine(sr.flavour);
    } else if (sr.hit) {
      mechLine(t, 'Punk Agenda', 'Scrap-Rocket', 'HIT',
        'Warthog "Dustbreaker"', sr.damage_dealt + ' dmg');
      playSfx('explosion');
      flavLine(sr.flavour);
    } else {
      mechLine(t, 'Punk Agenda', 'Scrap-Rocket', 'MISS', 'Warthog "Dustbreaker"', '');
    }
  }

  // ── Vehicle fire (hits with kills only) ──
  for (const vf of (tickLog.vehicle_fire || [])) {
    if (!vf.is_hit || !vf.is_penetration || vf.kills === 0) continue;
    const pack = PACK_NAMES[vf.target_pack];
    if (vf.weapon_name === 'Thumper GL') {
      mechLine(t, vf.vehicle_name, 'Thumper GL', 'HIT',
        `Pack "${pack}"`, `AoE — ${vf.kills} KIA`);
      playSfx('explosion');
    } else {
      mechLine(t, vf.vehicle_name, vf.weapon_name, 'HIT',
        `Pack "${pack}"`, vf.kills + ' KIA');
      playSfx('gunfire');
    }
    if (vf.flavour) flavLine(vf.flavour);
  }

  // ── Section fire (kills only) ──
  for (const sf of (tickLog.section_fire || [])) {
    if (sf.kills === 0) continue;
    const pack = PACK_NAMES[sf.target_pack];
    mechLine(t, sf.section_name, 'Scavenged Slugger', 'HIT',
      `Pack "${pack}"`, sf.kills + ' KIA');
    playSfx('gunfire');
    if (sf.flavour) flavLine(sf.flavour);
  }

  // ── Pack fire at sections (kills only, no SFX) ──
  for (const pf of (tickLog.pack_fire || [])) {
    if (pf.kills === 0) continue;
    mechLine(t, pf.pack_name, 'pistols/Uzis', 'HIT',
      SEC_NAMES[pf.target_section], pf.kills + ' KIA');
    if (pf.flavour) flavLine(pf.flavour);
  }

  // ── Pack scatter events ──
  for (const se of (tickLog.scatter_events || [])) {
    mechLine(t, `Pack "${se.pack_name}"`, '—', 'ROUTED',
      '—', se.strength_at_scatter + ' remaining, scattering');
    playSfx('scatter');
    flavLine(se.flavour);
  }
}

async function runTickAnimation(ticks) {
  for (const tickLog of ticks) {
    if (retreated) break;
    await processTickEvents(tickLog);
    updateStatusStrip(tickLog);
    if (retreated) break; // retreated during tick processing
    await delay(1500);
  }

  if (!retreated) {
    await showConcludeButton();
  } else {
    await delay(1500);
  }
}

function showConcludeButton() {
  return new Promise(resolve => {
    const btn = qs('btn-conclude');
    const retreatBtn = qs('btn-retreat');
    retreatBtn.disabled = true;
    btn.classList.add('visible');
    btn.onclick = () => {
      btn.classList.remove('visible');
      btn.onclick = null;
      stopMusic();
      resolve();
    };
  });
}

async function startBattleTicker(result, ticks) {
  resetBattleState();
  buildStatusStrip();
  qs('ticker-panel').innerHTML = '';

  const concludeBtn = qs('btn-conclude');
  concludeBtn.classList.remove('visible');
  concludeBtn.onclick = null;

  showOnly('screen-battle-ticker');
  playMusic(music.battleLoop);

  const retreatBtn = qs('btn-retreat');
  retreatBtn.disabled = false;
  retreatBtn.onclick = () => {
    if (retreated) return;
    retreated = true;
    retreatBtn.disabled = true;
    const panel = qs('ticker-panel');
    const el = document.createElement('div');
    el.className = 't-system';
    el.textContent = '— RETREAT CONFIRMED —';
    panel.appendChild(el);
    panel.scrollTop = panel.scrollHeight;
  };

  await runTickAnimation(ticks);

  if (retreated) stopMusic();
  showPostBattle(result, retreated);
}

// ── Screen 3: Post-Battle ─────────────────────────────────────────────────────
function showPostBattle(result, isRetreat) {
  const isWin = !isRetreat && result.outcome === 'Win';

  qs('post-art').src = isWin
    ? 'assets/art/victory_player.png'
    : 'assets/art/victory_raccoon.png';

  playMusic(isWin ? music.victory : music.defeat);

  const panel = qs('post-panel');
  panel.innerHTML = '';

  // Outcome title
  const titleEl = document.createElement('div');
  titleEl.className = 'post-title' + (isWin ? '' : ' defeat');
  titleEl.textContent = isWin ? '▶  VICTORY' : '▶  DEFEAT';
  panel.appendChild(titleEl);

  // Flavour text
  const flavEl = document.createElement('div');
  flavEl.className = 'post-flavour';

  if (isWin) {
    let text =
      `AFTER-ACTION REPORT — All ore secured. Four raccoon packs engaged; ` +
      `${result.packs_routed} routed, ${result.packs_destroyed} destroyed at point-blank ` +
      `range of its own Scrap-Rocket. ${result.defender_kia} personnel KIA.`;

    if (result.misfire_occurred) {
      text += `\n\nNotably: Pack ‘Punk Agenda’s’ Scrap-Rocket misfired. At least I assume that was a misfire, and not the intended effect. Either way, that did a lot of the work for us!`;
    }

    text +=
      `\n\nCommander reports the convoy is proceeding to base and requests someone prepare ` +
      `a bottle of something strong ahead of arrival. The boombox is also coming. ` +
      `Frederick seems to be a fan, and is singing along badly to the Ace of Spades.`;

    flavEl.textContent = text;
  } else {
    flavEl.textContent =
      `AFTER-ACTION REPORT — Convoy lost. Ore in the possession of the Raccoon Biker Gangs. ` +
      `${result.defender_kia} personnel KIA. ${result.defender_kia} MIA, presumed in raccoon ` +
      `custody — fate: Let’s say, ambiguous. They’re probably fine…\n\n` +
      `The raccoons have been observed celebrating. The music is extremely loud. ` +
      `I think they may have deafened themselves with their rockets.\n\n` +
      `Finance has been notified regarding the loss. Suffice to say, it didn’t go down well.`;
  }
  panel.appendChild(flavEl);

  // Stats block
  panel.appendChild(mkHr());

  const statsHead = document.createElement('div');
  statsHead.className = 'post-section-head';
  statsHead.textContent = 'ENGAGEMENT SUMMARY';
  panel.appendChild(statsHead);

  const grid = document.createElement('div');
  grid.className = 'post-stats-grid';
  [
    ['Defender KIA',      result.defender_kia],
    ['Attacker KIA',      result.attacker_kia],
    ['Packs Routed',      result.packs_routed],
    ['Engagement Length', result.ticks_elapsed + ' ticks'],
  ].forEach(([label, val]) => {
    const el = document.createElement('div');
    el.className = 'post-stat';
    el.innerHTML = `${label}: <b>${val}</b>`;
    grid.appendChild(el);
  });
  panel.appendChild(grid);

  // Highlights block
  if (result.highlights && result.highlights.length > 0) {
    panel.appendChild(mkHr());

    const hlHead = document.createElement('div');
    hlHead.className = 'post-section-head';
    hlHead.textContent = 'FIELD HIGHLIGHTS';
    panel.appendChild(hlHead);

    result.highlights.forEach(h => {
      const el = document.createElement('div');
      el.className = 'post-highlight';
      el.textContent = `Tick ${h.tick}: "${h.flavour}"`;
      panel.appendChild(el);
    });
  }

  // Return to base
  const footer = document.createElement('div');
  footer.className = 'post-footer';
  const rtbBtn = document.createElement('button');
  rtbBtn.className = 'btn-rtb';
  rtbBtn.textContent = 'RETURN TO BASE';
  rtbBtn.addEventListener('click', () => {
    stopMusic();
    showOnly('screen-hub');
  });
  footer.appendChild(rtbBtn);
  panel.appendChild(footer);

  showOnly('screen-post-battle');
}

function mkHr() {
  const hr = document.createElement('hr');
  hr.className = 'post-hr';
  return hr;
}

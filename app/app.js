'use strict';

let pubkey = null;
let token  = null;
let sessionId = null;

function isMobile() {
  return /Android|iPhone/i.test(navigator.userAgent);
}

// Compact base58 encoder — needed to submit Phantom's Uint8Array signature to the Rust backend.
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

function show(id) { document.getElementById(id).classList.remove('hidden'); }
function hide(id) { document.getElementById(id).classList.add('hidden'); }
function setStatus(id, msg) { document.getElementById(id).textContent = msg; }

async function apiPost(url, body) {
  const headers = { 'Content-Type': 'application/json' };
  if (token) headers['Authorization'] = 'Bearer ' + token;
  const r = await fetch(url, { method: 'POST', headers, body: JSON.stringify(body) });
  if (!r.ok) { const t = await r.text(); throw new Error(t); }
  return r.json();
}

// ── Screen 1: wallet connect ──────────────────────────────────────────────

document.getElementById('btn-connect').addEventListener('click', async () => {
  if (!window.solana?.isPhantom) {
    if (isMobile()) {
      setStatus('login-status', 'On mobile? Open this page in the Phantom app browser for the best experience.');
      document.getElementById('btn-phantom-browser').classList.remove('hidden');
    } else {
      setStatus('login-status', 'Phantom not detected. Install the Phantom browser extension.');
    }
    return;
  }
  try {
    const resp = await window.solana.connect();
    pubkey = resp.publicKey.toBase58();
    setStatus('login-status', 'Wallet connected: ' + pubkey.slice(0, 8) + '…');
    document.getElementById('btn-sign').disabled = false;
  } catch {
    setStatus('login-status', 'Wallet connection rejected.');
  }
});

// ── Screen 1: sign challenge and authenticate ─────────────────────────────

document.getElementById('btn-sign').addEventListener('click', async () => {
  setStatus('login-status', 'Requesting challenge…');
  try {
    const { challenge } = await apiPost('/api/auth/challenge', { wallet_address: pubkey });
    setStatus('login-status', 'Sign the message in Phantom…');
    const { signature } = await window.solana.signMessage(new TextEncoder().encode(challenge), 'utf8');
    setStatus('login-status', 'Verifying…');
    const result = await apiPost('/api/auth/verify', {
      wallet_address: pubkey,
      challenge,
      signature: b58enc(signature),
    });
    token = result.token_id;
    sessionStorage.setItem('token', token);
    hide('screen-login');
    show('screen-dashboard');
  } catch (e) {
    setStatus('login-status', 'Auth failed: ' + e.message);
  }
});

// ── Screen 2: start streaming combat session ──────────────────────────────

const DEMO_COMBAT = {
  section: {
    id: 'alpha-1', name: 'Alpha Section',
    max_strength: 4, current_strength: 4, individual_hp: 10,
    accuracy: 55, evasion: 10,
    weapon: { name: 'AT Launcher', ap: 6, base_damage: 20, tag: 'Missile', accuracy: 0 },
    armor_at: 0, armor_tag: 'Unarmored',
  },
  vehicle: {
    id: 'scout-1', name: 'Armored Scout',
    hp: 50, max_hp: 50, at: 4, armor_tag: 'LightArmor', evasion: 8,
    weapons: [{ name: 'Autocannon', ap: 3, base_damage: 14, tag: 'Slug', accuracy: 15 }],
  },
  max_ticks: 25,
  seed_override: 42,
  commander: {
    id: 'commander-vane-001',
    name: 'Colonel Vane',
    species: 'Human (Corporate)',
    rank: 5,
    skill: 9,
    success_aura: 15,
    quality_grade: 'Superior',
    ability: 'Precision Cadence — Section accuracy rolls made under his command are treated as if firing from prepared positions.',
    flavor_text: 'Has outlived seventeen engagements, four court martials, and one strongly-worded HR memorandum. The HR memorandum was the most dangerous of the three.',
    stress_level: 0,
    is_kia: false,
    is_shattered: false,
    can_retreat: true,
    passive_buffs: { accuracy: 10, evasion: 5, damage_reduction: 3 },
    attached_unit_id: null,
  },
};

document.getElementById('btn-start-combat').addEventListener('click', async () => {
  const feed = document.getElementById('tick-feed');
  feed.innerHTML = '';
  document.getElementById('btn-view-aar').disabled = true;

  const addLine = (text, cls) => {
    const p = document.createElement('p');
    p.textContent = text;
    if (cls) p.classList.add(cls);
    feed.appendChild(p);
    feed.scrollTop = feed.scrollHeight;
  };

  const addTick = (text, cls) => {
    const pre = document.createElement('pre');
    pre.textContent = text;
    if (cls) pre.classList.add(cls);
    feed.appendChild(pre);
    feed.scrollTop = feed.scrollHeight;
  };

  try {
    const { session_id } = await apiPost('/api/combat/stream/start', DEMO_COMBAT);
    sessionId = session_id;
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const ws = new WebSocket(`${proto}//${location.host}/api/combat/stream/${session_id}`);

    ws.onmessage = ({ data }) => {
      const ev = JSON.parse(data);
      const text = ev.narrative || `[Tick ${ev.tick_index}]`;
      addTick(text, ev.combat_ended ? 'outcome' : null);
    };
    ws.onclose = () => { document.getElementById('btn-view-aar').disabled = false; };
    ws.onerror = () => addLine('WebSocket error — check server logs.');
  } catch (e) {
    addLine('Failed to start session: ' + e.message);
  }
});

function formatAar(wrapper) {
  const r   = wrapper.report;
  const SEP = '╠══════════════════════════════════════╣';
  const END = '╚══════════════════════════════════════╝';

  function sBar(current, max) {
    return '◆'.repeat(Math.max(0, current)) +
           '◇'.repeat(Math.max(0, max - current));
  }
  function hBar(current, max) {
    const W = 10;
    const filled = Math.round(Math.max(0, current) / Math.max(1, max) * W);
    return '█'.repeat(filled) + '░'.repeat(W - filled);
  }

  const lines = [
    '╔══════════════════════════════════════╗',
    '║  AFTER-ACTION REPORT',
    `║  Session : ${wrapper.session_id}`,
    `║  Seed    : ${wrapper.seed}`,
    `║  Build   : ${wrapper.build_version}`,
    SEP,
  ];

  for (const tick of r.ticks) {
    lines.push(`║  TICK ${tick.tick}`);
    lines.push('║');
    lines.push(`║  [ VEHICLE FIRES — ${r.vehicle_name} ]`);

    for (const we of tick.vehicle_events) {
      if (!we.is_hit) {
        lines.push(`║    ${we.weapon_name}  MISS  [${we.hit_roll_breakdown}]`);
      } else if (!we.is_penetration) {
        lines.push(`║    ${we.weapon_name}  HIT  [${we.hit_roll_breakdown}]`);
        lines.push(`║      ${we.ap_vs_at} — NO PENETRATION`);
      } else {
        lines.push(`║    ${we.weapon_name}  HIT  [${we.hit_roll_breakdown}]`);
        lines.push(`║      ${we.ap_vs_at} — PENETRATION  dmg: ${we.final_damage}  → ${we.kill_count} KIA`);
      }
    }

    lines.push('║');
    if (tick.defender_suppressed) {
      lines.push(`║  [ ${r.section_name} SUPPRESSED — cannot return fire ]`);
    } else if (tick.section_event) {
      const se = tick.section_event;
      lines.push(`║  [ SECTION FIRES — ${r.section_name} ]`);
      if (se.hits_total > 0 && se.is_penetration) {
        lines.push(`║    ${se.shots_total} shots → ${se.hits_total} hits → ${se.total_damage} dmg`);
      } else if (se.hits_total > 0) {
        lines.push(`║    ${se.shots_total} shots → ${se.hits_total} hits → NO PENETRATION`);
      } else {
        lines.push(`║    ${se.shots_total} shots → 0 hits`);
      }
    }

    lines.push('║');
    lines.push(
      `║  End Tick ${tick.tick}` +
      `  │  Section: ${sBar(tick.section_strength_after, r.section_max_strength)}` +
      `  ${tick.section_strength_after}/${r.section_max_strength}` +
      `  │  Vehicle: ${hBar(tick.vehicle_hp_after, r.vehicle_max_hp)}` +
      `  ${Math.max(0, tick.vehicle_hp_after)}/${r.vehicle_max_hp} HP`
    );
    lines.push(SEP);
  }

  lines.push('║  ENGAGEMENT SUMMARY');
  lines.push(`║  Outcome : ${r.outcome}`);
  lines.push(`║  Ticks   : ${r.ticks.length}`);
  lines.push(
    `║  Section : ${sBar(r.section_final_strength, r.section_max_strength)}` +
    `  ${r.section_final_strength}/${r.section_max_strength}`
  );
  lines.push(
    `║  Vehicle : ${hBar(r.vehicle_final_hp, r.vehicle_max_hp)}` +
    `  ${Math.max(0, r.vehicle_final_hp)}/${r.vehicle_max_hp} HP`
  );
  lines.push('║');
  lines.push(`║  ${r.narrative_summary}`);
  lines.push(END);

  return lines.join('\n');
}

// ── Screen 3: after-action report ────────────────────────────────────────

document.getElementById('btn-view-aar').addEventListener('click', async () => {
  try {
    const headers = token ? { 'Authorization': 'Bearer ' + token } : {};
    const r = await fetch(`/api/combat/aar/${sessionId}`, { headers });
    if (!r.ok) throw new Error(await r.text());
    document.getElementById('aar-content').textContent = formatAar(await r.json());
    hide('screen-dashboard');
    show('screen-aar');
  } catch (e) {
    alert('Failed to load AAR: ' + e.message);
  }
});

document.getElementById('btn-back').addEventListener('click', () => {
  hide('screen-aar');
  show('screen-dashboard');
});

// The Commander reveal (BP-3a-1, demo plan §4a screen 4). Built with textContent only: no
// server string is ever parsed as HTML.
//
// The Commander is chosen by the join's `commander_id`, never by list position: the join
// writes its starters in one transaction, so their order is not pinned (Reviewer BP-2b-2 F4).
// Every list entry the server returns is shown; KIA and Shattered records are never filtered.

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function statRow(label, value) {
  const row = el('div', 'stat');
  row.append(el('span', 'stat-label', label), el('span', 'stat-value', value));
  return row;
}

function badges(record) {
  const wrap = el('span', 'badges');
  if (record.is_kia) wrap.append(el('span', 'badge badge-kia', 'KIA'));
  if (record.is_shattered) wrap.append(el('span', 'badge badge-shattered', 'SHATTERED'));
  return wrap;
}

function commanderCard(c) {
  const card = el('article', 'commander-card');
  if (c.is_kia) card.classList.add('is-kia');
  if (c.is_shattered) card.classList.add('is-shattered');

  const portrait = el('div', 'portrait');
  portrait.setAttribute('aria-hidden', 'true'); // placeholder silhouette, art lands later
  portrait.append(el('div', 'portrait-head'), el('div', 'portrait-body'));

  const info = el('div', 'commander-info');
  const name = el('h2', 'commander-name', c.name);
  name.append(badges(c));
  info.append(
    name,
    el('div', 'commander-grade', c.commander_grade ?? 'Ungraded'),
    statRow('Rank', c.rank),
    statRow('Origin', c.origin),
    statRow('Specialisation', c.specialization),
    statRow('Fatal Flaw', c.fatal_flaw),
  );
  if (c.veteran_trait !== null && c.veteran_trait !== undefined) {
    info.append(statRow('Veteran Trait', c.veteran_trait));
  }
  info.append(statRow('Stress', c.stress), statRow('XP', c.xp));

  card.append(portrait, info);
  return card;
}

function chip(kind, main, detail, record) {
  const node = el('li', 'chip');
  node.append(el('span', 'chip-kind', kind), el('span', 'chip-main', main));
  if (detail) node.append(el('span', 'chip-detail', detail));
  if (record) node.append(badges(record));
  return node;
}

function outfit(roster, chosenId, heading) {
  const section = el('section', 'outfit');
  section.append(el('h3', 'outfit-heading', heading));
  const list = el('ul', 'chips');
  for (const c of roster.commanders || []) {
    if (c.commander_id === chosenId) continue; // already the card above
    list.append(chip('Commander', c.name, `Rank ${c.rank}`, c));
  }
  for (const f of roster.foremen || []) list.append(chip('Foreman', f.name, f.foreman_grade, f));
  for (const s of roster.sections || []) list.append(chip('Section', s.name, `${s.headcount} troops`));
  for (const w of roster.crews || []) list.append(chip('Crew', `${w.headcount} miners`, `Morale ${w.morale}`));
  section.append(list);
  return section;
}

// Fills `container` with the reveal. Returns false when the roster has no Commander with
// `commanderId` (the caller shows an error rather than guessing another one).
export function renderReveal(container, roster, commanderId, copy) {
  container.replaceChildren();
  const chosen = (roster.commanders || []).find(c => c.commander_id === commanderId);
  if (!chosen) return false;
  container.append(
    el('h1', 'reveal-heading', copy.heading),
    commanderCard(chosen),
    outfit(roster, chosen.commander_id, copy.outfit),
  );
  return true;
}

// "More below" for the Breakpoint demo (BP-29b, feedback item 26; demo-only: never merged to dev, the port deletes
// this file). Each box that scrolls gets one small glowing chevron at its bottom edge while more of it lies below:
// shown while the box is not scrolled to within 4 px of its end. A box that fits, or isn't rendered (hidden, or
// under a hidden screen), measures scrollTop + clientHeight >= scrollHeight - 4, so it never shows one. The chevron
// is the box's next sibling, out of flow (position: absolute, placed from the two rects), so it moves nothing; it
// carries no text, takes no tap (pointer-events: none) and paints in the box's own stacking context (the reveal's
// sits under the Company card). It is switched by a class, never `hidden`, which demo_shop.js watches across the
// stage. #legend never overflows and the keeper's #missions-list is never shown in the demo (BP-29b spec check).
// Recomputed, once per frame, on: the box's scroll, its content changing, an image in it loading, any `hidden`
// written in the stage (a screen, sheet, card or row shown or hidden), and a window resize.

const BOXES = ['.screen-shop', '.screen-card', '.base-sheet', '#reveal-content', '#aar-units', '.company-card-lines'];
const SLACK = 4; // px

const CSS = `
.more-below { display: none; position: absolute; box-sizing: border-box; width: 3cqw; height: 3cqw; margin-left: -1.5cqw; border-right: 0.8cqw solid var(--accent); border-bottom: 0.8cqw solid var(--accent); transform: rotate(45deg); pointer-events: none; filter: drop-shadow(0 0 0.8cqw var(--accent)); }
.more-below.on { display: block; animation: more-below-pulse 1.4s ease-in-out infinite; }
@keyframes more-below-pulse { 0%, 100% { opacity: 0.45; } 50% { opacity: 1; } }
@media (prefers-reduced-motion: reduce) { .more-below.on { animation: none; } }
`;

const pairs = [];
let queued = false;

function place(box, arrow) {
  const more = box.scrollTop + box.clientHeight < box.scrollHeight - SLACK;
  arrow.classList.toggle('on', more);
  const op = more ? arrow.offsetParent : null;
  if (!op) return;
  const r = box.getBoundingClientRect(), p = op.getBoundingClientRect();
  arrow.style.left = `${r.left + r.width / 2 - p.left - op.clientLeft}px`;
  arrow.style.top = `${r.bottom - p.top - op.clientTop - 2 * arrow.offsetHeight}px`;
}

function update() {
  queued = false;
  for (const [box, arrow] of pairs) place(box, arrow);
}

function schedule() {
  if (queued) return;
  queued = true;
  requestAnimationFrame(update);
}

// Called last in mountOps, when every box exists.
export function mountMore() {
  const style = document.createElement('style');
  style.textContent = CSS;
  document.head.append(style);
  const content = new MutationObserver(schedule);
  for (const box of document.querySelectorAll(BOXES.join(', '))) {
    const arrow = document.createElement('div');
    arrow.className = 'more-below';
    arrow.setAttribute('aria-hidden', 'true');
    box.after(arrow);
    pairs.push([box, arrow]);
    box.addEventListener('scroll', schedule, { passive: true });
    box.addEventListener('load', schedule, true); // an image that loads late changes the box's height
    content.observe(box, { childList: true, subtree: true, characterData: true });
  }
  new MutationObserver(schedule).observe(document.getElementById('stage'), { subtree: true, attributes: true, attributeFilter: ['hidden'] });
  window.addEventListener('resize', schedule);
  schedule();
}

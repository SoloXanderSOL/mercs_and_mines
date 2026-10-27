// The sector hex map (BP-3a-2). Flat-top axial hexes, terrain drawn once into an offscreen
// layer (one Path2D per terrain type); veins and the gateway are a vector overlay drawn each
// frame, so they stay crisp at any zoom. Pointer Events pan and pinch; a frame is drawn only
// while a pointer moves (requestAnimationFrame, one queued at most), never on a timer.

const SQRT3 = Math.sqrt(3);

// The offscreen layer's longest side in device px (spec check: 2048 keeps pan frames at
// ~0.3 ms on a throttled phone, against ~39 ms at 4096, for 14 MB instead of 56 MB).
export const LAYER_CAP_PX = 2048;

const RADIUS = 35;                      // sector radius in hexes
const MAP_W = 3 * RADIUS + 2;           // flat-top map width in circumradius units (107)
const MAP_H = SQRT3 * (2 * RADIUS + 1); // flat-top map height in circumradius units (122.98)
const MAX_S = 20;                       // a hex 40 CSS px across
const INIT_S = 6;                       // a hex 12 CSS px across

// Wiki's palette (BP-3a-2), keyed by the `HexTerrain` variant strings (client contract).
export const PALETTE = {
  AshenPlains: '#6e6a62', ScorchedDesert: '#8a6a3a', RuinedCity: '#57493f', CityState: '#c9a24a',
  GunOutpost: '#6f8a9a', ToxicZone: '#6f8f2a', MagmaFlow: '#d2451e', CooledMagma: '#2e2a2b',
  VolcanicCaldera: '#8a1f12', Mountain: '#3d4147', Ravine: '#151517',
};
export const TERRAIN_ORDER = [
  'AshenPlains', 'ScorchedDesert', 'RuinedCity', 'CityState', 'GunOutpost', 'ToxicZone',
  'MagmaFlow', 'CooledMagma', 'VolcanicCaldera', 'Mountain', 'Ravine',
];
export const OVERLAY = { vein: '#ff8c2a', anchor: '#ffd27a', gateway: '#ffffff' };
const UNKNOWN = '#ff00ff';

// Axial (q, r) → map units (circumradius 1), flat-top; (0, 0) is the safe-zone centre.
function axialToUnit(q, r) {
  return { x: 1.5 * q, y: SQRT3 * (r + q / 2) };
}

function hexPath(path, cx, cy, s) {
  for (let i = 0; i < 6; i++) {
    const a = (Math.PI / 3) * i;
    const x = cx + s * Math.cos(a);
    const y = cy + s * Math.sin(a);
    if (i === 0) path.moveTo(x, y); else path.lineTo(x, y);
  }
  path.closePath();
}

export class HexMap {
  // Builds the layer and draws the initial view at once (the caller times this).
  // `sector` is the sector route's body: {terrain, magma_veins, gateway (or null)}.
  constructor(canvas, sector) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.dpr = Math.min(window.devicePixelRatio || 1, 2);
    this.veins = sector.magma_veins || [];
    this.gateway = sector.gateway || null; // null: no outline, centred on the sector (Reviewer BP-1c F1)
    this.unknownTerrain = new Set();
    this.pointers = new Map();
    this.frameQueued = false;
    this.listeners = new AbortController();
    this.buildLayer(sector.terrain || {});
    this.measure();
    this.home();
    this.bindGestures();
  }

  // Terrain into the offscreen layer, once: 11 fills, not 3,781.
  buildLayer(terrain) {
    const unit = Math.min(LAYER_CAP_PX / Math.max(MAP_W, MAP_H), MAX_S * this.dpr); // device px per unit
    this.layerUnit = unit;
    const layer = document.createElement('canvas');
    layer.width = Math.ceil(MAP_W * unit);
    layer.height = Math.ceil(MAP_H * unit);
    const paths = {};
    const ox = MAP_W / 2, oy = MAP_H / 2;
    for (const key in terrain) {
      const comma = key.indexOf(',');
      const q = Number(key.slice(0, comma)), r = Number(key.slice(comma + 1));
      const t = terrain[key];
      const { x, y } = axialToUnit(q, r);
      if (!paths[t]) paths[t] = new Path2D();
      hexPath(paths[t], (x + ox) * unit, (y + oy) * unit, unit * 0.98);
    }
    const lctx = layer.getContext('2d');
    for (const t in paths) {
      if (!(t in PALETTE)) this.unknownTerrain.add(t);
      lctx.fillStyle = PALETTE[t] || UNKNOWN;
      lctx.fill(paths[t]);
    }
    this.layer = layer;
  }

  // Sizes the canvas to its box. Returns false while the box has no size (screen hidden).
  measure() {
    const rect = this.canvas.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return false;
    this.w = rect.width;
    this.h = rect.height;
    this.canvas.width = Math.round(this.w * this.dpr);
    this.canvas.height = Math.round(this.h * this.dpr);
    this.minS = Math.min(this.w / MAP_W, this.h / MAP_H); // the whole sector fits
    return true;
  }

  // After a layout change (the toolbar, a turn of the phone, returning to the hub).
  resize() {
    if (!this.measure()) return;
    this.clamp();
    this.render();
  }

  // The initial view, and the Recentre button: the gateway (or the centre) at INIT_S.
  home() {
    const c = this.gateway ? axialToUnit(this.gateway.q, this.gateway.r) : { x: 0, y: 0 };
    this.view = { cx: c.x, cy: c.y, s: INIT_S };
    this.clamp();
    this.render();
  }

  clamp() {
    if (!this.w) return;
    const v = this.view;
    v.s = Math.min(MAX_S, Math.max(this.minS, v.s));
    const hx = Math.max(0, MAP_W / 2 - this.w / (2 * v.s));
    const hy = Math.max(0, MAP_H / 2 - this.h / (2 * v.s));
    v.cx = Math.min(hx, Math.max(-hx, v.cx));
    v.cy = Math.min(hy, Math.max(-hy, v.cy));
  }

  requestFrame() {
    if (this.frameQueued) return;
    this.frameQueued = true;
    requestAnimationFrame(() => { this.frameQueued = false; this.render(); });
  }

  render() {
    if (!this.w) return;
    const { ctx, view: v, dpr } = this;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = '#000';
    ctx.fillRect(0, 0, this.w, this.h);
    const k = v.s / this.layerUnit; // layer px → CSS px
    ctx.drawImage(this.layer,
      (-MAP_W / 2 - v.cx) * v.s + this.w / 2, (-MAP_H / 2 - v.cy) * v.s + this.h / 2,
      this.layer.width * k, this.layer.height * k);
    const toScreen = (q, r) => {
      const u = axialToUnit(q, r);
      return [(u.x - v.cx) * v.s + this.w / 2, (u.y - v.cy) * v.s + this.h / 2];
    };
    // Veins: a dot sized by tier; a tectonic anchor gets a ring.
    for (const vein of this.veins) {
      const [x, y] = toScreen(vein.q, vein.r);
      if (x < -v.s * 2 || y < -v.s * 2 || x > this.w + v.s * 2 || y > this.h + v.s * 2) continue;
      const rad = Math.max(1.5, v.s * (0.25 + 0.07 * vein.tier));
      ctx.beginPath();
      ctx.arc(x, y, rad, 0, Math.PI * 2);
      ctx.fillStyle = OVERLAY.vein;
      ctx.fill();
      if (vein.is_tectonic_anchor) {
        ctx.beginPath();
        ctx.arc(x, y, rad + Math.max(1.5, v.s * 0.2), 0, Math.PI * 2);
        ctx.strokeStyle = OVERLAY.anchor;
        ctx.lineWidth = Math.max(1, v.s * 0.08);
        ctx.stroke();
      }
    }
    // The gateway, last, so nothing covers it.
    if (this.gateway) {
      const [x, y] = toScreen(this.gateway.q, this.gateway.r);
      const p = new Path2D();
      hexPath(p, x, y, v.s);
      ctx.strokeStyle = OVERLAY.gateway;
      ctx.lineWidth = 2;
      ctx.stroke(p);
    }
  }

  bindGestures() {
    const c = this.canvas;
    const opts = { signal: this.listeners.signal };
    const end = e => { this.pointers.delete(e.pointerId); this.gestureStart(); };
    c.addEventListener('pointerdown', e => {
      c.setPointerCapture(e.pointerId);
      this.pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      this.gestureStart();
    }, opts);
    c.addEventListener('pointermove', e => {
      if (!this.pointers.has(e.pointerId)) return;
      this.pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      this.gestureMove();
    }, opts);
    c.addEventListener('pointerup', end, opts);
    c.addEventListener('pointercancel', end, opts);
  }

  // Re-anchors the gesture whenever the finger count changes, so 2 → 1 fingers pans on
  // without a jump.
  gestureStart() {
    this.g = { pts: [...this.pointers.values()].map(p => ({ ...p })), view: { ...this.view } };
  }

  gestureMove() {
    const g = this.g;
    if (!g || !this.w) return;
    const pts = [...this.pointers.values()];
    if (pts.length === 1 && g.pts.length === 1) {
      this.view.cx = g.view.cx - (pts[0].x - g.pts[0].x) / g.view.s;
      this.view.cy = g.view.cy - (pts[0].y - g.pts[0].y) / g.view.s;
    } else if (pts.length >= 2 && g.pts.length >= 2) {
      const rect = this.canvas.getBoundingClientRect();
      const d0 = Math.hypot(g.pts[0].x - g.pts[1].x, g.pts[0].y - g.pts[1].y) || 1;
      const d1 = Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y);
      const s = Math.min(MAX_S, Math.max(this.minS, g.view.s * d1 / d0));
      // keep the map point under the starting midpoint under the current midpoint
      const m0x = (g.pts[0].x + g.pts[1].x) / 2 - rect.left, m0y = (g.pts[0].y + g.pts[1].y) / 2 - rect.top;
      const m1x = (pts[0].x + pts[1].x) / 2 - rect.left, m1y = (pts[0].y + pts[1].y) / 2 - rect.top;
      const ux = g.view.cx + (m0x - this.w / 2) / g.view.s, uy = g.view.cy + (m0y - this.h / 2) / g.view.s;
      this.view = { s, cx: ux - (m1x - this.w / 2) / s, cy: uy - (m1y - this.h / 2) / s };
    }
    this.clamp();
    this.requestFrame();
  }

  // Removes the gesture listeners (a new campaign gets a new map on the same canvas).
  destroy() {
    this.listeners.abort();
    this.pointers.clear();
  }
}

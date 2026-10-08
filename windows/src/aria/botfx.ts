// Bot FX — state-based gestures for the mini Grok Bot mascots.
//
// One `BotGestureFx` per bot pill id (`agent_bot-<slug>`), shared by every
// canvas showing that bot (pill, compact grid). It lives across view rebuilds,
// so a pill redrawn on a state change carries on from the pose it was in
// instead of snapping. It plugs into BotEngine through the `gesture` hook.
//
//   trabajando  eyes dart left/right/up (saccades) + soft pulse glow in the bot colour
//   pregunta    head tilt (~11°) + a "?" bubble that pops above the head
//   listo       two happy hops with squash & stretch, then a smile and content eyes
//   error       decaying horizontal shake, worried brows + frown, faint red tint
//
// `prefers-reduced-motion: reduce` keeps the faces, tilt, tint and a static glow
// but drops the hops, shake, saccades, bobbing and flying sparkles.

import "../botfx.css";
import { BOT_PREFIX } from "../core/state";
import type { BotStateName } from "../core/layout";
import {
  BOT_STATES, BotEngine, hexToRGB,
  type BotGesture, type EyeShape, type GestureFrame, type RGB,
} from "./engine";

// ── Helpers ───────────────────────────────────────────────────────────────────

type Phase = "work" | "ask" | "done" | "error" | "none";
const PHASES: readonly Phase[] = ["work", "ask", "done", "error", "none"];

function phaseOf(s: BotStateName): Phase {
  switch (s) {
    case "working":
    case "thinking":
    case "searching":
      return "work";
    case "approval":
    case "question":
      return "ask";
    case "finished":
      return "done";
    case "error":
      return "error";
    default:
      return "none";
  }
}

const ERROR_RGB: RGB = [0.957, 0.314, 0.369]; // #F4505E
const INK = "rgb(16,19,26)";
const FONT = `system-ui, "Segoe UI Variable Text", "Segoe UI", sans-serif`;
const HEX = /^#?[0-9a-f]{6}$/i;

const nowS = () => performance.now() / 1000;
const clamp01 = (v: number) => Math.max(0, Math.min(1, v));
const smooth = (t: number) => { const c = clamp01(t); return c * c * (3 - 2 * c); };
const rgba = (c: RGB, a: number) =>
  `rgba(${Math.round(c[0] * 255)},${Math.round(c[1] * 255)},${Math.round(c[2] * 255)},${clamp01(a)})`;
const lighten = (c: RGB, t: number): RGB => [c[0] + (1 - c[0]) * t, c[1] + (1 - c[1]) * t, c[2] + (1 - c[2]) * t];
/** Frame-rate independent approach factor for a time constant `tau` seconds. */
const approach = (dt: number, tau: number) => 1 - Math.exp(-dt / tau);

const motionQuery: MediaQueryList | null =
  typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(prefers-reduced-motion: reduce)")
    : null;
let reducedMotion = motionQuery?.matches ?? false;
motionQuery?.addEventListener?.("change", (e) => { reducedMotion = e.matches; });

/** Small underdamped spring (for the "?" bubble pop). */
class Spring1 {
  v = 0;
  vel = 0;
  step(target: number, dt: number, response: number, damping: number) {
    const w = (2 * Math.PI) / response;
    const n = Math.max(1, Math.ceil(dt / (1 / 240)));
    const h = dt / n;
    for (let i = 0; i < n; i++) {
      this.vel += (w * w * (target - this.v) - 2 * damping * w * this.vel) * h;
      this.v += this.vel * h;
    }
  }
}

interface Sparkle {
  a: number; // direction
  d: number; // travel distance (R units)
  age: number;
  life: number;
  size: number;
  spin: number;
  white: boolean;
}

/** Hop sequence: (height, duration) per jump. */
const HOPS: readonly (readonly [number, number])[] = [[0.34, 0.5], [0.2, 0.42]];
const HOP_TOTAL = HOPS.reduce((s, h) => s + h[1], 0);
const SHAKE_DUR = 0.7;
const FLASH_DUR = 0.75;

// ── Per-bot gesture state ─────────────────────────────────────────────────────

export class BotGestureFx implements BotGesture {
  readonly id: string;

  // BotGesture outputs (recomputed in update()).
  dx = 0; dy = 0; rot = 0; sx = 1; sy = 1;
  look: readonly [number, number] | null = null;
  eye: EyeShape | null = null;
  open = 1;

  private started = false;
  private state: BotStateName = "idle";
  private shownState: BotStateName = "idle";
  private color: RGB | null = null;
  private w: Record<Phase, number> = { work: 0, ask: 0, done: 0, error: 0, none: 1 };
  private t = Math.random() * 10;

  private saccade: [number, number] = [0, 0];
  private nextSaccade = 0;
  private blinkAt = -1; // state-change blink start (s), masks the eye swap
  private hopAt = -1;
  private hopSparked = false;
  private shakeAt = -1;
  private flashAt = -1;
  private rotNow = 0;
  private bubble = new Spring1();
  private sparkles: Sparkle[] = [];
  lastSeen = nowS();

  constructor(id: string) {
    this.id = id;
  }

  /** Bot colour (hex). Invalid → null, and the colour-dependent effects fall back to the state colour. */
  setColor(hex: string | null | undefined) {
    this.color = hex && HEX.test(hex) ? hexToRGB(hex.startsWith("#") ? hex : `#${hex}`) : null;
  }

  /**
   * Called on every sync; only an actual change starts a transition. The very
   * first call settles straight into the pose, with no entry gesture.
   */
  setState(next: BotStateName) {
    const initial = !this.started;
    this.started = true;
    if (next === this.state && !initial) return;
    const prevPhase = phaseOf(this.state);
    this.state = next;
    const phase = phaseOf(next);
    if (initial) {
      this.shownState = next;
      for (const p of PHASES) this.w[p] = p === phase ? 1 : 0;
      this.eye = eyeFor(next);
      this.rotNow = this.rotTarget();
      this.bubble.v = phase === "ask" ? 1 : 0;
      return;
    }
    const t = nowS();
    this.blinkAt = t;
    if (phase === prevPhase) return;
    if (phase === "done") { this.hopAt = t; this.hopSparked = false; }
    if (phase === "error") this.shakeAt = t;
    if (phase === "work") this.nextSaccade = 0;
  }

  onState(next: BotStateName) {
    this.setState(next);
  }

  /** Short glow + sparkle burst (an approval was answered). */
  flash() {
    this.flashAt = nowS();
    if (reducedMotion) return;
    this.burst(7, 0.95);
  }

  private burst(count: number, reach: number) {
    for (let i = 0; i < count; i++) {
      this.sparkles.push({
        a: (i / count) * Math.PI * 2 + Math.random() * 0.6 - Math.PI / 2,
        d: reach * (0.75 + Math.random() * 0.45),
        age: -i * 0.025,
        life: 0.55 + Math.random() * 0.25,
        size: 0.13 + Math.random() * 0.07,
        spin: (Math.random() - 0.5) * 6,
        white: i % 2 === 0,
      });
    }
  }

  private rotTarget(): number {
    // ~11° total: the engine already eases towards the state's own tilt.
    return phaseOf(this.state) === "ask" ? 0.19 - BOT_STATES[this.state].tilt : 0;
  }

  update(dt: number) {
    const n = nowS();
    this.t += dt;
    const t = this.t;
    const reduced = reducedMotion;
    const phase = phaseOf(this.state);

    // Cross-fade between state poses (≈ 0.2 s time constant).
    const k = approach(dt, 0.2);
    for (const p of PHASES) this.w[p] += ((p === phase ? 1 : 0) - this.w[p]) * k;
    const ask = this.w.ask;

    // State-change blink: eyes close, the shape swaps while closed, they reopen.
    this.open = 1;
    if (this.blinkAt >= 0) {
      const b = n - this.blinkAt;
      if (b < 0.08) this.open = 1 - smooth(b / 0.08) * 0.94;
      else {
        this.shownState = this.state;
        this.open = b < 0.22 ? 0.06 + smooth((b - 0.08) / 0.14) * 0.94 : 1;
        if (b >= 0.22) this.blinkAt = -1;
      }
    } else {
      this.shownState = this.state;
    }
    this.eye = eyeFor(this.shownState);

    // Look target.
    if (phase === "work") {
      if (n > this.nextSaccade) {
        this.saccade = reduced ? [(Math.random() - 0.5) * 0.3, 0] : pickSaccade(this.saccade);
        this.nextSaccade = n + (reduced ? 3 + Math.random() * 2 : 0.45 + Math.random() * 1.1);
      }
      this.look = this.saccade;
    } else if (phase === "ask") {
      this.look = [0.38, -0.5]; // up towards the bubble
    } else if (phase === "done") {
      this.look = [0, -0.12];
    } else if (phase === "error") {
      this.look = [0, 0.3];
    } else {
      this.look = null;
    }

    // Head tilt (+ a gentle sway while asking).
    this.rotNow += (this.rotTarget() - this.rotNow) * approach(dt, 0.16);
    this.rot = this.rotNow + (reduced ? 0 : Math.sin(t * 1.7) * 0.025 * ask);

    // "?" bubble: underdamped pop in, quick shrink out.
    if (reduced) this.bubble.step(phase === "ask" ? 1 : 0, dt, 0.35, 1);
    else this.bubble.step(phase === "ask" ? 1 : 0, dt, 0.42, phase === "ask" ? 0.42 : 0.9);

    // Body pose: hop (listo) and shake (error).
    let dx = 0, dy = 0, sx = 1, sy = 1;
    if (this.hopAt >= 0) {
      const h = n - this.hopAt;
      if (h >= HOP_TOTAL || reduced) this.hopAt = -1;
      else {
        const pose = hopPose(h);
        dy += pose.dy; sx *= pose.sx; sy *= pose.sy;
        if (!this.hopSparked && h >= HOPS[0][1] * 0.8) {
          this.hopSparked = true;
          this.burst(3, 0.7);
        }
      }
    }
    if (this.shakeAt >= 0) {
      const s = n - this.shakeAt;
      if (s >= SHAKE_DUR || reduced) this.shakeAt = -1;
      else dx += 0.13 * Math.exp(-s / 0.17) * Math.sin(s * Math.PI * 2 * 10);
    }
    // Flash "boing".
    if (this.flashAt >= 0) {
      const f = n - this.flashAt;
      if (f >= FLASH_DUR) this.flashAt = -1;
      else if (!reduced) {
        const b = Math.sin(clamp01(f / 0.35) * Math.PI) * (1 - f / FLASH_DUR);
        sx *= 1 + 0.07 * b; sy *= 1 - 0.05 * b;
      }
    }
    // Keep the feet planted when squashing (body bottom ≈ 0.88 R below centre).
    dy += (1 - sy) * 0.88;
    this.dx = dx; this.dy = dy; this.sx = sx; this.sy = sy;

    for (const s of this.sparkles) s.age += dt;
    this.sparkles = this.sparkles.filter((s) => s.age < s.life);
  }

  // ── Drawing hooks ───────────────────────────────────────────────────────────

  private tone(engine: BotEngine): RGB {
    return this.color ?? engine.bodyColor ?? BOT_STATES[this.state].color;
  }

  drawUnder(x: CanvasRenderingContext2D, f: GestureFrame) {
    const { R, cx, cy } = f;
    const c = this.tone(f.engine);
    const pulse = reducedMotion ? 0.5 : 0.5 + 0.5 * Math.sin(this.t * Math.PI * 2 * 0.75);
    let alpha = this.w.work * (0.2 + 0.2 * pulse);
    let radius = R * (1.3 + 0.12 * pulse * this.w.work);
    if (this.flashAt >= 0) {
      const p = clamp01((nowS() - this.flashAt) / FLASH_DUR);
      const burst = (p < 0.15 ? p / 0.15 : 1 - (p - 0.15) / 0.85) * 0.75;
      alpha = Math.max(alpha, burst);
      radius = Math.max(radius, R * (1.25 + 0.4 * p));
    }
    if (alpha < 0.01) return;
    const g = x.createRadialGradient(cx, cy, R * 0.5, cx, cy, radius);
    g.addColorStop(0, rgba(lighten(c, 0.15), alpha));
    g.addColorStop(1, rgba(c, 0));
    x.fillStyle = g;
    x.beginPath();
    x.arc(cx, cy, radius, 0, Math.PI * 2);
    x.fill();
  }

  drawBody(x: CanvasRenderingContext2D, body: Path2D) {
    const a = this.w.error * 0.24;
    if (a < 0.01) return;
    x.fillStyle = rgba(ERROR_RGB, a);
    x.fill(body);
  }

  drawFace(x: CanvasRenderingContext2D, body: Path2D, f: GestureFrame) {
    const { engine, R, rx, ry } = f;
    const { done, error } = this.w;
    if (done < 0.02 && error < 0.02) return;
    const ew = R * 0.25 * (engine.isMini ? 1.9 : 1) * engine.es;
    const lw = Math.max(1, R * 0.09);

    x.save();
    x.clip(body);
    x.strokeStyle = INK;
    x.lineCap = "round";
    x.lineWidth = lw;

    // Worried brows: inner ends raised.
    if (error > 0.02) {
      x.globalAlpha = error;
      for (const sd of [-1, 1] as const) {
        const p = engine.facePoint(sd * BotEngine.EYE_SPACING, rx, ry);
        if (!p) continue;
        const by = p.y - ew * 0.95;
        const half = ew * 0.5 * p.fx;
        const lift = ew * 0.3 * error;
        x.beginPath();
        x.moveTo(p.x + sd * half, by + lift * 0.4); // outer, lower
        x.lineTo(p.x - sd * half, by - lift);       // inner, higher
        x.stroke();
      }
    }

    // Mouth: smile (listo) blending into a small frown (error).
    const m = engine.facePoint(0, rx, ry);
    if (m) {
      const curve = done - error; // +1 smile … −1 frown
      const amt = Math.max(done, error);
      const mw = R * 0.2 * m.fx * (1 + 0.25 * done);
      const my = m.y + R * 0.4 * m.fy;
      x.globalAlpha = clamp01(amt);
      x.beginPath();
      x.moveTo(m.x - mw, my - curve * R * 0.02);
      x.quadraticCurveTo(m.x, my + curve * R * 0.16, m.x + mw, my - curve * R * 0.02);
      x.stroke();
    }
    x.restore();
  }

  drawOver(x: CanvasRenderingContext2D, f: GestureFrame) {
    const { R, cx, cy } = f;
    const c = this.tone(f.engine);

    // "?" bubble above the head, top-right (the state badge sits top-left).
    const s = Math.max(0, this.bubble.v);
    if (s > 0.02) {
      const bob = reducedMotion ? 0 : Math.sin(this.t * 3.1) * 0.04 * R;
      const bx = cx + R * 0.66;
      const by = cy - R * 1.2 + bob;
      x.save();
      x.translate(bx, by);
      x.scale(s, s);
      x.globalAlpha = clamp01(s);
      x.fillStyle = "rgba(255,255,255,0.96)";
      x.beginPath();
      x.arc(0, 0, R * 0.34, 0, Math.PI * 2);
      x.fill();
      x.beginPath();
      x.arc(-R * 0.27, R * 0.33, R * 0.08, 0, Math.PI * 2);
      x.fill();
      x.strokeStyle = rgba(c, 0.9);
      x.lineWidth = Math.max(0.75, R * 0.05);
      x.beginPath();
      x.arc(0, 0, R * 0.34, 0, Math.PI * 2);
      x.stroke();
      x.fillStyle = INK;
      x.font = `900 ${R * 0.46}px ${FONT}`;
      x.textAlign = "center";
      x.textBaseline = "middle";
      x.fillText("?", 0, R * 0.03);
      x.restore();
    }

    // Sparkles (flash burst, landing of a hop).
    for (const p of this.sparkles) {
      if (p.age <= 0) continue;
      const k = p.age / p.life;
      const travel = 1 - Math.pow(1 - k, 3);
      const px = cx + Math.cos(p.a) * (0.9 + p.d * travel) * R;
      const py = cy + Math.sin(p.a) * (0.75 + p.d * travel) * R;
      const sz = R * p.size * (1 - k * 0.5);
      x.save();
      x.translate(px, py);
      x.rotate(p.spin * p.age);
      x.globalAlpha = k < 0.2 ? k / 0.2 : 1 - (k - 0.2) / 0.8;
      x.fillStyle = p.white ? "#fff" : rgba(lighten(c, 0.35), 1);
      sparklePath(x, sz);
      x.fill();
      x.restore();
    }
  }
}

function eyeFor(s: BotStateName): EyeShape | null {
  switch (phaseOf(s)) {
    case "work":
    case "ask":
      return s === "approval" ? "wide" : "pill";
    case "done":
      return "happy";
    case "error":
      return "pill"; // worried round eyes; the brows do the talking
    default:
      return null;
  }
}

/** Next eye target while working: left, right, up-left, up-right or centre. */
function pickSaccade(prev: readonly [number, number]): [number, number] {
  const spots: [number, number][] = [
    [-0.85, 0.05], [0.85, 0.05], [-0.45, -0.6], [0.45, -0.6], [0, -0.15], [0, 0.25],
  ];
  let next = spots[Math.floor(Math.random() * spots.length)];
  if (next[0] === prev[0] && next[1] === prev[1]) next = spots[(spots.indexOf(next) + 1) % spots.length];
  return [next[0] + (Math.random() - 0.5) * 0.15, next[1] + (Math.random() - 0.5) * 0.1];
}

/** Squash & stretch hop pose at time `h` seconds into the sequence. */
function hopPose(h: number): { dy: number; sx: number; sy: number } {
  let start = 0;
  for (const [height, dur] of HOPS) {
    if (h < start + dur) {
      const p = (h - start) / dur;
      // 0–0.18 crouch, 0.18–0.8 airborne, 0.8–1 landing squash.
      if (p < 0.18) {
        const c = Math.sin((p / 0.18) * Math.PI * 0.5);
        return { dy: 0, sx: 1 + 0.12 * c, sy: 1 - 0.15 * c };
      }
      if (p < 0.8) {
        const a = (p - 0.18) / 0.62;
        const stretch = Math.cos(a * Math.PI); // +1 take-off … −1 touch-down
        const st = 0.12 * Math.abs(stretch);
        return { dy: -height * Math.sin(a * Math.PI), sx: 1 - st * 0.6, sy: 1 + st };
      }
      const l = (p - 0.8) / 0.2;
      const c = Math.sin(l * Math.PI);
      return { dy: 0, sx: 1 + 0.1 * c, sy: 1 - 0.12 * c };
    }
    start += dur;
  }
  return { dy: 0, sx: 1, sy: 1 };
}

function sparklePath(x: CanvasRenderingContext2D, r: number) {
  const ri = r * 0.28;
  x.beginPath();
  for (let i = 0; i < 8; i++) {
    const rr = i % 2 ? ri : r;
    const a = -Math.PI / 2 + (i * Math.PI) / 4;
    x.lineTo(Math.cos(a) * rr, Math.sin(a) * rr);
  }
  x.closePath();
}

// ── Registry ──────────────────────────────────────────────────────────────────

const registry = new Map<string, BotGestureFx>();

/** True for Grok Bot pills (`agent_bot-<slug>`), the only mascots that gesture. */
export const isBotTask = (id: string) => id.startsWith(BOT_PREFIX);

/** The shared gesture state for a bot pill, created on first use. */
export function gestureFor(id: string): BotGestureFx {
  let fx = registry.get(id);
  if (!fx) {
    fx = new BotGestureFx(id);
    registry.set(id, fx);
  }
  fx.lastSeen = nowS();
  return fx;
}

/**
 * Advances every bot's gestures once per frame. `liveIds` are the bots that
 * still have a canvas; bots unseen for a minute are forgotten.
 */
export function tickBotFx(dt: number, liveIds: ReadonlySet<string>) {
  const n = nowS();
  for (const [id, fx] of registry) {
    if (liveIds.has(id)) fx.lastSeen = n;
    else if (n - fx.lastSeen > 60) { registry.delete(id); continue; }
    fx.update(dt);
  }
}

function resolve(agentId: string): BotGestureFx | undefined {
  return registry.get(agentId) ?? registry.get(`agent_${agentId}`) ?? registry.get(`${BOT_PREFIX}${agentId}`);
}

export type BotFxClass = "enter" | "pulse" | "flash" | "shake" | "pop";

export const botFx = {
  /**
   * Short glow + sparkle burst on that bot's mascot(s). Accepts the pill id
   * (`agent_bot-aegon`), the hook id (`bot-aegon`) or the slug (`aegon`).
   * Unknown ids are a no-op.
   */
  flash(agentId: string): void {
    resolve(agentId)?.flash();
  },

  /**
   * Restarts a one-shot CSS effect from botfx.css on any element (adds
   * `botfx-<name>`, removed again on animationend; `pulse` stays until
   * `clear`). Pass `color` to set `--botfx-color`.
   */
  play(el: HTMLElement, name: BotFxClass, color?: string): void {
    if (color) el.style.setProperty("--botfx-color", color);
    const cls = `botfx-${name}`;
    el.classList.remove(cls);
    void el.offsetWidth; // restart the animation
    el.classList.add(cls);
    if (name === "pulse") return;
    el.addEventListener("animationend", () => el.classList.remove(cls), { once: true });
  },

  /** Removes every botfx-* effect class from an element. */
  clear(el: HTMLElement): void {
    for (const n of ["enter", "pulse", "flash", "shake", "pop"] as const) el.classList.remove(`botfx-${n}`);
  },

  /** Whether the OS asks for reduced motion (effects are toned down when true). */
  get reducedMotion(): boolean {
    return reducedMotion;
  },
};

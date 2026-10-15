// Flow dictation's pill (pill.html). From Coucou's pill page (FlowPillView).
//
// What it shows comes whole from Rust in `pill-state` events; the nine dots
// come in `pill-bands` about 30 times a second while you speak. The page tells
// Rust where the pill and its bubble are (`pill_hot`), so the rest of the
// window lets clicks through; it draws only while the pill is on screen.

import "./pill.css";
import { Bridge, onEvent } from "../core/bridge";
import { FlowBridge, keyLabel, type PillState } from "../core/flow";
import type { Settings } from "../core/state";
import { BotEngine } from "../aria/engine";
import { resolveLanguage, setLanguage, systemLanguages, t } from "../i18n/i18n";

const BANDS = 9;
const THICKNESS = 34;

const ICONS: Record<string, string> = {
  x: "M6 6l12 12M18 6L6 18",
  check: "M5 12.5l4.5 4.5L19 7.5",
  mic: "M12 3a3 3 0 0 1 3 3v6a3 3 0 0 1-6 0V6a3 3 0 0 1 3-3zM5 11a7 7 0 0 0 14 0M12 18v3",
  copy: "M9 9h10v10H9zM5 15V5h10",
  download: "M12 4v11M7 10l5 5 5-5M5 20h14",
};

let state: PillState = { phase: "idle", key: "right_ctrl", announcement: null };
let hovered = false;
let visible = false;
const levels = new Array<number>(BANDS).fill(0);
const target = new Array<number>(BANDS).fill(0);

const root = document.getElementById("pill")!;

function svgIcon(name: string): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", ICONS[name] ?? "");
  svg.append(path);
  return svg;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", text = ""): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text) e.textContent = text;
  return e;
}

// ── ARIA ──────────────────────────────────────────────────────────────────────

const aria = new BotEngine();
aria.isMini = true;
aria.setState("idle", true);
const ariaCanvas = el("canvas");

function ariaSlot(): HTMLElement {
  const slot = el("span", "aria");
  slot.append(ariaCanvas);
  return slot;
}

// ── Looks ─────────────────────────────────────────────────────────────────────

type Look = "hidden" | "live" | "handsFree" | "message";

function look(): Look {
  switch (state.phase) {
    case "locked":
      return "handsFree";
    case "listening":
    case "thinking":
      return "live";
    case "copied":
    case "failed":
    case "pastedAgain":
      return "message";
    default:
      return "hidden";
  }
}

function messageText(): string {
  switch (state.phase) {
    case "copied":
      return t("Copied · press Ctrl+V to paste");
    case "failed":
      return t("Microphone unavailable");
    case "pastedAgain":
      return t("Last dictation pasted");
    default:
      return "";
  }
}

function measure(text: string): number {
  const c = document.createElement("canvas").getContext("2d")!;
  c.font = "500 12px system-ui, 'Segoe UI', sans-serif";
  return Math.ceil(c.measureText(text).width);
}

function length(l: Look): number {
  switch (l) {
    case "handsFree":
      return 158;
    case "message":
      return measure(messageText()) + 22 + 9 + 26 + 2;
    default:
      return 118;
  }
}

function pillButton(icon: string, tip: string, action: () => void, filled = false): HTMLElement {
  const b = el("button", filled ? "pbtn filled" : "pbtn");
  b.title = tip;
  b.append(svgIcon(icon));
  b.addEventListener("click", (e) => {
    e.stopPropagation();
    action();
  });
  return b;
}

const barsEl = el("span", "bars");
for (let i = 0; i < BANDS; i++) barsEl.append(el("i"));

function content(l: Look): HTMLElement {
  const c = el("div", "content");
  const key = keyLabel(state.key);
  switch (l) {
    case "handsFree":
      c.append(
        pillButton("x", t("Cancel"), () => void FlowBridge.pillAction("cancel")),
        barsEl,
        pillButton("check", t("Paste · or press {0}", { 0: key }), () => void FlowBridge.pillAction("finish"), true),
      );
      break;
    case "message":
      c.append(ariaSlot(), el("span", "message", messageText()));
      break;
    default:
      c.append(ariaSlot(), barsEl);
  }
  return c;
}

function bubble(): HTMLElement | null {
  const a = state.announcement;
  if (!a) return null;
  const b = el("div", "bubble");
  const icon = el("span", "icon");
  const name = { mic: "mic", clipboard: "copy", download: "download" }[a.icon] ?? "check";
  icon.append(svgIcon(name));
  icon.style.color = a.tint;
  b.append(icon, el("span", "text", a.text));
  if (a.action) {
    const act = el("button", "act", a.action);
    act.addEventListener("click", () => void FlowBridge.pillAction("announcement", a.id));
    b.append(act);
  }
  const close = el("button", "close");
  close.title = t("Close");
  close.append(svgIcon("x"));
  close.addEventListener("click", () => void FlowBridge.pillAction("dismiss", a.id));
  b.append(close);
  return b;
}

// ── Drawing ───────────────────────────────────────────────────────────────────

let capsule: HTMLElement | null = null;
let lastKey = "";

function render() {
  const l = look();
  const key = `${l}|${state.phase}|${state.key}`;
  if (!capsule || key !== lastKey) {
    lastKey = key;
    const next = el("div", "capsule");
    next.append(content(l));
    if (capsule) {
      // Keep the size it had, so the new one grows from it.
      next.style.width = capsule.style.width;
      next.style.height = capsule.style.height;
    }
    capsule = next;
  }
  const open = l !== "hidden";
  capsule.classList.toggle("open", open);
  capsule.classList.toggle("handsfree", l === "handsFree");
  requestAnimationFrame(() => {
    if (!capsule) return;
    capsule.style.width = `${open ? length(l) : 0}px`;
    capsule.style.height = `${open ? THICKNESS : 0}px`;
  });
  const b = bubble();
  root.replaceChildren(...(b ? [b] : []), capsule);
  aria.setState(state.phase === "thinking" ? "thinking" : "idle");
  wake();
  // Where the mouse is taken, once the sizes have settled.
  setTimeout(reportHot, 60);
  setTimeout(reportHot, 420);
}

function reportHot() {
  const rects: [number, number, number, number][] = [];
  const W = window.innerWidth;
  const H = window.innerHeight;
  if (look() === "handsFree" || look() === "live") rects.push([(W - 200) / 2, H - 46, 200, 46]);
  const b = root.querySelector<HTMLElement>(".bubble");
  if (b) {
    const r = b.getBoundingClientRect();
    rects.push([r.left - 6, r.top - 6, r.width + 12, r.height + 12]);
  }
  void FlowBridge.pillHot(rects);
}

function setHovered(inside: boolean) {
  if (inside === hovered) return;
  hovered = inside;
  void FlowBridge.pillHover(inside);
}

document.addEventListener("pointermove", (e) => {
  const target = e.target as HTMLElement | null;
  setHovered(Boolean(capsule?.contains(target) || target?.closest?.(".bubble")));
});
document.addEventListener("pointerleave", () => setHovered(false));

// ── The animation loop (only while the pill shows) ────────────────────────────

let frame: number | null = null;
let last = performance.now();

function dotGoal(now: number, i: number): number {
  if (state.phase === "thinking") return 0.25 + 0.25 * Math.sin(now / 160 - i * 0.7);
  if (state.phase === "listening" || state.phase === "locked") return target[i];
  return 0;
}

function tick(now: number) {
  frame = null;
  const dt = Math.min(0.05, (now - last) / 1000);
  last = now;
  for (let i = 0; i < BANDS; i++) {
    levels[i] += (dotGoal(now, i) - levels[i]) * Math.min(1, dt * 14);
    const bar = barsEl.children[i] as HTMLElement;
    bar.style.height = `${3.4 + 16 * Math.max(0, levels[i])}px`;
  }
  // Her eyes wander while you speak.
  aria.lookX = Math.sin(now / 900) * 0.6;
  aria.lookY = Math.cos(now / 1300) * 0.3;
  aria.update(dt);
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  const size = 22 / 0.6;
  if (ariaCanvas.width !== Math.round(size * dpr)) {
    ariaCanvas.width = Math.round(size * dpr);
    ariaCanvas.height = Math.round(size * dpr);
  }
  const ctx = ariaCanvas.getContext("2d");
  if (ctx) {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, size, size);
    aria.draw(ctx, size, size);
  }
  if (visible && look() !== "hidden") frame = requestAnimationFrame(tick);
}

function wake() {
  if (frame === null && visible) {
    last = performance.now();
    frame = requestAnimationFrame(tick);
  }
}

// ── Boot ──────────────────────────────────────────────────────────────────────

async function main() {
  const boot = await Bridge.boot();
  setLanguage(resolveLanguage(boot?.settings.language, systemLanguages()));
  state = (await FlowBridge.pillState()) ?? state;
  render();
  void onEvent<PillState>("pill-state", (s) => {
    state = s;
    if (s.phase === "listening") target.fill(0);
    render();
  });
  void onEvent<number[]>("pill-bands", (bands) => {
    for (let i = 0; i < BANDS; i++) target[i] = bands[i] ?? 0;
    wake();
  });
  void onEvent<boolean>("pill-visible", (v) => {
    visible = v;
    if (!v) setHovered(false);
    render();
  });
  // The cursor poll tells when the pointer leaves the pill.
  void onEvent<boolean>("pill-pointer", (inside) => {
    if (!inside) setHovered(false);
  });
  void onEvent<Settings>("settings-changed", (s) => {
    setLanguage(resolveLanguage(s.language, systemLanguages()));
    render();
  });
}

void main();

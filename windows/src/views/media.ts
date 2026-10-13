// The music pill: its card in the overview and its two buttons in the row.
// Everything comes from the system media controls (media.rs), so it works
// with Spotify, a browser tab or any player Windows lists. Each button is one
// click on the player. The look is Coucou's Spotify card and pill
// (SpotifyViews.swift, NowPlayingViews.swift): sizes and colours are the Mac's.

import { h, svg } from "./dom";
import { MEDIA_ID, State, mediaPosition, type AgentTask, type MediaInfo } from "../core/state";
import { Bridge } from "../core/bridge";
import { t } from "../i18n/i18n";

const MEDIA_ICONS = {
  play: "M7 4.6c0-.8.9-1.3 1.6-.9l11 6.9c.6.4.6 1.4 0 1.8l-11 6.9c-.7.4-1.6-.1-1.6-.9V4.6z",
  pause: "M6 4.5h4v15H6zM14 4.5h4v15h-4z",
  next: "M2.5 6.2c0-.7.8-1.1 1.4-.7l7.6 5.8c.5.4.5 1 0 1.4l-7.6 5.8c-.6.4-1.4 0-1.4-.7V6.2zm10 0c0-.7.8-1.1 1.4-.7l7.6 5.8c.5.4.5 1 0 1.4l-7.6 5.8c-.6.4-1.4 0-1.4-.7V6.2z",
  prev: "M21.5 6.2c0-.7-.8-1.1-1.4-.7l-7.6 5.8c-.5.4-.5 1 0 1.4l7.6 5.8c.6.4 1.4 0 1.4-.7V6.2zm-10 0c0-.7-.8-1.1-1.4-.7l-7.6 5.8c-.5.4-.5 1 0 1.4l7.6 5.8c.6.4 1.4 0 1.4-.7V6.2z",
  // Stroked (svg(…, { stroke })).
  shuffle: "M3 7h3.2c1.9 0 3.1.9 4.1 2.5l3.4 5c1 1.6 2.2 2.5 4.1 2.5H21M3 17h3.2c1.6 0 2.7-.6 3.6-1.8M14.2 8.8C15.1 7.6 16.2 7 17.8 7H21M18.6 4.6 21 7l-2.4 2.4M18.6 14.6 21 17l-2.4 2.4",
  repeat: "M4 11.5V10a3 3 0 0 1 3-3h13.5M17.6 4.1 20.5 7l-2.9 2.9M20 12.5V14a3 3 0 0 1-3 3H3.5M6.4 19.9 3.5 17l2.9-2.9",
  note: "M12.2 3.6h1.7c.3 2.3 1.8 3.5 4.2 4.6l-.6 1.6c-1.4-.5-2.7-1.2-3.6-2.1v9.5a3.3 3.3 0 1 1-1.7-2.88V3.6z",
  volume: [
    "M3.5 9.5h3L11 5.8v12.4l-4.5-3.7h-3zM15 9.5l5 5M20 9.5l-5 5",
    "M3.5 9.5h3L11 5.8v12.4l-4.5-3.7h-3zM14.5 9.6a3.4 3.4 0 0 1 0 4.8",
    "M3.5 9.5h3L11 5.8v12.4l-4.5-3.7h-3zM14.5 9.6a3.4 3.4 0 0 1 0 4.8M17 7.2a6.8 6.8 0 0 1 0 9.6",
    "M3.5 9.5h3L11 5.8v12.4l-4.5-3.7h-3zM14.5 9.6a3.4 3.4 0 0 1 0 4.8M17 7.2a6.8 6.8 0 0 1 0 9.6M19.4 4.8a10.2 10.2 0 0 1 0 14.4",
  ],
} as const;

/** NowPlayingProgress.format: m:ss, or h:mm:ss past an hour. */
export function clock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

/** NowPlayingVolume.icon: 0 = muted, then one, two or three waves (volume 0…1). */
export function volumeLevel(volume: number): 0 | 1 | 2 | 3 {
  if (volume <= 0) return 0;
  if (volume < 0.34) return 1;
  if (volume < 0.67) return 2;
  return 3;
}

function lighter(hex: string, amount: number): string {
  const v = parseInt(hex.replace("#", ""), 16);
  const c = [(v >> 16) & 255, (v >> 8) & 255, v & 255].map((x) => Math.min(255, Math.round(x + amount * 255)));
  return `rgb(${c[0]},${c[1]},${c[2]})`;
}

function control(action: Parameters<typeof Bridge.mediaControl>[0], value?: number) {
  return (e: Event) => {
    e.stopPropagation();
    void Bridge.mediaControl(action, value);
  };
}

// ── The moving bar ────────────────────────────────────────────────────────────
// It moves only while the island is open on the music card and the song plays:
// checked on every state change, so a folded island stops it.

let paint: (() => void) | null = null;
let timer: number | null = null;

function syncMusicTimer() {
  const want = paint != null && State.mode === "expanded" && State.view === "overview" &&
    State.focusId === MEDIA_ID && !!State.media?.playing;
  if (want && timer == null) {
    timer = window.setInterval(() => paint?.(), 500);
  } else if (!want && timer != null) {
    window.clearInterval(timer);
    timer = null;
  }
}

State.subscribe(syncMusicTimer);

// ── Slider (NowPlayingBar) ────────────────────────────────────────────────────

/**
 * A grey track whose filled part takes the pill's colour and grows a knob
 * under the pointer; drag or click anywhere on it.
 */
function slider(color: string, enabled: boolean, onChange: (f: number) => void, onCommit: (f: number) => void) {
  const fill = h("i", { class: "np-fill" });
  const knob = h("i", { class: "np-knob" });
  const el = h("div", { class: enabled ? "np-bar" : "np-bar off" }, h("i", { class: "np-track" }), fill, knob);
  let hovering = false;
  let dragging = false;
  let fraction = 0;
  const at = (clientX: number) => {
    const r = el.getBoundingClientRect();
    return Math.min(1, Math.max(0, (clientX - r.left) / Math.max(1, r.width)));
  };
  const draw = () => {
    const active = enabled && (hovering || dragging);
    el.classList.toggle("active", active);
    const pct = `${fraction * 100}%`;
    fill.style.width = pct;
    fill.style.background = active ? color : "#C5C8CD";
    knob.style.left = `clamp(0px, calc(${pct} - 4.5px), calc(100% - 9px))`;
  };
  el.addEventListener("mouseenter", () => { hovering = true; draw(); });
  el.addEventListener("mouseleave", () => { hovering = false; draw(); });
  el.addEventListener("click", (e) => e.stopPropagation());
  el.addEventListener("pointerdown", (e) => {
    if (!enabled || e.button !== 0) return;
    e.stopPropagation();
    dragging = true;
    try {
      el.setPointerCapture(e.pointerId);
    } catch {
      /* capture is a nicety */
    }
    fraction = at(e.clientX);
    onChange(fraction);
    draw();
  });
  el.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    fraction = at(e.clientX);
    onChange(fraction);
    draw();
  });
  const end = (e: PointerEvent) => {
    if (!dragging) return;
    dragging = false;
    fraction = at(e.clientX);
    onCommit(fraction);
    draw();
  };
  el.addEventListener("pointerup", end);
  el.addEventListener("pointercancel", end);
  return {
    el,
    get dragging() { return dragging; },
    set(f: number) {
      if (dragging) return;
      fraction = Math.min(1, Math.max(0, f));
      draw();
    },
  };
}

// ── Card ──────────────────────────────────────────────────────────────────────

/** What makes the card redraw (the bar moves by itself in between). */
export function musicCardKey(): string {
  const m = State.media;
  if (!m) return "";
  return [m.title, m.artist, m.album, m.app, m.playing, m.positionMs, m.atMs, m.durationMs, m.shuffle, m.repeat,
    m.canPrev, m.canNext, m.canSeek, m.canShuffle, m.canRepeat, m.cover?.length ?? 0,
    m.volume == null ? "" : Math.round(m.volume * 100)].join("|");
}

export function musicCard(task: AgentTask): HTMLElement {
  const m = State.media;
  if (!m?.active) {
    paint = null;
    return h("div", { class: "int-card" },
      h("div", { class: "int-status" }, h("span", { text: t("Nothing is playing") })));
  }
  const color = task.color;
  const dim = "#6B7079";

  // Artwork row: the cover fades in over the note.
  const art = h("div", { class: m.cover ? "np-art has-art" : "np-art", title: m.app || task.name },
    svg(MEDIA_ICONS.note, 16));
  (art.firstChild as SVGElement).style.color = `${color}b3`;
  if (m.cover) art.append(h("img", { src: m.cover, alt: "", draggable: "false" }));
  const sub = [m.artist, m.album].filter(Boolean).join(" · ") || m.app || task.name;
  const head = h("div", { class: "np-head" },
    art,
    h("div", { class: "np-text" },
      h("div", { class: "np-title-row" },
        h("i", { class: "np-dot", style: `background:${m.playing ? color : dim}` }),
        h("span", { class: "np-title", text: m.title || t("Unknown song"), title: m.title })),
      h("div", { class: "np-sub", text: sub, title: sub })));

  // Progress row: elapsed, the bar, what is left.
  const elapsed = h("span", { class: "np-time elapsed" });
  const remaining = h("span", { class: "np-time remaining" });
  let dragFraction: number | null = null;
  const draw = () => {
    const d = m.durationMs;
    const pos = dragFraction != null ? dragFraction * d : mediaPosition(m);
    const f = d > 0 ? Math.min(1, Math.max(0, pos / d)) : 0;
    progress.set(f);
    elapsed.textContent = d > 0 || pos > 0 ? clock(pos) : "";
    remaining.textContent = d > 0 ? `-${clock(Math.max(0, d - pos))}` : "";
  };
  const progress = slider(color, m.canSeek && m.durationMs > 0,
    (f) => { dragFraction = f; draw(); },
    (f) => {
      dragFraction = null;
      void Bridge.mediaControl("seek", Math.round(f * m.durationMs));
    });
  if (m.canSeek) progress.el.title = t("Jump to this point");
  draw();
  paint = () => {
    if (!progress.el.isConnected || progress.dragging) return;
    draw();
  };
  const progressRow = h("div", { class: "np-progress" }, elapsed, progress.el, remaining);

  // Controls row: shuffle, previous, play, next, repeat — and the volume.
  const icon = (path: string, title: string, onclick: (e: Event) => void, enabled: boolean, stroke = 0, size = 11) =>
    h("button", { class: "np-icon", title, disabled: !enabled, onclick }, svg(path, size, stroke ? { stroke } : {}));
  const shuffle = icon(MEDIA_ICONS.shuffle, t("Shuffle"), control("shuffle"), m.canShuffle, 2.2, 10);
  shuffle.style.color = m.shuffle ? color : dim;
  const repeatOn = !!m.repeat && m.repeat !== "none";
  const repeatTitle = m.repeat === "track" ? t("Repeat this song") : m.repeat === "list" ? t("Repeat all") : t("Repeat");
  const repeat = icon(MEDIA_ICONS.repeat, repeatTitle, control("repeat"), m.canRepeat, 2.2, 10);
  repeat.style.color = repeatOn ? color : dim;
  if (m.repeat === "track") repeat.append(h("span", { class: "np-one", text: "1" }));
  const prev = icon(MEDIA_ICONS.prev, t("Previous"), control("prev"), m.canPrev);
  const next = icon(MEDIA_ICONS.next, t("Next"), control("next"), m.canNext);
  prev.style.color = "#C5C8CD";
  next.style.color = "#C5C8CD";
  const playIcon = svg(m.playing ? MEDIA_ICONS.pause : MEDIA_ICONS.play, 9);
  if (!m.playing) playIcon.style.transform = "translateX(1px)";
  const play = h("button", { class: "np-play", title: m.playing ? t("Pause") : t("Play"), onclick: control("toggle") }, playIcon);

  const controls = h("div", { class: "np-controls" },
    h("div", { class: "np-buttons" }, shuffle, prev, play, next, repeat),
    h("div", { class: "grow" }));

  if (m.volume != null) {
    const volIcon = h("span", { class: "np-vol-icon" });
    const showLevel = (v: number) => {
      volIcon.replaceChildren(svg(MEDIA_ICONS.volume[volumeLevel(v)], 10, { stroke: 2.2 }));
      volumeBox.title = `${t("Volume")} ${Math.round(v * 100)}%`;
    };
    // A drag sends at most one volume every 120 ms (SpotifyController.setVolume).
    let pending: number | null = null;
    let wait: number | null = null;
    const send = () => {
      const v = pending;
      pending = null;
      if (v == null) {
        wait = null;
        return;
      }
      void Bridge.mediaControl("volume", v);
      wait = window.setTimeout(send, 120);
    };
    const setVolume = (f: number) => {
      showLevel(f);
      pending = f;
      if (wait == null) send();
    };
    const volume = slider(color, true, setVolume, setVolume);
    const volumeBox = h("div", { class: "np-volume" }, volIcon, volume.el);
    volume.set(m.volume);
    showLevel(m.volume);
    controls.append(volumeBox);
  }

  return h("div", { class: "int-card np-card" }, head, progressRow, controls);
}

// ── Pill ──────────────────────────────────────────────────────────────────────

/**
 * Play/pause and next on the pill, shown while the pointer is over it
 * (SpotifyPill, MusicControlButton): round buttons in the pill's colour
 * (musicPillColors).
 */
export function musicPillButtons(m: MediaInfo | null): HTMLElement {
  const playing = !!m?.playing;
  const button = (title: string, icon: string, onclick: (e: Event) => void, enabled: boolean) =>
    h("button", { class: "np-pill-btn", title, disabled: !enabled, onclick }, svg(icon, 8));
  return h("span", { class: "np-pill-controls" },
    button(playing ? t("Pause") : t("Play"), playing ? MEDIA_ICONS.pause : MEDIA_ICONS.play, control("toggle"), true),
    button(t("Next"), MEDIA_ICONS.next, control("next"), !!m?.canNext));
}

/**
 * The pill glows in its colour under the pointer, like the Mac's SpotifyPill.
 * The hover itself is CSS (.np-pill:hover): a pill rebuilt under the pointer
 * keeps its buttons.
 */
export function musicPillColors(pill: HTMLElement, color: string) {
  pill.style.setProperty("--np-soft", `${color}24`);
  pill.style.setProperty("--np-wash", `${color}2e`);
  pill.style.setProperty("--np-edge", `${color}8c`);
  pill.style.setProperty("--np-glow", `${color}59`);
  pill.style.setProperty("--np-light", lighter(color, 0.3));
}

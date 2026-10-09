// The music pill: its card in the overview and its two buttons in the row.
// Everything comes from the system media controls (media.rs), so it works
// with Spotify, a browser tab or any player Windows lists. Each button is one
// click on the player.

import { h, svg } from "./dom";
import { ICONS } from "./icons";
import { Bridge } from "../core/bridge";
import { MEDIA_ID, State, mediaPosition, type AgentTask, type MediaInfo } from "../core/state";
import { t } from "../i18n/i18n";

const MEDIA_ICONS = {
  play: "M8 5.6v12.8a1 1 0 0 0 1.52.85l10.2-6.4a1 1 0 0 0 0-1.7L9.52 4.75A1 1 0 0 0 8 5.6z",
  pause: "M6.5 5h4v14h-4zM13.5 5h4v14h-4z",
  next: "M4.5 6.3v11.4a.9.9 0 0 0 1.4.74l8.2-5.7a.9.9 0 0 0 0-1.48L5.9 5.56a.9.9 0 0 0-1.4.74zM16.5 5h3v14h-3z",
  prev: "M19.5 6.3v11.4a.9.9 0 0 1-1.4.74l-8.2-5.7a.9.9 0 0 1 0-1.48l8.2-5.7a.9.9 0 0 1 1.4.74zM4.5 5h3v14h-3z",
  // Stroked (svg(…, { stroke })).
  shuffle: "M3 7h3c2.2 0 3.4 1.1 4.6 3l2.8 4c1.2 1.9 2.4 3 4.6 3h3M3 17h3c1.3 0 2.2-.4 3-1.1M14.4 8.1c.8-.7 1.7-1.1 3-1.1h3.6M18 4l3 3-3 3M18 14l3 3-3 3",
  repeat: "M4 12V9.5A3.5 3.5 0 0 1 7.5 6H20M17 3l3 3-3 3M20 12v2.5a3.5 3.5 0 0 1-3.5 3.5H4M7 21l-3-3 3-3",
  note: "M9 18V6l11-2v12M9 18a3 3 0 1 1-3-3 3 3 0 0 1 3 3zm11-2a3 3 0 1 1-3-3 3 3 0 0 1 3 3z",
} as const;

function clock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
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

  const cover = m.cover
    ? h("img", { class: "music-cover", src: m.cover, alt: "" })
    : h("div", { class: "music-cover empty", style: `color:${color}` }, svg(MEDIA_ICONS.note, 18, { stroke: 1.8 }));
  const sub = [m.artist, m.album].filter(Boolean).join(" · ") || task.name;
  const head = h("div", { class: "music-head" },
    cover,
    h("div", { class: "music-meta" },
      h("div", { class: "music-title" },
        h("i", { class: "music-dot", style: `background:${m.playing ? "#22C55E" : "#6B7079"}` }),
        h("b", { text: m.title || t("Unknown song"), title: m.title })),
      h("span", { class: "music-sub", text: sub, title: sub })));

  const fill = h("i", { class: "music-fill", style: `background:${color}` });
  const knob = h("i", { class: "music-knob" });
  const bar = h("div", { class: m.canSeek ? "music-bar seek" : "music-bar" }, fill, knob);
  const now = h("span", { class: "music-time" });
  const total = h("span", { class: "music-time", text: m.durationMs > 0 ? clock(m.durationMs) : "" });
  const draw = () => {
    const pos = mediaPosition(m);
    now.textContent = m.durationMs > 0 || pos > 0 ? clock(pos) : "";
    const pct = m.durationMs > 0 ? Math.min(100, (pos / m.durationMs) * 100) : 0;
    fill.style.width = `${pct}%`;
    knob.style.left = `${pct}%`;
  };
  draw();
  paint = () => {
    if (!bar.isConnected) return;
    draw();
  };
  if (m.canSeek) {
    bar.title = t("Jump to this point");
    bar.addEventListener("click", (e) => {
      e.stopPropagation();
      const r = bar.getBoundingClientRect();
      const ratio = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
      void Bridge.mediaControl("seek", Math.round(ratio * m.durationMs));
    });
  }
  const progress = h("div", { class: "music-progress" }, now, bar, total);

  const btn = (cls: string, icon: string, title: string, onclick: (e: Event) => void, enabled: boolean, stroke = 0) =>
    h("button", { class: `music-btn ${cls}`, title, disabled: !enabled, onclick }, svg(icon, cls === "play" ? 14 : 13, stroke ? { stroke } : {}));

  const repeatTitle = m.repeat === "track" ? t("Repeat this song") : m.repeat === "list" ? t("Repeat all") : t("Repeat");
  const repeat = btn(m.repeat && m.repeat !== "none" ? "on" : "", MEDIA_ICONS.repeat, repeatTitle, control("repeat"), m.canRepeat, 2);
  if (m.repeat === "track") repeat.append(h("span", { class: "music-one", text: "1" }));
  repeat.style.setProperty("--on", color);
  const shuffle = btn(m.shuffle ? "on" : "", MEDIA_ICONS.shuffle, t("Shuffle"), control("shuffle"), m.canShuffle, 2);
  shuffle.style.setProperty("--on", color);

  const controls = h("div", { class: "music-controls" },
    shuffle,
    btn("", MEDIA_ICONS.prev, t("Previous"), control("prev"), m.canPrev),
    btn("play", m.playing ? MEDIA_ICONS.pause : MEDIA_ICONS.play, m.playing ? t("Pause") : t("Play"), control("toggle"), true),
    btn("", MEDIA_ICONS.next, t("Next"), control("next"), m.canNext),
    repeat,
  );

  if (m.volume != null) {
    const slider = h("input", {
      class: "music-volume", type: "range", min: 0, max: 100, step: 1,
      value: Math.round(m.volume * 100), title: t("Volume"),
      style: `accent-color:${color}`,
    });
    let pending: number | null = null;
    slider.addEventListener("input", () => {
      // At most one call per frame while dragging.
      if (pending != null) return;
      pending = requestAnimationFrame(() => {
        pending = null;
        void Bridge.mediaControl("volume", Number(slider.value) / 100);
      });
    });
    slider.addEventListener("click", (e) => e.stopPropagation());
    controls.append(h("span", { class: "music-vol" }, svg(ICONS.speakerOn, 12), slider));
  }

  return h("div", { class: "int-card music-card" }, head, progress, controls);
}

// ── Pill ──────────────────────────────────────────────────────────────────────

/** Play/pause and next, right on the pill. */
export function musicPillButtons(m: MediaInfo | null): HTMLElement {
  const playing = !!m?.playing;
  return h("span", { class: "pill-music-btns" },
    h("button", { class: "pill-music-btn", title: playing ? t("Pause") : t("Play"), onclick: control("toggle") },
      svg(playing ? MEDIA_ICONS.pause : MEDIA_ICONS.play, 9)),
    h("button", { class: "pill-music-btn", title: t("Next"), disabled: !m?.canNext, onclick: control("next") },
      svg(MEDIA_ICONS.next, 9)));
}

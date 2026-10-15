// Settings → Voice: "Hey ARIA" (voice/listen.rs). Off by default; turning it
// on downloads the models once (progress on `voice-engine`) and keeps the
// microphone open — the one thing ARIA does while hidden, said here plainly.

import { Bridge, onEvent, type VoiceStatus } from "../core/bridge";
import type { Settings } from "../core/state";
import { h, clear } from "../views/dom";
import { t } from "../i18n/i18n";

interface EngineEvent {
  engine: string;
  ready: boolean;
  downloading?: boolean;
  pct?: number;
  item?: string;
  error?: string;
}

function stateText(s: VoiceStatus): string {
  switch (s.state) {
    case "off": return t("Off");
    case "preparing": return t("Getting ready…");
    case "idle": return t("Listening for “Hey ARIA”");
    case "listening": return t("Listening…");
    case "thinking": return t("Thinking…");
    case "speaking": return t("Speaking…");
    case "paused": return t("Paused");
    case "error": return t("Stopped: {error}", { error: s.error ?? "" });
  }
}

let listening = false;
/** The section on screen; the events above are listened to once. */
let shown: { refresh(): void; engine(e: EngineEvent): void } | null = null;

export function voiceSection(ctx: { get: () => Settings; save: () => Promise<void> }): HTMLElement {
  const s0 = ctx.get();
  const status = h("span", { class: "hint" });
  const progress = h("div", { class: "hint" });
  const history = h("div", { style: "display:flex;flex-direction:column;gap:4px" });
  const resume = h("button", { text: t("Resume listening"), onclick: () => void Bridge.voiceResume().then(refresh) });
  resume.style.display = "none";

  const voice = h("select", {}) as HTMLSelectElement;
  voice.append(h("option", { value: "piper", text: t("ARIA (Mexican Spanish, downloaded)") }));
  void Bridge.voiceVoices().then((names) => {
    for (const name of names ?? []) voice.append(h("option", { value: name, text: name }));
    voice.value = ctx.get().voiceName;
    if (!voice.value) voice.value = "piper";
  });
  voice.value = s0.voiceName;
  voice.addEventListener("change", () => {
    ctx.get().voiceName = voice.value;
    void ctx.save();
  });
  const tryVoice = h("button", { text: t("Try"), onclick: () => void Bridge.voiceTry(voice.value) });

  function toggle(on: boolean, onChange: (v: boolean) => void): HTMLElement {
    const el = h("button", { class: on ? "switch on" : "switch", "aria-pressed": on });
    el.addEventListener("click", () => {
      const next = !el.classList.contains("on");
      el.classList.toggle("on", next);
      onChange(next);
    });
    return el;
  }

  async function refresh() {
    const s = await Bridge.voiceStatus();
    if (!s) return;
    status.textContent = stateText(s);
    resume.style.display = s.state === "paused" ? "" : "none";
    clear(history);
    if (s.history.length === 0) {
      history.append(h("span", { class: "hint", text: t("Nothing heard yet.") }));
    }
    for (const item of s.history) {
      const at = new Date(item.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
      history.append(h("div", { class: "row" }, h("span", { class: "hint", text: at }), h("span", { text: item.text })));
    }
  }

  const sttStatus = h("span", { class: "hint" });
  const sttProgress = h("div", { class: "hint" });
  const sttButton = h("button", {}) as HTMLButtonElement;
  let sttReady = false;
  async function refreshStt() {
    const s = await Bridge.voiceParakeetStatus();
    if (!s) return;
    sttReady = s.ready;
    sttStatus.textContent = s.ready
      ? t("Downloaded: dictation, meetings and “Hey ARIA” use it")
      : t("Not downloaded: Whisper is used ({size} MB)", { size: s.sizeMb });
    sttButton.textContent = s.ready ? t("Remove") : t("Download");
    sttButton.disabled = false;
  }
  sttButton.addEventListener("click", () => {
    sttButton.disabled = true;
    if (sttReady) {
      void Bridge.voiceParakeetRemove().then(refreshStt);
      return;
    }
    Bridge.voiceParakeetDownload()
      .catch((e) => { sttProgress.textContent = t("Download: {error}", { error: String(e) }); })
      .finally(() => void refreshStt());
  });
  void refreshStt();

  shown = {
    refresh: () => void refresh(),
    engine: (e) => {
      if (e.engine === "parakeet") {
        if (e.error) sttProgress.textContent = t("Download: {error}", { error: e.error });
        else if (e.downloading) sttProgress.textContent = t("Downloading {item}… {pct}%", { item: e.item ?? "", pct: e.pct ?? 0 });
        else sttProgress.textContent = "";
        return;
      }
      if (e.error) progress.textContent = t("Download: {error}", { error: e.error });
      else if (e.downloading) progress.textContent = t("Downloading {item}… {pct}%", { item: e.item ?? "", pct: e.pct ?? 0 });
      else progress.textContent = "";
    },
  };
  if (!listening) {
    listening = true;
    void onEvent("voice-state", () => shown?.refresh());
    void onEvent<EngineEvent>("voice-engine", (e) => shown?.engine(e));
  }
  void refresh();

  return h("section", {},
    h("h2", {}, h("span", { text: t("Voice") })),
    h("div", { class: "row" },
      h("label", { text: t("“Hey ARIA”") }),
      toggle(s0.voiceWake, (v) => { ctx.get().voiceWake = v; void ctx.save(); }),
      status,
    ),
    h("div", {
      class: "notice warn",
      text: t("While this is on, the microphone stays open and ARIA uses a little CPU even when the island is hidden. Everything runs on this PC: no audio leaves it or is saved. The first time, about 100 MB of voice models are downloaded."),
    }),
    progress,
    h("div", { class: "hint", text: t("Say “Hey ARIA” or “Oye ARIA”, then what you need. Say “Pausa” to stop listening, “Gracias” to end. Anything with side effects still asks for your click.") }),
    h("div", { class: "row" }, h("label", {}), resume),
    h("div", { class: "row" },
      h("label", { text: t("Answer out loud") }),
      toggle(s0.voiceSpeak, (v) => { ctx.get().voiceSpeak = v; void ctx.save(); }),
    ),
    h("div", { class: "row" }, h("label", { text: t("Her voice") }), voice, tryVoice),
    h("div", { class: "row" },
      h("label", { text: t("Subtitles") }),
      toggle(s0.voiceSubtitles, (v) => { ctx.get().voiceSubtitles = v; void ctx.save(); }),
    ),
    h("div", { class: "subhead", text: t("Speech recognition") }),
    h("div", { class: "row" }, h("label", { text: t("Parakeet") }), sttButton, sttStatus),
    h("div", {
      class: "hint",
      text: t("NVIDIA's Parakeet understands the name ARIA and 25 languages, offline and much faster than Whisper on this PC."),
    }),
    sttProgress,
    h("div", { class: "subhead", text: t("Heard recently (kept in memory only)") }),
    history,
    h("div", { class: "row" },
      h("button", { text: t("Clear"), onclick: () => void Bridge.voiceClearHistory().then(refresh) }),
    ),
  );
}

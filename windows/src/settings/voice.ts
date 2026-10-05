// Ajustes → Voz: a voice per Grok Bot (Piper or a cloud engine), listen,
// install, read replies aloud, and the cloud keys. The commands are Aegon's and
// may not exist yet: every call says "no disponible todavía" then. The keys go
// straight to Rust (set_tts_key) and the field is emptied at once: they are
// never kept here, in localStorage or in a log line.

import { onEvent } from "../core/bridge";
import {
  CMD, EVT, callCmd, mirrorSpeakCursor, readRepliesEnabled, setReadReplies, speakCursorEnabled,
  type VoiceEngineEvent, type VoiceInfo,
} from "../core/botcmds";
import type { GrokBot, Settings } from "../core/state";
import { h, clear } from "../views/dom";

const errText = (err: unknown) => (err instanceof Error ? err.message : String(err));
const notice = (kind: "ok" | "err" | "warn", text: string) => h("div", { class: `notice ${kind}`, text });


/** The Cursor agent's row: same as a Bot's, with id "cursor". */
const CURSOR_VOICE = { id: "cursor", name: "Agente de Cursor", color: "#e5e7eb" };

const ENGINE_LABELS: Record<string, string> = {
  piper: "Piper (en el PC)",
  elevenlabs: "ElevenLabs",
  azure: "Azure",
};

/** `voices`: bot id → voice id, from settings (Rust writes it in set_bot_voice). */
export function voiceSection(
  bots: GrokBot[],
  voices0: Record<string, string>,
  ctx: { get: () => Settings; save: () => Promise<void> },
): HTMLElement {
  // Voice ids are used whole (`piper:es_MX-claude-high`), never split on ":".
  const chosen = (bot: string) => voices0[bot] ?? "";
  const feedback = h("div");
  const engineLine = h("div", { class: "hint", text: "Motor de voz: esperando estado…" });
  const progress = h("div", { class: "voice-progress" }, h("i"));
  progress.style.display = "none";
  const botRows = h("div", { class: "voice-bots" });
  let voices: VoiceInfo[] = [];

  const readToggle = h("button", { class: readRepliesEnabled() ? "switch on" : "switch" });
  readToggle.addEventListener("click", () => {
    const on = !readToggle.classList.contains("on");
    readToggle.classList.toggle("on", on);
    setReadReplies(on);
  });

  function say(kind: "ok" | "err" | "warn", text: string) {
    clear(feedback);
    feedback.append(notice(kind, text));
  }

  function grouped(select: HTMLSelectElement, selected: string) {
    clear(select);
    select.append(h("option", { value: "", text: voices.length ? "Elige una voz…" : "Sin voces (no disponible todavía)" }));
    const engines = [...new Set(voices.map((v) => v.engine))];
    for (const engine of engines) {
      const group = h("optgroup", { label: ENGINE_LABELS[engine] ?? engine }) as HTMLOptGroupElement;
      for (const v of voices.filter((x) => x.engine === engine)) {
        group.append(h("option", {
          value: v.id,
          text: `${v.name} · ${v.lang}${v.installed ? "" : v.sizeMb ? ` · ${v.sizeMb} MB sin instalar` : " · sin instalar"}`,
        }));
      }
      select.append(group);
    }
    select.value = voices.some((v) => v.id === selected) ? selected : "";
  }

  function botRow(bot: Pick<GrokBot, "id" | "name" | "color">): HTMLElement {
    const select = h("select", { style: "flex:1 1 auto;min-width:0" }) as HTMLSelectElement;
    const listen = h("button", { text: "Escuchar" });
    const install = h("button", { class: "primary", text: "Instalar" });
    const refresh = () => {
      const v = voices.find((x) => x.id === select.value);
      listen.toggleAttribute("disabled", !v || !v.installed);
      install.style.display = v && !v.installed ? "" : "none";
      install.textContent = v?.sizeMb ? `Instalar (${v.sizeMb} MB)` : "Instalar";
    };
    grouped(select, chosen(bot.id));
    refresh();
    select.addEventListener("change", async () => {
      refresh();
      const voiceId = select.value;
      if (!voiceId) return;
      try {
        await callCmd(CMD.setBotVoice, { bot: bot.id, voiceId });
        voices0[bot.id] = voiceId;
        say("ok", `Voz de ${bot.name} guardada.`);
      } catch (err) {
        say("err", `Voz de ${bot.name}: ${errText(err)}`);
      }
    });
    listen.addEventListener("click", async () => {
      try {
        await callCmd(CMD.previewVoice, { voiceId: select.value, text: `Hola, soy ${bot.name}.` });
      } catch (err) {
        say("err", `Escuchar: ${errText(err)}`);
      }
    });
    install.addEventListener("click", async () => {
      install.setAttribute("disabled", "");
      say("ok", "Instalando la voz… el avance aparece arriba. Si se corta la conexión, sigue sola al volver.");
      try {
        await callCmd(CMD.installVoice, { voiceId: select.value });
        say("ok", `Voz de ${bot.name} instalada.`);
        await loadVoices();
      } catch (err) {
        say("err", `Instalar: ${errText(err)}`);
      } finally {
        install.removeAttribute("disabled");
      }
    });
    return h("div", { class: "row" },
      h("label", {}, h("i", { class: "dot", style: `background:${bot.color}` }), h("span", { text: ` ${bot.name}`, style: `color:${bot.color}` })),
      select, listen, install);
  }

  function drawBots() {
    clear(botRows);
    // The Cursor agent speaks too (set_bot_voice with bot "cursor").
    botRows.append(botRow(CURSOR_VOICE));
    if (bots.length === 0) {
      botRows.append(h("div", { class: "hint", text: "Conecta un Bot de Grok para elegir su voz." }));
      return;
    }
    for (const b of bots) botRows.append(botRow(b));
  }

  const speakCursor = h("button", { class: speakCursorEnabled(ctx.get().speakCursor) ? "switch on" : "switch" });
  speakCursor.addEventListener("click", () => {
    const on = !speakCursor.classList.contains("on");
    speakCursor.classList.toggle("on", on);
    ctx.get().speakCursor = on;
    mirrorSpeakCursor(on);
    void ctx.save();
  });

  async function loadVoices() {
    try {
      voices = (await callCmd<VoiceInfo[]>(CMD.listVoices)) ?? [];
    } catch (err) {
      voices = [];
      engineLine.textContent = `Voces: ${errText(err)}`;
    }
    drawBots();
  }

  // ── Cloud keys ──
  const keyStatus: Record<"elevenlabs" | "azure", HTMLElement> = {
    elevenlabs: h("span", { class: "hint" }),
    azure: h("span", { class: "hint" }),
  };
  async function loadKeyStatus() {
    try {
      const st = await callCmd<{ elevenlabs?: boolean; azure?: boolean }>(CMD.ttsKeyStatus);
      for (const k of ["elevenlabs", "azure"] as const) keyStatus[k].textContent = st?.[k] ? "configurada" : "sin configurar";
    } catch (err) {
      for (const k of ["elevenlabs", "azure"] as const) keyStatus[k].textContent = errText(err);
    }
  }
  function keyRow(engine: "elevenlabs" | "azure", label: string): HTMLElement {
    const field = h("input", {
      type: "password", placeholder: engine === "azure" ? "región:clave (ej. eastus:xxxx)" : "Pega la clave",
      autocomplete: "off", spellcheck: "false",
      style: "flex:1 1 auto;min-width:0",
    }) as HTMLInputElement;
    const save = h("button", { class: "primary", text: "Guardar" });
    save.addEventListener("click", async () => {
      const key = field.value.trim();
      // Out of the field before anything else: it lives only in this call.
      field.value = "";
      if (!key) return;
      // Azure needs its region in front: "eastus:xxxx".
      if (engine === "azure" && !/^[^:\s]+:\S+/.test(key)) {
        say("err", "La clave de Azure va como región:clave (ej. eastus:xxxx).");
        return;
      }
      try {
        await callCmd(CMD.setTtsKey, { engine, key });
        say("ok", `Clave de ${label} guardada.`);
      } catch (err) {
        // Never echo the key or the raw error back.
        say("err", "No se pudo guardar la clave todavía.");
      }
      void loadKeyStatus();
    });
    return h("div", { class: "row" }, h("label", { text: label }), field, save, keyStatus[engine]);
  }

  // ── Engine state (Piper / Whisper): ready, downloading N % ──
  void onEvent<VoiceEngineEvent>(EVT.voiceEngine, (p) => {
    const name = p?.engine ? ENGINE_LABELS[p.engine] ?? p.engine : "Motor de voz";
    const what = p?.item ? ` ${p.item}` : " modelo";
    if (p?.downloading && p.error) {
      engineLine.textContent = `${name}: ${p.error}`;
    } else if (p?.downloading) {
      const pct = Math.max(0, Math.min(100, Math.round(p.pct ?? 0)));
      engineLine.textContent = `${name}: descargando${what} ${pct}%`;
      progress.style.display = "";
      (progress.firstChild as HTMLElement).style.width = `${pct}%`;
    } else if (p?.error) {
      progress.style.display = "none";
      engineLine.textContent = `${name}: no se pudo descargar${what} (${p.error})`;
    } else {
      progress.style.display = "none";
      engineLine.textContent = p?.ready ? `${name}: listo` : `${name}: no disponible todavía`;
      if (p?.ready) void loadVoices();
    }
  });

  drawBots();
  void loadVoices();
  void loadKeyStatus();

  return h("section", {},
    h("h2", {}, h("span", { text: "Voz" })),
    engineLine,
    progress,
    h("div", { class: "row" },
      h("label", { text: "Leer respuestas en voz alta" }),
      readToggle,
      h("span", { class: "hint", text: "Las respuestas de tus Bots se leen con su voz al llegar." })),
    botRows,
    h("div", { class: "row" },
      h("label", { text: "Leer en voz alta los avisos del agente de Cursor" }),
      speakCursor,
      h("span", { class: "hint", text: "Cuando termina, pregunta o pide tu aprobación, lo dice con su voz." })),
    keyRow("elevenlabs", "Clave de ElevenLabs"),
    keyRow("azure", "Clave de Azure"),
    h("div", { class: "hint", text: "Azure: escribe la región y la clave separadas por dos puntos, p. ej. eastus:xxxx." }),
    feedback,
  );
}

// Settings → Dictation: Flow (flow/mod.rs). Hold a key anywhere, speak, let go
// and the words are pasted where you type. Off by default. The dictionary,
// replacements, snippets, per-app styles, numbers and history live here too;
// all of it stays on this PC (%LOCALAPPDATA%\ARIA\Flow).

import { onEvent } from "../core/bridge";
import {
  FLOW_KEYS, FlowBridge, keyLabel, type AppStyle, type FlowPrefs, type FlowStyle, type FlowWords, type Snippet,
} from "../core/flow";
import { h, clear } from "../views/dom";
import { t } from "../i18n/i18n";

function newId(): string {
  return `${Date.now().toString(16)}-${Math.random().toString(16).slice(2, 8)}`;
}

function toggle(on: boolean, onChange: (v: boolean) => void): HTMLElement {
  const el = h("button", { class: on ? "switch on" : "switch", "aria-pressed": on });
  el.addEventListener("click", () => {
    const next = !el.classList.contains("on");
    el.classList.toggle("on", next);
    onChange(next);
  });
  return el;
}

function select<T extends string>(options: { id: T; label: string }[], value: T, onChange: (v: T) => void): HTMLSelectElement {
  const s = h("select", {}) as HTMLSelectElement;
  for (const o of options) s.append(h("option", { value: o.id, text: o.label }));
  s.value = value;
  s.addEventListener("change", () => onChange(s.value as T));
  return s;
}

function styleOptions(): { id: FlowStyle; label: string }[] {
  return [
    { id: "verbatim", label: t("As said") },
    { id: "natural", label: t("Natural") },
    { id: "polished", label: t("Polished (emails)") },
  ];
}

/** "said → written" lines, one replacement each. */
export function parseReplacements(text: string): { from: string; to: string }[] {
  const out: { from: string; to: string }[] = [];
  for (const line of text.split(/\r?\n/)) {
    const m = line.split(/\s*(?:=>|→|->)\s*/);
    if (m.length === 2 && m[0].trim() && m[1].trim()) out.push({ from: m[0].trim(), to: m[1].trim() });
  }
  return out;
}

export function flowSection(): HTMLElement {
  const body = h("div", { style: "display:flex;flex-direction:column;gap:10px" });
  const section = h("section", {}, h("h2", {}, h("span", { text: t("Dictation") })), body);
  void build(body);
  return section;
}

async function build(body: HTMLElement) {
  const status = await FlowBridge.status();
  if (!status) {
    body.append(h("div", { class: "hint", text: t("Dictation is not available here.") }));
    return;
  }
  const prefs: FlowPrefs = status.prefs;
  const save = () => void FlowBridge.setPrefs(prefs);

  const keyHint = h("div", { class: "hint" });
  const showKeyHint = () => {
    keyHint.textContent = t("Hold {key} anywhere and speak; let go and ARIA pastes your words where you type. Tap it twice for hands-free, tap again to paste. Esc cancels.", { key: keyLabel(prefs.key) });
  };
  showKeyHint();

  body.append(
    h("div", { class: "row" },
      h("label", { text: t("Dictate anywhere") }),
      toggle(prefs.enabled, (v) => { prefs.enabled = v; save(); }),
    ),
    keyHint,
  );
  if (!status.modelReady) {
    body.append(h("div", { class: "notice warn", text: t("Dictation needs Parakeet: download it in Voice → Speech recognition.") }));
  }
  body.append(
    h("div", { class: "row" },
      h("label", { text: t("Key") }),
      select(FLOW_KEYS, prefs.key, (v) => { prefs.key = v; save(); showKeyHint(); }),
    ),
    h("div", { class: "row" },
      h("label", { text: t("Clean up with AI") }),
      toggle(prefs.cleanup, (v) => { prefs.cleanup = v; save(); }),
    ),
    h("div", {
      class: "hint",
      text: status.cleanupProvider
        ? t("Removes the “um”, the repeats and the corrections, and fixes punctuation. Only the text goes to {provider}, never the audio.", { provider: status.cleanupProvider })
        : t("Needs an AI with a key (xAI, Anthropic, OpenAI or Google), or a model chosen for Ollama or LM Studio, in Settings → Assistant. Without one the words are pasted as heard."),
    }),
    h("div", { class: "row" },
      h("label", { text: t("Style") }),
      select(styleOptions(), prefs.defaultStyle, (v) => { prefs.defaultStyle = v; save(); }),
    ),
    h("div", { class: "row" },
      h("label", { text: t("Keep history") }),
      select(
        [
          { id: "0", label: t("Not kept") },
          { id: "7", label: t("7 days") },
          { id: "30", label: t("30 days") },
          { id: "90", label: t("90 days") },
          { id: "-1", label: t("Forever") },
        ],
        String(prefs.keepDays),
        (v) => { prefs.keepDays = Number(v); save(); },
      ),
    ),
  );

  // ── Numbers ─────────────────────────────────────────────────────────────────
  const numbers = h("div", { class: "hint" });
  const refreshNumbers = async () => {
    const s = await FlowBridge.stats();
    if (!s) return;
    numbers.textContent = t("This week: {words} words, {wpm} words a minute, {saved} minutes saved. Streak: {streak} days.", {
      words: s.week.words,
      wpm: s.week.wordsPerMinute,
      saved: Math.round(s.week.minutesSaved),
      streak: s.streak,
    });
  };
  body.append(h("div", { class: "subhead", text: t("Statistics") }), numbers);

  // ── Dictionary and replacements ─────────────────────────────────────────────
  const words: FlowWords = (await FlowBridge.words()) ?? { words: [], replacements: [] };
  const wordsArea = h("textarea", { rows: "3", placeholder: t("One per line: Supabase, Quintero…") }) as HTMLTextAreaElement;
  wordsArea.value = words.words.join("\n");
  wordsArea.addEventListener("change", () => {
    words.words = wordsArea.value.split(/\r?\n/).map((w) => w.trim()).filter(Boolean);
    void FlowBridge.setWords(words);
  });
  const repArea = h("textarea", { rows: "3", placeholder: t("said → written, one per line") }) as HTMLTextAreaElement;
  repArea.value = words.replacements.map((r) => `${r.from} → ${r.to}`).join("\n");
  repArea.addEventListener("change", () => {
    words.replacements = parseReplacements(repArea.value).map((r) => ({ id: newId(), ...r }));
    void FlowBridge.setWords(words);
  });
  body.append(
    h("div", { class: "subhead", text: t("Dictionary") }),
    h("div", { class: "hint", text: t("Words ARIA should always spell this way.") }),
    wordsArea,
    h("div", { class: "hint", text: t("Replacements: what you say, and what ARIA writes instead.") }),
    repArea,
  );

  // ── Snippets ────────────────────────────────────────────────────────────────
  let snippets: Snippet[] = (await FlowBridge.snippets()) ?? [];
  const snippetList = h("div", { style: "display:flex;flex-direction:column;gap:6px" });
  const saveSnippets = () => void FlowBridge.setSnippets(snippets.filter((s) => s.trigger.trim()));
  const drawSnippets = () => {
    clear(snippetList);
    for (const s of snippets) {
      const trigger = h("input", { type: "text", placeholder: t("When I say…") }) as HTMLInputElement;
      trigger.value = s.trigger;
      trigger.addEventListener("change", () => { s.trigger = trigger.value; saveSnippets(); });
      const text = h("textarea", { rows: "2", placeholder: t("…write this") }) as HTMLTextAreaElement;
      text.value = s.text;
      text.addEventListener("change", () => { s.text = text.value; saveSnippets(); });
      const remove = h("button", {
        text: t("Remove"),
        onclick: () => { snippets = snippets.filter((x) => x !== s); saveSnippets(); drawSnippets(); },
      });
      snippetList.append(h("div", { class: "row" }, trigger, text, remove));
    }
  };
  drawSnippets();
  body.append(
    h("div", { class: "subhead", text: t("Snippets") }),
    h("div", { class: "hint", text: t("Say the trigger, get the whole text: a signature, an address, a link.") }),
    snippetList,
    h("div", { class: "row" }, h("button", {
      text: t("Add a snippet"),
      onclick: () => { snippets.push({ id: newId(), trigger: "", text: "" }); drawSnippets(); },
    })),
  );

  // ── Styles per app ──────────────────────────────────────────────────────────
  let styles: AppStyle[] = (await FlowBridge.styles()) ?? [];
  const styleList = h("div", { style: "display:flex;flex-direction:column;gap:4px" });
  const addApp = h("select", {}) as HTMLSelectElement;
  const drawStyles = () => {
    clear(styleList);
    for (const s of styles) {
      styleList.append(h("div", { class: "row" },
        h("label", { text: s.name }),
        select(styleOptions(), s.style, (v) => { s.style = v; void FlowBridge.setStyles(styles); }),
        h("button", {
          text: t("Remove"),
          onclick: () => { styles = styles.filter((x) => x !== s); void FlowBridge.setStyles(styles); drawStyles(); },
        }),
      ));
    }
  };
  const fillApps = async () => {
    clear(addApp);
    addApp.append(h("option", { value: "", text: t("Add an open app…") }));
    for (const a of (await FlowBridge.runningApps()) ?? []) {
      if (!styles.some((s) => s.id === a.id)) addApp.append(h("option", { value: a.id, text: a.name }));
    }
  };
  addApp.addEventListener("focus", () => void fillApps());
  addApp.addEventListener("change", () => {
    const name = addApp.selectedOptions[0]?.textContent ?? addApp.value;
    if (!addApp.value) return;
    styles.push({ id: addApp.value, name, style: prefs.defaultStyle });
    void FlowBridge.setStyles(styles);
    drawStyles();
    void fillApps();
  });
  drawStyles();
  void fillApps();
  body.append(
    h("div", { class: "subhead", text: t("Style per app") }),
    h("div", { class: "hint", text: t("With the clean-up on: emails polished, chats natural, code and terminals exactly as said.") }),
    styleList,
    h("div", { class: "row" }, addApp),
  );

  // ── History ─────────────────────────────────────────────────────────────────
  const history = h("div", { style: "display:flex;flex-direction:column;gap:4px" });
  const refreshHistory = async () => {
    clear(history);
    const entries = (await FlowBridge.history()) ?? [];
    if (entries.length === 0) history.append(h("span", { class: "hint", text: t("Nothing dictated yet.") }));
    for (const e of entries.slice(0, 30)) {
      const when = new Date(e.date).toLocaleString([], { day: "2-digit", month: "short", hour: "2-digit", minute: "2-digit" });
      history.append(h("div", { class: "row" },
        h("span", { class: "hint", text: e.app ? `${when} · ${e.app}` : when }),
        h("span", { text: e.text, style: "flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap", title: e.text }),
        h("button", { text: t("Copy"), onclick: () => void FlowBridge.copy(e.text) }),
        h("button", { text: t("Delete"), onclick: () => void FlowBridge.remove(e.id).then(refreshHistory) }),
      ));
    }
  };
  body.append(
    h("div", { class: "subhead", text: t("History") }),
    history,
    h("div", { class: "row" },
      h("button", { text: t("Clear history"), onclick: () => void FlowBridge.clear(false).then(refreshHistory) }),
      h("button", { text: t("Clear history and statistics"), onclick: () => void FlowBridge.clear(true).then(() => { void refreshHistory(); void refreshNumbers(); }) }),
    ),
  );

  void refreshNumbers();
  void refreshHistory();
  void onEvent("flow-history", () => {
    void refreshHistory();
    void refreshNumbers();
  });
}

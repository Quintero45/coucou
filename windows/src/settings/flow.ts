// Settings → Flow: dictation (flow/mod.rs). Hold a key anywhere, speak, let go
// and the words are pasted where you type. Off by default. Its pages: the
// dictation itself and its history, the statistics, the dictionary and
// replacements, the snippets and the styles per app; all of it stays on this
// PC (%LOCALAPPDATA%\ARIA\Flow).

import { onEvent } from "../core/bridge";
import {
  FLOW_KEYS, FlowBridge, keyLabel, type AppStyle, type FlowPrefs, type FlowStats, type FlowStatus, type FlowStyle,
  type FlowTotals, type FlowWords, type Snippet,
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

export interface FlowSections {
  dictation: HTMLElement;
  insights: HTMLElement;
  dictionary: HTMLElement;
  snippets: HTMLElement;
  styles: HTMLElement;
}

function section(title: string): { el: HTMLElement; body: HTMLElement } {
  const body = h("div", { style: "display:flex;flex-direction:column;gap:10px" });
  return { el: h("section", {}, h("h2", {}, h("span", { text: title })), body), body };
}

/** One section per Flow page, filled once the status has been read. */
export function flowSections(): FlowSections {
  const dictation = section(t("Dictation"));
  const insights = section(t("Statistics"));
  const dictionary = section(t("Dictionary"));
  const snippets = section(t("Snippets"));
  const styles = section(t("Style per app"));
  void FlowBridge.status().then((status) => {
    if (!status) {
      dictation.body.append(h("div", { class: "hint", text: t("Dictation is not available here.") }));
      return;
    }
    const prefs: FlowPrefs = status.prefs;
    const save = () => void FlowBridge.setPrefs(prefs);
    buildDictation(dictation.body, status, prefs, save);
    void buildInsights(insights.body);
    void buildDictionary(dictionary.body);
    void buildSnippets(snippets.body);
    void buildStyles(styles.body, prefs);
  });
  return { dictation: dictation.el, insights: insights.el, dictionary: dictionary.el, snippets: snippets.el, styles: styles.el };
}

function buildDictation(body: HTMLElement, status: FlowStatus, prefs: FlowPrefs, save: () => void) {
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
      h("button", { text: t("Clear history and statistics"), onclick: () => void FlowBridge.clear(true).then(refreshHistory) }),
    ),
  );
  void refreshHistory();
  void onEvent("flow-history", () => void refreshHistory());
}

type Period = "week" | "month" | "all";

function duration(minutes: number): string {
  const m = Math.round(minutes);
  if (m >= 60) return t("{0} h {1}", { 0: Math.floor(m / 60), 1: String(m % 60).padStart(2, "0") });
  return t("{0} min", { 0: m });
}

/** Words, time saved, speed, dictations, the words of each day and the apps
 *  you dictate in. Port of Coucou's FlowHubInsights. */
async function buildInsights(body: HTMLElement) {
  let stats: FlowStats | null = await FlowBridge.stats();
  let period: Period = "month";
  const tabs = h("div", { class: "segmented" });
  const content = h("div", { style: "display:flex;flex-direction:column;gap:14px" });
  const periods: [Period, string][] = [["week", t("Week")], ["month", t("Month")], ["all", t("All")]];
  for (const [id, text] of periods) {
    tabs.append(h("button", {
      class: id === period ? "on" : "",
      text,
      onclick: (ev: Event) => {
        period = id;
        for (const b of tabs.children) b.classList.toggle("on", b === ev.currentTarget);
        draw();
      },
    }));
  }
  body.append(tabs, content);

  const stat = (label: string, value: string, sub: string | null) => {
    const el = h("div", { class: "flow-stat" }, h("div", { class: "hint", text: label }), h("b", { text: value }));
    if (sub) el.append(h("div", { class: "hint", text: sub }));
    return el;
  };

  function draw() {
    clear(content);
    const totals: FlowTotals = stats?.[period] ?? { words: 0, seconds: 0, dictations: 0, wordsPerMinute: 0, minutesSaved: 0 };
    content.append(h("div", { class: "flow-stats" },
      stat(t("Words dictated"), totals.words.toLocaleString(), null),
      stat(t("Time saved"), duration(totals.minutesSaved), t("vs typing at 45 words/min")),
      stat(t("Speed"), String(Math.round(totals.wordsPerMinute)), t("words/min")),
      stat(t("Dictations"), totals.dictations.toLocaleString(), null),
    ));
    if (stats && stats.streak > 0) {
      content.append(h("div", { class: "hint", text: t("Streak: {0} days in a row.", { 0: stats.streak }) }));
    }

    const days = (stats?.daily ?? []).slice(period === "week" ? -7 : -30);
    const max = Math.max(1, ...days.map(([, w]) => w));
    const bars = h("div", { class: "flow-bars" });
    for (const [key, words] of days) {
      const bar = h("i", { class: words === 0 ? "zero" : "", title: `${key} · ${words.toLocaleString()}` });
      bar.style.height = `${Math.max(words === 0 ? 0 : 3, (words / max) * 100)}%`;
      bars.append(bar);
    }
    const axis = h("div", { class: "flow-axis" });
    days.forEach(([key], i) => {
      if (i % (period === "week" ? 1 : 5) === 0 || i === days.length - 1) axis.append(h("span", { text: String(Number(key.split("-")[2])) }));
    });
    content.append(h("div", { class: "subhead", text: t("Words per day") }), bars, axis);

    const apps = Object.entries(stats?.apps ?? {}).sort((a, b) => b[1] - a[1]).slice(0, 8);
    const total = apps.reduce((s, [, w]) => s + w, 0);
    content.append(h("div", { class: "subhead", text: t("Where you dictate") }));
    if (apps.length === 0) content.append(h("div", { class: "hint", text: t("Dictate in an app and it shows here.") }));
    for (const [name, words] of apps) {
      const share = total > 0 ? words / total : 0;
      content.append(h("div", { class: "flow-app" },
        h("div", { class: "row" }, h("span", { text: name, style: "flex:1" }), h("span", { class: "hint", text: `${Math.round(share * 100)} %` })),
        h("div", { class: "track" }, h("i", { style: `width:${Math.max(2, share * 100)}%` })),
      ));
    }
    content.append(h("div", { class: "hint", text: t("Counted on this computer, never sent anywhere.") }));
  }
  draw();
  void onEvent("flow-history", async () => {
    stats = await FlowBridge.stats();
    draw();
  });
}

async function buildDictionary(body: HTMLElement) {
  const words: FlowWords = (await FlowBridge.words()) ?? { words: [], replacements: [] };
  const wordsArea = h("textarea", { rows: "6", placeholder: t("One per line: Supabase, Quintero…") }) as HTMLTextAreaElement;
  wordsArea.value = words.words.join("\n");
  wordsArea.addEventListener("change", () => {
    words.words = wordsArea.value.split(/\r?\n/).map((w) => w.trim()).filter(Boolean);
    void FlowBridge.setWords(words);
  });
  const repArea = h("textarea", { rows: "6", placeholder: t("said → written, one per line") }) as HTMLTextAreaElement;
  repArea.value = words.replacements.map((r) => `${r.from} → ${r.to}`).join("\n");
  repArea.addEventListener("change", () => {
    words.replacements = parseReplacements(repArea.value).map((r) => ({ id: newId(), ...r }));
    void FlowBridge.setWords(words);
  });
  body.append(
    h("div", { class: "hint", text: t("Words ARIA should always spell this way.") }),
    wordsArea,
    h("div", { class: "hint", text: t("Replacements: what you say, and what ARIA writes instead.") }),
    repArea,
  );
}

async function buildSnippets(body: HTMLElement) {
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
    h("div", { class: "hint", text: t("Say the trigger, get the whole text: a signature, an address, a link.") }),
    snippetList,
    h("div", { class: "row" }, h("button", {
      text: t("Add a snippet"),
      onclick: () => { snippets.push({ id: newId(), trigger: "", text: "" }); drawSnippets(); },
    })),
  );
}

async function buildStyles(body: HTMLElement, prefs: FlowPrefs) {
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
    h("div", { class: "hint", text: t("With the clean-up on: emails polished, chats natural, code and terminals exactly as said.") }),
    styleList,
    h("div", { class: "row" }, addApp),
  );
}

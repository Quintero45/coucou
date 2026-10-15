// Flow dictation's commands (flow/mod.rs): the pill's page and Settings →
// Dictation. Same no-op-in-a-browser rule as bridge.ts.

import { invoke } from "@tauri-apps/api/core";
import { IS_TAURI } from "./bridge";

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!IS_TAURI) return null;
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    console.error(`[aria] ${cmd} failed`, err);
    return null;
  }
}

export type FlowKey = "right_ctrl" | "right_alt" | "right_shift" | "left_ctrl_super";
export type FlowStyle = "verbatim" | "natural" | "polished";

export interface FlowPrefs {
  enabled: boolean;
  key: FlowKey;
  /** 0 = not kept, -1 = forever. */
  keepDays: number;
  cleanup: boolean;
  defaultStyle: FlowStyle;
}

export interface FlowStatus {
  prefs: FlowPrefs;
  modelReady: boolean;
  keys: string;
  cleanupProvider: string | null;
}

export interface FlowEntry {
  id: string;
  date: number;
  text: string;
  app: string | null;
  words: number;
  seconds: number;
}

export interface FlowTotals {
  words: number;
  seconds: number;
  dictations: number;
  wordsPerMinute: number;
  minutesSaved: number;
}

export interface FlowStats {
  week: FlowTotals;
  month: FlowTotals;
  all: FlowTotals;
  daily: [string, number][];
  apps: Record<string, number>;
  streak: number;
}

export interface Replacement {
  id: string;
  from: string;
  to: string;
}

export interface FlowWords {
  words: string[];
  replacements: Replacement[];
}

export interface Snippet {
  id: string;
  trigger: string;
  text: string;
}

export interface AppStyle {
  id: string;
  name: string;
  style: FlowStyle;
}

export interface PillAnnouncement {
  id: number;
  icon: string;
  tint: string;
  text: string;
  action: string | null;
  actionId: string | null;
}

export interface PillState {
  phase: "idle" | "listening" | "thinking" | "locked" | "copied" | "failed" | "pastedAgain";
  key: FlowKey | "";
  announcement: PillAnnouncement | null;
  /** ARIA is taking a call's notes since then (ms since 1970). */
  noteStartedAt?: number | null;
}

export const FLOW_KEYS: { id: FlowKey; label: string }[] = [
  { id: "right_ctrl", label: "Right Ctrl" },
  { id: "right_alt", label: "Right Alt" },
  { id: "right_shift", label: "Right Shift" },
  { id: "left_ctrl_super", label: "Ctrl + Win" },
];

export function keyLabel(key: string): string {
  return FLOW_KEYS.find((k) => k.id === key)?.label ?? key;
}

export const FlowBridge = {
  status: () => call<FlowStatus>("flow_status"),
  setPrefs: (prefs: FlowPrefs) => call<void>("flow_set_prefs", { prefs }),
  history: () => call<FlowEntry[]>("flow_history"),
  remove: (id: string) => call<void>("flow_delete", { id }),
  clear: (everything: boolean) => call<void>("flow_clear", { everything }),
  stats: () => call<FlowStats>("flow_stats"),
  words: () => call<FlowWords>("flow_words"),
  setWords: (words: FlowWords) => call<void>("flow_set_words", { words }),
  snippets: () => call<Snippet[]>("flow_snippets"),
  setSnippets: (snippets: Snippet[]) => call<void>("flow_set_snippets", { snippets }),
  styles: () => call<AppStyle[]>("flow_styles"),
  setStyles: (styles: AppStyle[]) => call<void>("flow_set_styles", { styles }),
  runningApps: () => call<{ id: string; name: string }[]>("flow_running_apps"),
  copy: (text: string) => call<boolean>("flow_copy", { text }),

  pillState: () => call<PillState>("pill_state"),
  pillHot: (rects: [number, number, number, number][]) => call<void>("pill_hot", { rects }),
  pillHover: (inside: boolean) => call<void>("pill_hover", { inside }),
  pillAction: (action: string, id?: number) => call<void>("pill_action", { action, id: id ?? null }),
};

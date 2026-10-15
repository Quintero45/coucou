// Meeting notes' commands (notes/mod.rs). Same no-op-in-a-browser rule as bridge.ts.

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

/** Like call(), but the error message comes back for the page to show. */
async function attempt<T>(cmd: string, args?: Record<string, unknown>): Promise<{ ok: T } | { error: string }> {
  if (!IS_TAURI) return { error: "not available" };
  try {
    return { ok: await invoke<T>(cmd, args) };
  } catch (err) {
    return { error: String(err) };
  }
}

export interface NoteSpeaker {
  id: string;
  name: string;
  color: number;
  isMe: boolean;
}

export interface NoteLine {
  id: string;
  speaker: string;
  start: number;
  end: number;
  text: string;
}

export interface NoteTodo {
  id: string;
  text: string;
  owner: string | null;
  due: string | null;
  done: boolean;
}

export interface Note {
  id: string;
  title: string;
  titleEdited: boolean;
  start: number;
  end: number | null;
  app: string | null;
  status: "recording" | "processing" | "ready" | "failed";
  speakers: NoteSpeaker[];
  lines: NoteLine[];
  summary: string | null;
  keyPoints: string[];
  todos: NoteTodo[];
  error: string | null;
  startLocal: string;
}

export interface NotesState {
  notes: Note[];
  recording: string | null;
  working: string[];
  parakeet: boolean;
  speakersModel: boolean;
  speakersModelMb: number;
}

/** 75 → "1:15", 3725 → "1:02:05". */
export function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const mm = String(Math.floor((s % 3600) / 60)).padStart(2, "0");
  const ss = String(s % 60).padStart(2, "0");
  return s >= 3600 ? `${Math.floor(s / 3600)}:${mm}:${ss}` : `${Math.floor(s / 60)}:${ss}`;
}

export const NotesBridge = {
  state: () => call<NotesState>("notes_state"),
  start: () => attempt<string>("notes_start"),
  stop: () => call<void>("notes_stop"),
  rename: (id: string, title: string) => call<void>("notes_rename", { id, title }),
  renameSpeaker: (id: string, speaker: string, name: string) => call<void>("notes_rename_speaker", { id, speaker, name }),
  toggleTodo: (id: string, todo: string) => call<void>("notes_toggle_todo", { id, todo }),
  remove: (id: string) => call<void>("notes_delete", { id }),
  summarize: (id: string) => call<void>("notes_summarize", { id }),
  retry: (id: string) => call<void>("notes_retry", { id }),
  markdown: (id: string) => call<string>("notes_markdown", { id }),
  exportFile: (id: string) => attempt<string>("notes_export", { id }),
  downloadSpeakers: () => attempt<void>("notes_download_speakers"),
};

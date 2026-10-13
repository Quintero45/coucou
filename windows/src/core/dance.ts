// When ARIA dances to the music any Windows player reports (src-tauri/src/media.rs):
// the Mac's rules (BotCanvasView, DesktopMochi.swift), on the music pill.

import type { BotStateName, IslandMode, IslandViewName } from "./layout";
import { MEDIA_ID } from "./state";

/** The states she dances in; the rest (an alert, an error, sleep) win. */
const DANCE_STATES: ReadonlySet<BotStateName> = new Set(["idle", "working", "thinking", "searching", "finished"]);

/**
 * The island's ARIA: in the compact island whenever music plays, expanded only
 * on the overview with the music pill in front.
 */
export function islandDances(o: {
  music: boolean;
  state: BotStateName;
  mode: IslandMode;
  view: IslandViewName;
  focusId: string | null | undefined;
}): boolean {
  if (!o.music || !DANCE_STATES.has(o.state)) return false;
  if (o.mode === "compact") return true;
  return o.mode === "expanded" && o.view === "overview" && o.focusId === MEDIA_ID;
}

/** ARIA on the desktop: the compact island's rules. */
export function desktopDances(music: boolean, state: BotStateName): boolean {
  return music && DANCE_STATES.has(state);
}

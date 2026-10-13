// "Hey ARIA" in the island: a request heard by voice (voice/listen.rs) opens
// the chat and is sent as if typed — same tools, same approval cards. The
// island tells Rust what it shows, so a wake waits while a card needs a click.

import { Bridge, onEvent, type VoiceState } from "../core/bridge";
import { State } from "../core/state";
import { t } from "../i18n/i18n";
import type { Island } from "./island";

/** Whether the character's pose is the one voice put there. */
let posed = false;

function pose(state: VoiceState) {
  if (state === "listening") {
    if (State.stateOverride === null) {
      State.stateOverride = "question";
      posed = true;
    }
  } else if (posed) {
    posed = false;
    if (State.stateOverride === "question") State.stateOverride = null;
  }
  State.notify();
}

export function registerVoiceHandlers(island: Island) {
  void onEvent<{ state: VoiceState; error?: string }>("voice-state", ({ state, error }) => {
    pose(state);
    if (state === "paused") island.announce(t("Listening paused. Resume it in Settings → Voice."));
    if (state === "error" && error) island.announce(t("“Hey ARIA” stopped: {error}", { error }));
  });

  void onEvent<{ text: string }>("voice-heard", ({ text }) => {
    if (State.paused) {
      void Bridge.voiceReply("");
      return;
    }
    if (State.settings.voiceSubtitles) island.announce(t("You said: “{text}”", { text }));
    State.voiceAsk = text;
    island.alert("prompt");
    State.notify();
  });

  let last = "";
  State.subscribe(() => {
    if (!State.settings.voiceWake) return;
    const key = `${State.view}|${State.mode}|${!!State.pendingApproval}`;
    if (key === last) return;
    last = key;
    void Bridge.voiceIsland(State.view, State.mode, !!State.pendingApproval);
  });
}

// Entry point: boot the bridge, wire the island, start the greeting.

import "./style.css";
import { Bridge, IS_TAURI, onEvent } from "./core/bridge";
import { Sound } from "./core/sound";
import { State, type MediaInfo, type Settings } from "./core/state";
import { Island } from "./island/island";
import { registerHookHandlers } from "./island/hooks";
import { registerAssistantHandlers } from "./island/assistant";
import { registerIntegrationHandlers, refreshConfigured } from "./island/integrations";
import { registerShortcutHandlers } from "./island/shortcuts";
import { Recap } from "./recap/recap";
import { onLanguageChange, resolveLanguage, setLanguage, systemLanguages } from "./i18n/i18n";

/** Shows the language Settings asks for ("" = the system's, when ARIA has it). */
function applyLanguage() {
  setLanguage(resolveLanguage(State.settings.language, systemLanguages()));
}

async function main() {
  const root = document.getElementById("root");
  if (!root) return;

  void Sound.preload();

  const island = new Island(root);

  const boot = await Bridge.boot();
  if (boot) {
    State.settings = { ...State.settings, ...boot.settings };
  }
  // A language change redraws the island in place: the texts given as tl(…)
  // relabel themselves (views/dom.ts) and the views redraw the rest on this
  // sync. Nothing is rebuilt, so tasks, steps and the chat stay as they are.
  onLanguageChange(() => State.notify());
  applyLanguage();
  // Rust shows the tray and its errors in the same language as the webview.
  void Bridge.setSystemLanguages(systemLanguages());
  island.applySettings();
  State.loadIntegrationTasks();
  State.ensureAriaPill();
  // The music pill: what plays now, then every change media.rs reports.
  await onEvent<MediaInfo>("media", (m) => State.setMedia(m));
  void Bridge.mediaState().then((m) => m && State.setMedia(m));
  if (boot && !boot.cursorPoll) island.followPageCursor();
  await island.desktop.init();

  await onEvent<{ x: number; y: number }>("cursor", ({ x, y }) => island.onCursor(x, y));
  await onEvent<boolean>("pointer-inside", (inside) => island.setPointerInside(inside));

  /** Pause has to reach Rust too, or the pollers keep calling out. */
  const setPaused = (on: boolean) => {
    if (State.paused === on) return;
    State.paused = on;
    void Bridge.setPaused(on);
  };

  await onEvent<string>("tray", (what) => {
    switch (what) {
      case "settings":
        setPaused(false);
        island.alert("settings");
        break;
      case "open":
        setPaused(false);
        island.alert(State.defaultView());
        break;
      case "recap":
        setPaused(false);
        void Recap.open(island);
        break;
      case "wardrobe":
        setPaused(false);
        island.alert("wardrobe");
        break;
      case "pause":
        setPaused(!State.paused);
        if (State.paused) island.fsm.forceHidden();
        else island.reveal();
        break;
    }
  });

  await onEvent<null>("screen-changed", () => void Bridge.reposition());

  // Global shortcuts (shortcuts.rs). They open and navigate; none of them answers.
  await onEvent<string>("shortcut", (action) => {
    if (State.paused && action !== "toggle") return;
    switch (action) {
      case "chat":
        island.alert("prompt");
        break;
      case "alert":
        if (State.pendingApproval) {
          State.setFocus(State.pendingApproval.pillId);
          island.alert(State.defaultView());
        } else {
          island.alert(State.defaultView());
        }
        break;
      case "toggle":
        if (State.mode === "expanded") island.collapse();
        else island.alert(State.defaultView());
        break;
      case "reveal":
        // Hold Space (keyhold.rs): opens like "toggle" does, never folds.
        if (State.mode !== "expanded") island.alert(State.defaultView());
        break;
      case "mute":
        island.actions.toggleSound();
        break;
      case "next-pill":
      case "prev-pill": {
        if (State.tasks.length < 2) break;
        const idx = State.tasks.findIndex((t) => t.id === State.focusId);
        const step = action === "next-pill" ? 1 : -1;
        const next = State.tasks[(idx + step + State.tasks.length) % State.tasks.length];
        State.setFocus(next.id);
        island.alert("overview");
        break;
      }
    }
  });

  // The settings window writes preferences; apply them here without a restart.
  await onEvent<Settings>("settings-changed", (s) => {
    const previousMain = State.mainPillId;
    State.settings = { ...State.settings, ...s };
    applyLanguage();
    island.applySettings();
    State.loadIntegrationTasks();
    // A new main tool comes to the front, as on macOS.
    if (State.mainPillId !== previousMain) State.setFocus(State.mainPillId);
    void refreshConfigured();
  });

  registerHookHandlers(island);
  registerAssistantHandlers(island);
  registerIntegrationHandlers(island);
  registerShortcutHandlers(island, () => setPaused(false));

  // Monday recap: app start (greeting over), an agent starting work, waking up.
  const checkRecap = () => void Recap.check(island);
  island.onGreetingDone = checkRecap;
  island.onWake = checkRecap;
  await onEvent<null>("recap-check", checkRecap);

  island.launch();

  // In a plain browser there is no wake strip behind the cursor: make the whole
  // page wake the island so the visuals can be checked with `npm run dev`.
  if (!IS_TAURI) {
    document.addEventListener("click", () => Sound.resume(), { once: true });
  }
}

void main();

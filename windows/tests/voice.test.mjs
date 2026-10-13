// "Hey ARIA" (island/voice.ts, settings/voice.ts): off until the owner turns
// it on, and a heard request goes through the chat like a typed one.

import { test } from "node:test";
import assert from "node:assert/strict";
import { DEFAULT_SETTINGS, State } from "../src/core/state.ts";

test("the microphone stays closed by default", () => {
  assert.equal(DEFAULT_SETTINGS.voiceWake, false);
  assert.equal(DEFAULT_SETTINGS.voiceSpeak, true);
  assert.equal(DEFAULT_SETTINGS.voiceName, "piper");
  assert.equal(DEFAULT_SETTINGS.voiceSubtitles, true);
});

test("nothing heard is waiting at launch", () => {
  assert.equal(State.voiceAsk, null);
});

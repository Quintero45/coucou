// The music pill (media.rs → State.setMedia): it comes and goes with the player.

import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { DEFAULT_SETTINGS, MEDIA_ID, State, mediaColor, mediaPosition } from "../src/core/state.ts";

const playing = (over = {}) => ({
  active: true, app: "Spotify", title: "Late Night Commit", artist: "Mochi FM", album: "Tiny Island",
  playing: true, positionMs: 73_000, durationMs: 164_000, atMs: 1_000_000,
  shuffle: false, repeat: "none", canPrev: true, canNext: true, canSeek: true, canShuffle: true, canRepeat: true,
  cover: null, volume: 0.5, ...over,
});

beforeEach(() => {
  State.tasks = [];
  State.focusId = null;
  State.media = null;
  State.os = "windows";
  State.settings = { ...DEFAULT_SETTINGS, showAgents: true };
  State.loadIntegrationTasks();
  State.ensureAriaPill();
});

const ids = () => State.tasks.map((t) => t.id);

test("a player with a song brings the pill, right after the main one", () => {
  State.setMedia(playing());
  assert.equal(ids()[0], State.mainPillId);
  assert.equal(ids()[1], MEDIA_ID);
  const pill = State.tasks[1];
  assert.equal(pill.name, "Spotify");
  assert.equal(pill.color, "#1ED760");
});

test("the pill follows the player and goes away with it", () => {
  State.setMedia(playing());
  State.setMedia(playing({ app: "Chrome" }));
  assert.equal(State.tasks.filter((t) => t.id === MEDIA_ID).length, 1);
  assert.equal(State.tasks.find((t) => t.id === MEDIA_ID).color, mediaColor("Chrome"));
  State.focusId = MEDIA_ID;
  State.setMedia(playing({ active: false, title: "" }));
  assert.ok(!ids().includes(MEDIA_ID));
  assert.equal(State.focusId, State.mainPillId);
});

test("a player without a name is just Music", () => {
  State.setMedia(playing({ app: "" }));
  assert.equal(State.tasks.find((t) => t.id === MEDIA_ID).name, "Music");
});

test("the bar moves on while it plays, and stops at the end", () => {
  const m = playing();
  assert.equal(mediaPosition(m, 1_000_000), 73_000);
  assert.equal(mediaPosition(m, 1_010_000), 83_000);
  assert.equal(mediaPosition(m, 9_000_000), 164_000);
  assert.equal(mediaPosition({ ...m, playing: false }, 1_010_000), 73_000);
});

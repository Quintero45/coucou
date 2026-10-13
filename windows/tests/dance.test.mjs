// ARIA dances to the music (core/dance.ts, aria/engine.ts) and the music
// card's small helpers (views/media.ts).

import { test } from "node:test";
import assert from "node:assert/strict";
import { desktopDances, islandDances } from "../src/core/dance.ts";
import { danceTransform, stepDanceLevel } from "../src/aria/engine.ts";
import { MEDIA_ID } from "../src/core/state.ts";
import { clock, volumeLevel } from "../src/views/media.ts";

const base = { music: true, state: "idle", mode: "compact", view: "overview", focusId: "integration_claude" };

test("in the compact island she dances whenever music plays", () => {
  assert.equal(islandDances(base), true);
  assert.equal(islandDances({ ...base, music: false }), false);
});

test("an alert, an error or sleep wins over the dance", () => {
  for (const state of ["approval", "error", "sleeping"]) {
    assert.equal(islandDances({ ...base, state }), false, state);
    assert.equal(desktopDances(true, state), false, state);
  }
  assert.equal(desktopDances(true, "working"), true);
  assert.equal(desktopDances(false, "idle"), false);
});

test("expanded, she dances only on the music card", () => {
  const open = { ...base, mode: "expanded" };
  assert.equal(islandDances(open), false);
  assert.equal(islandDances({ ...open, focusId: MEDIA_ID }), true);
  assert.equal(islandDances({ ...open, focusId: MEDIA_ID, view: "chat" }), false);
  assert.equal(islandDances({ ...base, mode: "hidden" }), false);
});

test("the dance fades in over 0.3 s and out over 0.5 s", () => {
  assert.equal(stepDanceLevel(0, true, 0.15), 0.5);
  assert.equal(stepDanceLevel(0, true, 1), 1);
  assert.equal(stepDanceLevel(1, false, 0.25), 0.5);
  assert.equal(stepDanceLevel(1, false, 1), 0);
  assert.equal(stepDanceLevel(0, false, 1), 0);
});

test("standing still the dance changes nothing, dancing it hops and sways", () => {
  const still = danceTransform(0.3, 0, 30);
  assert.deepEqual([still.dx, still.dy, still.rotate, still.sx, still.sy].map((v) => Math.abs(v)), [0, 0, 0, 1, 1]);
  // Half a beat at 112 BPM: the top of the hop.
  const top = danceTransform(60 / 112 / 2, 1, 30);
  assert.ok(Math.abs(top.dy + 6) < 1e-9);
  assert.ok(Math.abs(top.dx) > 2);
});

test("the card's clock and volume icon", () => {
  assert.equal(clock(73_000), "1:13");
  assert.equal(clock(3_725_000), "1:02:05");
  assert.equal(clock(-5), "0:00");
  assert.deepEqual([0, 0.2, 0.5, 0.9].map(volumeLevel), [0, 1, 2, 3]);
});

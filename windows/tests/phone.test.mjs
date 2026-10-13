// The phone link (island/phone-link.ts, core/phone-chat.ts): off by default, and
// what reaches the phone is a folder's name and a bare command, never a path.

import { test } from "node:test";
import assert from "node:assert/strict";
import { DEFAULT_SETTINGS } from "../src/core/state.ts";
import { MAX_ALLOWED, PHONE_PROVIDERS, modelId, modelsOf, providerOf, toggleModel, usableProviders } from "../src/core/phone-chat.ts";
import { bareCommand, folderName } from "../src/island/phone-link.ts";

test("the link is off by default", () => {
  assert.notEqual(DEFAULT_SETTINGS.phoneLink, true);
  assert.notEqual(DEFAULT_SETTINGS.phoneChat, true);
});

test("Cursor is not offered to the phone's chat", () => {
  assert.ok(!PHONE_PROVIDERS.some((p) => p.id === "cursor"));
  assert.ok(PHONE_PROVIDERS.some((p) => p.id === "anthropic"));
});

test("model ids are provider/model and only known providers count", () => {
  assert.equal(modelId("anthropic", "claude-x"), "anthropic/claude-x");
  assert.equal(providerOf("anthropic/claude-x"), "anthropic");
  assert.equal(providerOf("cursor/auto"), null);
  assert.equal(providerOf("anthropic/"), null);
  assert.equal(providerOf("nothing"), null);
});

test("ticking a model keeps order, has no duplicates and is capped", () => {
  assert.deepEqual(toggleModel(["a/1"], "a/1", true), ["a/1"]);
  assert.deepEqual(toggleModel(["a/1", "b/2"], "a/1", false), ["b/2"]);
  const many = Array.from({ length: MAX_ALLOWED + 5 }, (_, i) => `openai/m${i}`);
  assert.equal(toggleModel(many, "openai/x", true).length, MAX_ALLOWED);
  assert.deepEqual(modelsOf(["openai/a", "xai/b", "openai/c"], "openai"), ["a", "c"]);
});

test("cloud providers need a key, model servers do not", () => {
  const ids = usableProviders(DEFAULT_SETTINGS, {}).map((p) => p.id);
  assert.ok(!ids.includes("anthropic"));
  assert.ok(ids.includes("ollama"));
  assert.ok(usableProviders(DEFAULT_SETTINGS, { anthropic: true }).some((p) => p.id === "anthropic"));
});

test("the phone gets a folder's name and a bare command", () => {
  assert.equal(folderName("C:\\Users\\me\\proj"), "proj");
  assert.equal(folderName("/home/me/proj/"), "proj");
  assert.equal(folderName(".."), undefined);
  assert.equal(folderName(null), undefined);
  assert.equal(bareCommand("Bash", "Bash · npm test"), "npm test");
  assert.equal(bareCommand("Bash", "npm test"), "npm test");
});

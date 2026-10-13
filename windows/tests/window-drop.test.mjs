// ARIA dropped on another app's window: the chat's first message carries it
// (views/chat.ts firstContext), and a dropped file still wins.

import { test } from "node:test";
import assert from "node:assert/strict";
import { State } from "../src/core/state.ts";
import { firstContext } from "../src/views/chat.ts";

test("the window she was dropped on is the chat's context", () => {
  State.droppedFile = null;
  State.droppedWindow = { appName: "chrome", title: "Inbox", screenshot: "aria-1-window.png" };
  assert.deepEqual(firstContext(), { kind: "window", appName: "chrome", title: "Inbox", screenshot: "aria-1-window.png" });
});

test("a dropped file comes first, and nothing dropped means no context", () => {
  State.droppedWindow = { appName: "chrome", title: "Inbox" };
  State.droppedFile = { name: "a.pdf", path: "C:\\a.pdf" };
  assert.deepEqual(firstContext(), { kind: "file", name: "a.pdf", path: "C:\\a.pdf" });
  State.droppedFile = null;
  State.droppedWindow = null;
  assert.equal(firstContext(), null);
});

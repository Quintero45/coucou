// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift, now talking to the assistant (agent.rs): replies
// stream in as `chat-delta`, each tool it runs shows as a step, and the send
// button turns into Stop while it works.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { renderMarkdown } from "./markdown";
import { Bridge, onEvent, type ChatContext } from "../core/bridge";
import { parseTodos, sendToAll, sendToBot as sendToGrokBot } from "../core/botchat";
import { Sound } from "../core/sound";
import { ASSISTANT_ID, State, aiProvider, type ChatMessage } from "../core/state";
import type { ViewHost } from "./views";
import { t, tl } from "../i18n/i18n";

let nextId = 1;

// ── The conversation survives a restart ───────────────────────────────────────
// The last messages stay in the island's localStorage, on this PC only; what
// she learnt from them is in memory/ (history.jsonl, notes.md, skills).

const CHAT_KEY = "aria.chat";
const CHAT_KEPT = 60;

function loadChat(): ChatMessage[] {
  try {
    const raw = JSON.parse(localStorage.getItem(CHAT_KEY) ?? "[]") as unknown;
    if (!Array.isArray(raw)) return [];
    return raw
      .filter((m): m is ChatMessage =>
        !!m && (m.role === "user" || m.role === "assistant") && typeof m.content === "string")
      .map((m) => ({ id: nextId++, role: m.role, content: m.content, steps: Array.isArray(m.steps) ? m.steps : undefined }));
  } catch {
    return [];
  }
}

function saveChat() {
  const kept = State.chatHistory
    .filter((m) => m.content || m.steps?.length)
    .slice(-CHAT_KEPT)
    .map(({ role, content, steps }) => ({ role, content, steps }));
  try {
    localStorage.setItem(CHAT_KEY, JSON.stringify(kept));
  } catch {
    // Full or unavailable: the chat just won't come back after a restart.
  }
}

/** ARIA's pill follows her turn: thinking, working on a step, then at rest. */
function ariaPill(state: "thinking" | "working" | "idle", finalLine?: string, step?: string) {
  const task = State.tasks.find((x) => x.id === ASSISTANT_ID);
  if (!task) return;
  if (state === "thinking") {
    task.steps = [];
    task.stepIndex = 0;
    delete task.stepSeq;
  }
  if (finalLine !== undefined) task.finalLine = finalLine;
  try {
    if (step) State.appendStep(ASSISTANT_ID, step);
    State.updateTask(ASSISTANT_ID, state);
  } catch (err) {
    // The pill is decoration: it never stops the chat itself.
    void Bridge.log(`aria pill ${state} failed: ${String(err).slice(0, 200)}`);
  }
}

const firstLine = (text: string) =>
  (text.split("\n").find((l) => l.trim()) ?? "").replace(/[*_`#>]/g, "").trim().slice(0, 140);

function steps(message: ChatMessage): HTMLElement | null {
  if (!message.steps?.length) return null;
  return h(
    "div",
    { class: "chat-steps" },
    ...message.steps.map((s) => h("div", { class: "chat-step", text: `› ${s}` })),
  );
}

function bubble(message: ChatMessage): HTMLElement {
  if (message.role === "user") {
    return h(
      "div",
      { class: "chat-row user" },
      h("div", { class: "bubble", text: message.content }),
    );
  }
  const reply = h("div", { class: "reply" }, steps(message));
  if (message.content) {
    const text = h("div");
    renderMarkdown(text, message.content);
    reply.append(text);
  }
  return h("div", { class: "chat-row" }, reply);
}

function typingDots(): HTMLElement {
  return h(
    "div",
    { class: "chat-row" },
    h("div", { class: "typing" }, h("i"), h("i"), h("i")),
  );
}

/** The coloured chip showing what the question is about (a dropped file). */
function contextChip(label: string): HTMLElement {
  const chip = h("div", { class: "chip" }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

export function buildPrompt(onHeightChange: () => void): ViewHost {
  if (State.chatHistory.length === 0) State.chatHistory = loadChat();
  const chipRow = h("div", { class: "chip-row" });
  const log = h("div", { class: "chat-log" });
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: t("Ask me anything…"),
    spellcheck: "false",
  }) as HTMLInputElement;
  const send = h("button", { class: "send-btn", title: tl("Send") }, svg(ICONS.arrowUp, 11));
  const bar = h("div", { class: "chat-bar" }, input, send);

  const el = h(
    "div",
    { class: "view" },
    h("div", { class: "card wash chat-card" }, h("div", { class: "chat-body" }, chipRow, log, bar)),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  let sending = false;
  let renderedKey = "";
  /** The reply being written right now, and its row in the log. */
  let live: ChatMessage | null = null;
  let liveRow: HTMLElement | null = null;
  let repaint = 0;

  const scrollDown = () => {
    log.scrollTop = log.scrollHeight;
  };

  /** Re-renders only the live reply, at most once a frame. */
  const paintLive = () => {
    if (repaint) return;
    repaint = requestAnimationFrame(() => {
      repaint = 0;
      if (!live) return;
      const row = bubble(live);
      if (liveRow?.isConnected) liveRow.replaceWith(row);
      else {
        log.querySelector(".typing")?.parentElement?.remove();
        log.append(row);
      }
      liveRow = row;
      scrollDown();
    });
  };

  void onEvent<string>("chat-delta", (text) => {
    if (!live) return;
    live.content += text;
    State.stateOverride = "working";
    paintLive();
  });
  void onEvent<{ label: string }>("chat-step", ({ label }) => {
    if (!live) return;
    (live.steps ??= []).push(label);
    State.stateOverride = "working";
    paintLive();
    ariaPill("working", undefined, label);
  });

  function setSendMode(stop: boolean) {
    clear(send);
    send.append(stop ? h("i", { class: "stop-square" }) : svg(ICONS.arrowUp, 11));
    send.title = stop ? t("Stop") : t("Send");
    send.classList.toggle("stop", stop);
  }

  /** "@Name task" goes straight to that Grok Bot, without ARIA in between. */
  function botTarget(query: string): { bot: string; task: string } | null {
    if (!query.startsWith("@")) return null;
    // "@todos mensaje": the same message to every Grok Bot.
    const all = parseTodos(query);
    if (all != null) return all.trim() ? { bot: "*", task: all.trim() } : null;
    const lower = query.toLowerCase();
    const hit = State.settings.grokBots
      .flatMap((b) => [b.name, b.id].map((n) => ({ id: b.id, prefix: `@${n.toLowerCase()}` })))
      .filter((c) => lower.startsWith(c.prefix) && /\s/.test(query.charAt(c.prefix.length)))
      .sort((a, b) => b.prefix.length - a.prefix.length)[0];
    if (!hit) return null;
    const task = query.slice(hit.prefix.length).trim();
    return task ? { bot: hit.id, task } : null;
  }

  async function sendToBot(target: { bot: string; task: string }, query: string) {
    State.chatHistory.push({ id: nextId++, role: "user", content: query });
    const message: ChatMessage = { id: nextId++, role: "assistant", content: "", steps: [] };
    State.chatHistory.push(message);
    State.notify();
    onHeightChange();
    // Same path as the Bot's conversation, so it shows up there too.
    const sent = target.bot === "*" ? await sendToAll(target.task) : await sendToGrokBot(target.bot, target.task);
    if (sent.ok) {
      message.content = `✓ ${sent.message}`;
    } else {
      message.content = `⚠ ${sent.message}`;
    }
    saveChat();
  }

  async function submit() {
    const query = input.value.trim();
    if (!query || sending) return;
    input.value = "";
    sending = true;
    setSendMode(true);
    Sound.play("send");

    const target = botTarget(query);
    if (target) {
      await sendToBot(target, query);
      sending = false;
      setSendMode(false);
      renderedKey = "";
      State.notify();
      onHeightChange();
      input.focus();
      return;
    }

    const file = State.droppedFile;
    const context: ChatContext | null =
      State.chatHistory.length === 0 && file ? { kind: "file", name: file.name, path: file.path } : null;

    State.chatHistory.push({ id: nextId++, role: "user", content: query });
    // In the history from the start, so the island grows to fit it as it streams.
    const message: ChatMessage = { id: nextId++, role: "assistant", content: "", steps: [] };
    live = message;
    liveRow = null;
    State.chatHistory.push(message);
    // Anything that throws from here on still lands in finally: a stuck
    // `sending` would leave the box disabled until a restart.
    try {
      State.stateOverride = "thinking";
      ariaPill("thinking");
      State.notify();
      onHeightChange();
      const reply = await Bridge.chatSend(query, context);
      message.content = reply.text;
      State.stateOverride = null;
      ariaPill("idle", firstLine(reply.text));
      Sound.play("finish");
    } catch (err) {
      State.stateOverride = null;
      const text = String(err).replace(/^Error:\s*/, "");
      ariaPill("idle", `⚠ ${firstLine(text)}`);
      if (message.content || message.steps?.length) {
        message.content += `${message.content ? "\n\n" : ""}⚠ ${text}`;
      } else {
        State.chatHistory = State.chatHistory.filter((m) => m !== message);
        State.noteMessage = text;
        State.view = "note";
      }
      Sound.play("error");
    } finally {
      saveChat();
      live = null;
      liveRow = null;
      sending = false;
      setSendMode(false);
      renderedKey = "";
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  send.addEventListener("click", () => {
    if (sending) {
      Sound.play("blip");
      void Bridge.chatStop();
    } else {
      void submit();
    }
  });
  input.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") {
      e.preventDefault();
      void submit();
    }
    e.stopPropagation(); // Escape closes the island, not the chat
  });

  return {
    el,
    sync() {
      const file = State.droppedFile;
      const wantChip = file?.name ?? "";
      if (chipRow.dataset.label !== wantChip) {
        chipRow.dataset.label = wantChip;
        clear(chipRow);
        if (wantChip) chipRow.append(contextChip(wantChip));
      }

      const key = `${State.chatHistory.length}:${sending}`;
      if (key !== renderedKey) {
        renderedKey = key;
        // A new chat (shortcut, a dropped file) empties what comes back after a restart too.
        if (!sending) saveChat();
        clear(log);
        liveRow = null;
        for (const m of State.chatHistory) {
          if (m === live && !live.content && !live.steps?.length) {
            log.append(typingDots());
            continue;
          }
          const row = bubble(m);
          if (m === live) liveRow = row;
          log.append(row);
        }
        scrollDown();
      }

      const provider = aiProvider(State.settings.provider);
      if (State.chatDraft !== null && !sending) {
        input.value = State.chatDraft;
        State.chatDraft = null;
        requestAnimationFrame(() => {
          input.focus();
          input.setSelectionRange(input.value.length, input.value.length);
        });
      }
      const bots = State.settings.grokBots;
      input.placeholder = State.chatHistory.length > 0
        ? t("Continue…")
        : bots.length > 0
          ? t("Ask {provider}… or “@{bot} task”", { provider: provider.name, bot: bots[0].name })
          : t("Ask {provider}…", { provider: provider.name });
      input.disabled = sending;
    },
    focus() {
      input.focus();
      input.select();
    },
  };
}

// The Cursor agent says its notices aloud (Ajustes → Voz): finished, a
// question, an approval it needs. Listens to the same "hook" event hooks.ts
// handles (a second listener, nothing there changes) and calls speak with
// bot "cursor". Texts are short, Spanish, and go through redactText.

import { Bridge, onEvent } from "../core/bridge";
import { redactText } from "../core/botlog";
import { CMD, callCmd, parseChoices, speakCursorEnabled } from "../core/botcmds";
import { State } from "../core/state";
import { BotLive } from "../core/botlive";

interface CursorHook {
  hook_event_name?: string;
  coucou_agent?: string;
  coucou_kind?: string;
  message?: string;
  last_assistant_message?: string;
  tool_name?: string;
  tool_input?: Record<string, unknown>;
}

/** Same notice again within this window: stay quiet. */
const REPEAT_MS = 3000;
let last = { key: "", at: 0 };

const firstLine = (t: string | undefined, max = 120) =>
  redactText((t ?? "").split("\n").map((l) => l.replace(/[*_`#>]/g, "").trim()).find(Boolean) ?? "").slice(0, max);

/** What the approval is for, from the usual fields, already redacted. */
function target(tool: string, input: Record<string, unknown>): string {
  for (const f of ["command", "file_path", "path", "url", "query", "pattern", "description"]) {
    const v = input[f];
    if (typeof v === "string" && v.trim()) return `${tool}: ${firstLine(v, 80)}`;
  }
  return tool;
}

function noticeFor(p: CursorHook): { kind: string; text: string } | null {
  if (p.coucou_kind === "ask_user_question") {
    const qs = Array.isArray(p.tool_input?.questions) ? (p.tool_input!.questions as { question?: unknown }[]) : [];
    const q = typeof qs[0]?.question === "string" ? (qs[0].question as string) : "";
    return { kind: "question", text: q ? `Tengo una pregunta: ${firstLine(q)}` : "Tengo una pregunta." };
  }
  switch (p.hook_event_name) {
    case "Stop": {
      const summary = firstLine(p.last_assistant_message ?? p.message);
      return { kind: "finished", text: summary ? `Terminé. ${summary}` : "Terminé." };
    }
    case "Notification": {
      const msg = p.message ?? "";
      if (/permission|permiso|approv|aprob/i.test(msg)) return { kind: "approval", text: `Necesito tu aprobación para ${firstLine(msg, 80)}` };
      if (msg.trim().endsWith("?")) return { kind: "question", text: `Tengo una pregunta: ${firstLine(msg)}` };
      return null;
    }
    case "PermissionRequest": {
      // A question card (tool "Pregunta", or any request with choices): asked, not approved.
      const choices = parseChoices(p as unknown as Record<string, unknown>);
      if (p.tool_name === "Pregunta" || choices) {
        const command = p.tool_input?.command;
        const asked = typeof command === "string" ? command : p.message;
        const q = firstLine(asked);
        // Up to three short choices are read out; longer lists aren't.
        const opts = choices?.options.map((o) => o.label) ?? [];
        const tail = opts.length >= 2 && opts.length <= 3 && opts.every((o) => o.length <= 20)
          ? ` ¿${opts.slice(0, -1).join(", ")} o ${opts.at(-1)}?`
          : "";
        return { kind: "question", text: (q ? `Tengo una pregunta: ${q}` : "Tengo una pregunta.") + tail };
      }
      return { kind: "approval", text: `Necesito tu aprobación para ${target(p.tool_name ?? "una acción", p.tool_input ?? {})}` };
    }
    default:
      return null;
  }
}

let registered = false;

export function registerCursorVoice() {
  if (registered) return;
  registered = true;
  void onEvent<CursorHook>("hook", (p) => {
    if (!p || (p.coucou_agent ?? "").trim().toLowerCase() !== "cursor") return;
    const n = noticeFor(p);
    if (!n) return;
    // Every notice that stays silent says why in coucou.log.
    const skip = (why: string) => void Bridge.log(`speak cursor skip=${why} kind=${n.kind}`);
    if (State.paused) return skip("paused");
    if (!State.settings.showCursorAgent) return skip("hidden");
    if (!speakCursorEnabled(State.settings.speakCursor)) return skip("speakCursorOff");
    const key = `${n.kind}|${n.text}`;
    const now = Date.now();
    if (key === last.key && now - last.at < REPEAT_MS) return skip("dup");
    last = { key, at: now };
    void Bridge.log(`speak cursor kind=${n.kind}`);
    // The notice still shows on the island if the voice fails.
    const said = callCmd(CMD.speak, { text: n.text, bot: "cursor" });
    // «Hablando» on Cursor's avatar while it reads (estimated length).
    BotLive.markSpeaking("cursor", n.text.length, said);
    said.catch((err) => void Bridge.log(`speak cursor failed: ${String(err)}`));
  });
}

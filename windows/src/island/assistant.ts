// Approval cards for ARIA's own tools (policy.rs). Same card and buttons as an
// agent's permission request; answering goes back through approval_decision.
// No click in time, a paused island or a card already up all mean no.

import { Bridge, onEvent } from "../core/bridge";
import { setApprovalDetail } from "../core/layout";
import { Sound } from "../core/sound";
import { ASSISTANT_ID, State } from "../core/state";
import { t } from "../i18n/i18n";
import type { Island } from "./island";

interface AssistantApproval {
  requestId: string;
  tool: string;
  command: string;
  detail: string | null;
  allowAlways: boolean;
}

/** Something ARIA did on her own (Settings → Without asking). */
interface AssistantActed {
  tool: string;
  target: string;
}

let timeout: number | null = null;

export function registerAssistantHandlers(island: Island) {
  void onEvent<AssistantActed>("assistant-acted", (act) => {
    const target = act.target.split("\n").find((l) => l.trim()) ?? "";
    island.announce(t("Done without asking: {tool} · {target}", { tool: act.tool, target }));
  });

  void onEvent<AssistantApproval>("assistant-approval", (req) => {
    if (State.paused || State.pendingApproval) {
      void Bridge.approvalDecline(req.requestId);
      return;
    }
    State.pendingApproval = {
      requestId: req.requestId,
      sessionId: "",
      tool: req.tool,
      command: req.command,
      pillId: ASSISTANT_ID,
      allowAlways: req.allowAlways,
      detail: req.detail,
    };
    // Sized before the alert so the island opens straight to the right height.
    setApprovalDetail(!!req.detail);
    void Bridge.approvalAck(req.requestId);
    State.isPinned = true;
    Sound.play("approval");
    island.alert("approval");
    if (timeout != null) window.clearTimeout(timeout);
    // Rust gives up after 120 s (15 min for a diff or a skill); so does the card.
    timeout = window.setTimeout(
      () => {
        timeout = null;
        if (State.pendingApproval?.requestId !== req.requestId) return;
        State.pendingApproval = null;
        State.isPinned = false;
        island.dropPin();
        if (State.view === "approval") island.setView("prompt");
        State.notify();
      },
      req.detail ? 905_000 : 122_000,
    );
    State.notify();
  });
}

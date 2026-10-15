// Settings → Meeting notes (notes/mod.rs): ARIA records a call, writes who
// said what and a summary with the to-dos. Everything stays on this PC
// (%LOCALAPPDATA%\ARIA\Notes); the audio is deleted once the transcript is
// written.

import { onEvent } from "../core/bridge";
import { clock, NotesBridge, type Note, type NotesState } from "../core/notes";
import { FlowBridge } from "../core/flow";
import { h, clear } from "../views/dom";
import { t } from "../i18n/i18n";

const SPEAKER_COLORS = ["#7aa2ff", "#f5a524", "#22c55e", "#f472b6", "#a78bfa", "#2dd4bf", "#fb7185", "#facc15"];

function minutes(note: Note): number {
  return Math.max(1, Math.round(((note.end ?? Date.now()) - note.start) / 60_000));
}

function statusText(note: Note, working: boolean): string {
  if (note.status === "recording") return t("Recording");
  if (note.status === "processing" || (working && note.lines.length === 0)) return t("Writing the transcript…");
  if (working) return t("Writing the summary…");
  if (note.status === "failed") return t("Failed");
  return "";
}

export function notesSection(): HTMLElement {
  const body = h("div", { style: "display:flex;flex-direction:column;gap:10px" });
  const section = h("section", { id: "notes" }, h("h2", {}, h("span", { text: t("Meeting notes") })), body);
  void build(body);
  return section;
}

async function build(body: HTMLElement) {
  const open = new Set<string>();
  let state: NotesState | null = null;
  let timer: number | null = null;
  let message = "";

  const top = h("div", { style: "display:flex;flex-direction:column;gap:10px" });
  const list = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  body.append(
    h("div", {
      class: "hint",
      text: t("ARIA records the call (your microphone and what the PC plays), writes who said what, and a summary with the to-dos. Everything stays on this PC and the audio is deleted once the transcript is written. Tell the other people they are being recorded."),
    }),
    top,
    list,
  );

  const refresh = async () => {
    state = await NotesBridge.state();
    const focused = document.activeElement;
    if (focused instanceof HTMLInputElement && body.contains(focused)) {
      focused.addEventListener("blur", () => render(), { once: true });
      return;
    }
    render();
  };

  function render() {
    clear(top);
    clear(list);
    if (!state) {
      top.append(h("div", { class: "hint", text: t("Meeting notes are not available here.") }));
      return;
    }
    const s = state;
    if (!s.parakeet) {
      top.append(h("div", { class: "notice warn", text: t("Meeting notes need Parakeet: download it in Voice → Speech recognition.") }));
    }
    if (!s.speakersModel) {
      const get = h("button", { text: t("Download ({0} MB)", { 0: s.speakersModelMb }) });
      get.addEventListener("click", async () => {
        get.setAttribute("disabled", "");
        get.textContent = t("Downloading…");
        const r = await NotesBridge.downloadSpeakers();
        message = "error" in r ? r.error : "";
        void refresh();
      });
      top.append(
        h("div", { class: "row" }, h("label", { text: t("Tell the voices apart") }), get),
        h("div", { class: "hint", text: t("Without it, everyone on the call is “Others”.") }),
      );
    }
    const recording = s.notes.find((n) => n.id === s.recording);
    const action = recording
      ? h("button", { class: "danger", text: t("Stop and write the notes") })
      : h("button", { class: "primary", text: t("Take notes of a call") });
    if (!recording && !s.parakeet) action.setAttribute("disabled", "");
    action.addEventListener("click", async () => {
      action.setAttribute("disabled", "");
      if (recording) {
        await NotesBridge.stop();
      } else {
        const r = await NotesBridge.start();
        message = "error" in r ? r.error : "";
      }
      void refresh();
    });
    const live = h("span", { class: "hint" });
    top.append(h("div", { class: "row" }, action, live));
    if (message) top.append(h("div", { class: "notice err", text: message }));
    if (timer !== null) window.clearInterval(timer);
    timer = null;
    if (recording) {
      const tick = () => {
        live.textContent = `● ${clock((Date.now() - recording.start) / 1000)}`;
      };
      tick();
      timer = window.setInterval(tick, 1000);
    }

    if (s.notes.length === 0) list.append(h("div", { class: "hint", text: t("No notes yet.") }));
    for (const note of s.notes) list.append(card(note, s.working.includes(note.id)));
  }

  function card(note: Note, working: boolean): HTMLElement {
    const expanded = open.has(note.id);
    const status = statusText(note, working);
    const head = h("div", { class: "row", style: "cursor:pointer" },
      h("label", { text: `${expanded ? "▾" : "▸"} ${note.title}` }),
      h("span", { class: "hint", text: [note.startLocal, `${minutes(note)} min`, note.app ?? "", status].filter(Boolean).join(" · ") }),
    );
    head.addEventListener("click", () => {
      if (expanded) open.delete(note.id);
      else open.add(note.id);
      render();
    });
    const box = h("div", { class: "note-card" }, head);
    if (!expanded) return box;

    const title = h("input", { type: "text", value: note.title }) as HTMLInputElement;
    title.addEventListener("change", () => void NotesBridge.rename(note.id, title.value));
    box.append(h("div", { class: "row" }, h("label", { text: t("Title") }), title));

    if (note.error) box.append(h("div", { class: "notice err", text: note.error }));
    if (note.summary) {
      box.append(h("div", { class: "subhead", text: t("Summary") }), h("div", { class: "note-text", text: note.summary }));
    }
    if (note.keyPoints.length) {
      box.append(h("div", { class: "subhead", text: t("Key points") }), h("ul", {}, ...note.keyPoints.map((p) => h("li", { text: p }))));
    }
    if (note.todos.length) {
      box.append(h("div", { class: "subhead", text: t("To do") }));
      for (const todo of note.todos) {
        const check = h("input", { type: "checkbox" }) as HTMLInputElement;
        check.checked = todo.done;
        check.addEventListener("change", () => void NotesBridge.toggleTodo(note.id, todo.id));
        const extra = [todo.owner, todo.due].filter(Boolean).join(", ");
        box.append(h("label", { class: "note-todo" }, check, h("span", { text: extra ? `${todo.text} (${extra})` : todo.text })));
      }
    }
    if (note.speakers.length) {
      box.append(h("div", { class: "subhead", text: t("People") }));
      for (const sp of note.speakers) {
        const name = h("input", { type: "text", value: sp.name }) as HTMLInputElement;
        name.addEventListener("change", () => void NotesBridge.renameSpeaker(note.id, sp.id, name.value));
        box.append(h("div", { class: "row" },
          h("span", { class: "note-dot", style: `background:${SPEAKER_COLORS[sp.color % SPEAKER_COLORS.length]}` }),
          name,
        ));
      }
    }
    if (note.lines.length) {
      box.append(h("div", { class: "subhead", text: t("Transcript") }));
      const lines = h("div", { class: "note-lines" });
      for (const line of note.lines) {
        const sp = note.speakers.find((x) => x.id === line.speaker);
        lines.append(h("div", { class: "note-line" },
          h("b", { text: sp?.name ?? line.speaker, style: `color:${SPEAKER_COLORS[(sp?.color ?? 0) % SPEAKER_COLORS.length]}` }),
          h("span", { class: "hint", text: ` ${clock(line.start)} ` }),
          h("span", { text: line.text }),
        ));
      }
      box.append(lines);
    }

    const actions = h("div", { class: "row" });
    if (note.status === "ready" && note.lines.length && !working) {
      actions.append(h("button", { text: note.summary ? t("Summarize again") : t("Summarize"), onclick: () => void NotesBridge.summarize(note.id) }));
    }
    if (note.status === "failed") actions.append(h("button", { text: t("Try again"), onclick: () => void NotesBridge.retry(note.id) }));
    if (note.status === "ready") {
      actions.append(
        h("button", {
          text: t("Copy as Markdown"),
          onclick: async () => {
            const md = await NotesBridge.markdown(note.id);
            if (md) void FlowBridge.copy(md);
          },
        }),
        h("button", {
          text: t("Save as Markdown"),
          onclick: async () => {
            const r = await NotesBridge.exportFile(note.id);
            message = "error" in r ? r.error : "";
            render();
          },
        }),
      );
    }
    if (note.status !== "recording") {
      actions.append(h("button", {
        class: "danger",
        text: t("Delete"),
        onclick: () => {
          open.delete(note.id);
          void NotesBridge.remove(note.id).then(refresh);
        },
      }));
    }
    box.append(actions);
    return box;
  }

  let pending: number | null = null;
  void onEvent<string>("notes-changed", () => {
    if (pending !== null) return;
    pending = window.setTimeout(() => {
      pending = null;
      void refresh();
    }, 250);
  });
  void onEvent("voice-engine", () => void refresh());
  await refresh();
}

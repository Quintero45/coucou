// Buttons for a question that comes with choices (botcmds.ts QUESTION_OPTIONS):
// one per option, the first six visible and the rest behind "Más…", plus an
// "Otra respuesta" box when the asker allows it. Used by the approval card and
// by the conversation; what a pick does is the caller's.

import { QUESTION_OPTIONS, type ChoiceOption } from "../core/botcmds";
import { h } from "./dom";
import { t } from "../i18n/i18n";

export function buildChoices(opts: {
  options: readonly ChoiceOption[];
  allowCustom: boolean;
  onPick: (answer: string) => void;
}): HTMLElement {
  const root = h("div", { class: "choices" });
  const list = h("div", { class: "choices-list" });
  let done = false;
  const pick = (answer: string) => {
    const a = answer.trim();
    if (!a || done) return;
    // One answer per question: a double click mustn't send twice.
    done = true;
    root.classList.add("picked");
    opts.onPick(a);
  };

  const button = (o: ChoiceOption) =>
    h("button", {
      class: "btn secondary choice",
      title: o.description ?? o.label,
      onclick: (e: Event) => {
        e.stopPropagation();
        pick(o.label);
      },
    }, h("span", { text: o.label }));

  const visible = opts.options.slice(0, QUESTION_OPTIONS.visible);
  const rest = opts.options.slice(QUESTION_OPTIONS.visible);
  list.append(...visible.map(button));
  if (rest.length) {
    const more = h("button", { class: "btn secondary choice more", title: t("More options: {count}", { count: rest.length }) }, h("span", { text: t("More…") }));
    more.addEventListener("click", (e) => {
      e.stopPropagation();
      more.replaceWith(...rest.map(button));
    });
    list.append(more);
  }
  root.append(list);

  if (opts.allowCustom) {
    const input = h("input", {
      type: "text",
      class: "choices-custom",
      placeholder: t("Other answer…"),
      spellcheck: "true",
      maxlength: "500",
    }) as HTMLInputElement;
    const send = h("button", { class: "btn primary", title: t("Send (Enter)") }, h("span", { text: t("Send") }));
    send.addEventListener("click", (e) => {
      e.stopPropagation();
      pick(input.value);
    });
    input.addEventListener("keydown", (e) => {
      // Typing here must not reach the island's Y / N shortcuts.
      e.stopPropagation();
      if (e.key === "Enter" && !e.isComposing) {
        e.preventDefault();
        pick(input.value);
      }
    });
    root.append(h("div", { class: "choices-custom-row" }, input, send));
  }
  return root;
}

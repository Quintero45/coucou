// The mail card — port of MailView from IslandViewContent.swift. Opens on a
// dropped file ("Email" on the choose card) or filled in by ARIA
// (compose_email). Only the owner's click on Send sends it (mail.rs): through
// Outlook on this PC, else the owner's Resend integration.

import { h, clear } from "./dom";
import { Bridge, type MailContact, type MailStatus } from "../core/bridge";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import type { ViewActions, ViewHost } from "./views";
import { t, tl } from "../i18n/i18n";

/** mail.rs's error codes, in words. */
export function mailError(code: string): string {
  const c = code.replace(/^Error:\s*/, "");
  if (c === "no_recipient") return t("Add at least one recipient.");
  if (c.startsWith("bad_address:")) return t("{address} is not a valid address.", { address: c.slice(12) });
  if (c === "unresolved") return t("Outlook did not recognise a recipient.");
  if (c === "file_missing") return t("The file is no longer there.");
  if (c === "file_too_big") return t("The file is over 20 MB.");
  if (c === "no_outlook" || c === "no_mail") return t("Outlook is not set up on this PC and Resend is not connected: nothing was sent.");
  if (c === "resend_no_from") return t("Write the sender address Resend should use.");
  if (c === "no_draft") return t("ARIA could not write it this time.");
  if (c.startsWith("outlook:")) return t("Outlook: {error}", { error: c.slice(8) });
  if (c.startsWith("resend:")) return t("Resend: {error}", { error: c.slice(7) });
  return c;
}

/** "1.2 MB", "340 KB". */
export function fileSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

function baseName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

export function buildMail(actions: ViewActions): ViewHost {
  let status: MailStatus | null = null;
  let contacts: MailContact[] = [];
  let sending = false;
  let drafting = false;
  let shownFor: object | null = null;
  let path = "";

  const heading = h("div", { class: "mail-head" });
  const listId = "mail-contacts";
  const contactList = h("datalist", { id: listId });
  const field = (label: string, input: HTMLElement) =>
    h("label", { class: "mail-field" }, h("span", { text: label }), input);
  const to = h("input", { class: "mail-input", type: "text", spellcheck: "false", list: listId, placeholder: "address@example.com" }) as HTMLInputElement;
  const subject = h("input", { class: "mail-input", type: "text", placeholder: tl("Subject") }) as HTMLInputElement;
  const from = h("input", { class: "mail-input", type: "text", spellcheck: "false", placeholder: "sender@yourdomain.com" }) as HTMLInputElement;
  const fromRow = field(t("From"), from);
  const body = h("textarea", { class: "mail-body", placeholder: tl("Message") }) as HTMLTextAreaElement;
  const note = h("div", { class: "mail-note" });

  const send = h("button", { class: "btn primary", text: tl("Send") }) as HTMLButtonElement;
  const write = h("button", { class: "btn secondary", text: tl("Write it for me") }) as HTMLButtonElement;
  const cancel = h("button", { class: "btn secondary", text: tl("Cancel") }) as HTMLButtonElement;

  for (const el of [to, subject, from, body]) {
    // Escape closes the island; Enter in a one-line field doesn't send by surprise.
    el.addEventListener("keydown", (e) => e.stopPropagation());
  }

  function say(text: string, error = false) {
    note.textContent = text;
    note.classList.toggle("error", error);
  }

  function busy() {
    send.disabled = sending || drafting;
    write.disabled = sending || drafting;
    send.textContent = sending ? t("Sending…") : t("Send");
    write.textContent = drafting ? t("Writing…") : t("Write it for me");
  }

  function renderHeading(size?: number) {
    clear(heading);
    heading.append(h("b", { text: t("New email") }));
    if (path) {
      const name = baseName(path);
      const label = t("with {name}", { name }) + (size != null ? ` · ${fileSize(size)}` : "");
      heading.append(h("span", { class: "mail-file", title: path, text: label }));
    }
  }

  async function refreshStatus() {
    status = await Bridge.mailStatus(path || undefined);
    if (!status) return;
    renderHeading(status.size);
    fromRow.style.display = !status.outlook && status.resend ? "" : "none";
    if (!from.value) from.value = status.resendFrom;
    if (path && status.size == null) say(mailError("file_missing"), true);
    else if (status.size != null && status.size > 20 * 1024 * 1024) say(mailError("file_too_big"), true);
    else if (!status.outlook && !status.resend) say(mailError("no_mail"), true);
    else if (!status.outlook) say(t("Outlook is not here: it will go through Resend."));
  }

  async function loadContacts() {
    if (contacts.length) return;
    contacts = (await Bridge.mailContacts()) ?? [];
    clear(contactList);
    for (const c of contacts) {
      contactList.append(h("option", { value: c.address, label: c.name && c.name !== c.address ? c.name : "" }));
    }
  }

  send.addEventListener("click", async () => {
    if (sending || drafting) return;
    sending = true;
    busy();
    say("");
    try {
      if (fromRow.style.display !== "none" && from.value.trim() !== State.settings.mailFrom) {
        State.settings.mailFrom = from.value.trim();
        await Bridge.saveSettings(State.settings);
      }
      const sent = await Bridge.mailSend({
        to: to.value,
        subject: subject.value,
        body: body.value,
        path: path || undefined,
      });
      Sound.play("send");
      State.mailDraft = null;
      State.noteMessage = t("Sent to {to}.", { to: sent.to.join(", ") });
      actions.setView("note");
      window.setTimeout(() => {
        if (State.view === "note") actions.setView(State.defaultView());
      }, 2400);
    } catch (err) {
      say(mailError(String(err)), true);
      Sound.play("error");
    } finally {
      sending = false;
      busy();
    }
  });

  write.addEventListener("click", async () => {
    if (sending || drafting) return;
    drafting = true;
    busy();
    say("");
    try {
      const d = await Bridge.mailDraft(path ? baseName(path) : "", to.value, body.value);
      if (d.subject) subject.value = d.subject;
      if (d.body) body.value = d.body;
    } catch (err) {
      say(mailError(String(err)), true);
    } finally {
      drafting = false;
      busy();
    }
  });

  cancel.addEventListener("click", () => {
    State.mailDraft = null;
    if (State.droppedFile) actions.setView("choose");
    else actions.setView(State.defaultView());
  });

  const el = h(
    "div",
    { class: "view" },
    h(
      "div",
      { class: "card" },
      h(
        "div",
        { class: "stack mail-stack" },
        heading,
        field(t("To"), to),
        field(t("Subject"), subject),
        fromRow,
        body,
        note,
        h("div", { class: "actions" }, send, write, cancel),
        contactList,
      ),
    ),
  );
  fromRow.style.display = "none";

  return {
    el,
    sync() {
      // A new draft (a drop, or ARIA filling it in) replaces what the card shows.
      const draft = State.mailDraft;
      if (!draft || draft === shownFor) return;
      shownFor = draft;
      path = draft.path ?? "";
      to.value = draft.to;
      subject.value = draft.subject || (path ? baseName(path) : "");
      body.value = draft.body;
      renderHeading();
      say("");
      busy();
      void refreshStatus();
      void loadContacts();
    },
    focus() {
      (to.value ? body : to).focus();
    },
  };
}

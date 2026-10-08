// Settings → Coming from Coucou: what the first launch brought over
// (migrate.rs), agents still on Coucou's relay, and the two clicks that finish
// the move — Coucou's own uninstaller, and deleting Coucou's copy of the keys.
// Hidden once there is nothing left to say.

import { Bridge, type MigrationStatus } from "../core/bridge";
import { clear, h } from "../views/dom";
import { statusDot } from "./parts";
import { t } from "../i18n/i18n";

/** In the current language (src/i18n); the settings window redraws on a change. */
const TEXT = {
  get title() { return t("Coming from Coucou"); },
  copied: (at: string, files: number, keys: number) =>
    t("On {date} ARIA brought over {files} files and {keys} keys from Coucou. Coucou's own folders were left as they were.", { date: at, files, keys }),
  problems: (count: number) => t("{count} could not be copied: aria.log lists them.", { count }),
  agents: (names: string) => t("Still on Coucou's relay: {names}. Reinstall them in this window so they reach ARIA.", { names }),
  oldKeys: (count: number) => t("Coucou still holds {count} keys in the Credential Manager. ARIA has its own copy.", { count }),
  get clearKeys() { return t("Delete Coucou's keys"); },
  get clearAgain() { return t("Click again to delete them"); },
  get cleared() { return t("Coucou's keys deleted."); },
  get uninstall() { return t("Uninstall Coucou…"); },
  get uninstallHint() { return t("Opens Coucou's own uninstaller. ARIA keeps everything it copied."); },
};

function errorText(err: unknown): string {
  return String(err).replace(/^Error:\s*/, "");
}

/** Something left to do; the report alone is not worth a section. */
const relevant = (s: MigrationStatus) => s.oldKeys > 0 || s.uninstaller || s.legacyAgents.length > 0;

/** The section on screen: reread when the window comes back, since agents are
 *  reinstalled and Coucou is uninstalled outside of it. */
let refreshShown: (() => Promise<void>) | null = null;
window.addEventListener("focus", () => void refreshShown?.());

export async function coucouSection(): Promise<HTMLElement | null> {
  const first = await Bridge.migrationStatus();
  if (!first || !relevant(first)) return null;

  const body = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const feedback = h("div", { class: "hint" });
  const heading = h("h2", {});
  const section = h("section", {}, heading, body, feedback);

  const refresh = async () => {
    if (!section.isConnected) return;
    const next = await Bridge.migrationStatus();
    if (!next) return;
    if (!relevant(next)) {
      section.remove();
      return;
    }
    draw(next);
  };

  const draw = (s: MigrationStatus) => {
    clear(heading);
    heading.append(statusDot(s.legacyAgents.length === 0), h("span", { text: TEXT.title }));
    clear(body);
    if (s.report) {
      body.append(h("div", { class: "hint", text: TEXT.copied(s.report.at, s.report.files, s.report.keys.length) }));
      if (s.report.problems.length > 0) {
        body.append(h("div", { class: "notice warn", text: TEXT.problems(s.report.problems.length) }));
      }
    }
    if (s.legacyAgents.length > 0) {
      body.append(h("div", { class: "notice warn", text: TEXT.agents(s.legacyAgents.join(", ")) }));
    }
    const row = h("div", { class: "row" });
    if (s.uninstaller) {
      row.append(
        h("button", {
          text: TEXT.uninstall,
          onclick: async () => {
            feedback.textContent = "";
            try {
              await Bridge.migrationUninstallCoucou();
            } catch (err) {
              feedback.textContent = errorText(err);
              await refresh();
            }
          },
        }),
        h("span", { class: "hint", text: TEXT.uninstallHint }),
      );
    }
    if (row.childElementCount > 0) body.append(row);
    if (s.oldKeys > 0) {
      let armed = false;
      const button = h("button", { class: "danger", text: TEXT.clearKeys }) as HTMLButtonElement;
      button.onclick = async () => {
        if (!armed) {
          armed = true;
          button.textContent = TEXT.clearAgain;
          return;
        }
        button.disabled = true;
        try {
          await Bridge.migrationClearOldKeys();
          feedback.textContent = TEXT.cleared;
          await refresh();
        } catch (err) {
          feedback.textContent = errorText(err);
          button.disabled = false;
        }
      };
      body.append(h("div", { class: "hint", text: TEXT.oldKeys(s.oldKeys) }), h("div", { class: "row" }, button));
    }
  };
  draw(first);
  refreshShown = refresh;
  return section;
}

// «Sonidos» — ARIA's own sounds (recibido, pregunta, listo, error), played by
// Rust's play_sound (Aerys) at settings.soundVolume. Uses the settings Rust
// already carries, soundEnabled and soundVolume, saved with saveSettingsMerged.

import { playSound } from "../core/botcmds";
import type { Settings } from "../core/state";
import { h } from "../views/dom";
import { t } from "../i18n/i18n";

export function soundsSection(ctx: { get: () => Settings; save: () => Promise<void> }): HTMLElement {
  const s0 = ctx.get();
  const toggle = h("button", { class: s0.soundEnabled !== false ? "switch on" : "switch", title: t("ARIA sounds") });
  const volume = h("input", {
    type: "range", min: "0", max: "0.2", step: "0.01",
    value: String(Math.max(0, Math.min(0.2, typeof s0.soundVolume === "number" ? s0.soundVolume : 0.15))),
  }) as HTMLInputElement;
  volume.disabled = s0.soundEnabled === false;

  toggle.addEventListener("click", () => {
    const on = !toggle.classList.contains("on");
    toggle.classList.toggle("on", on);
    volume.disabled = !on;
    ctx.get().soundEnabled = on;
    void ctx.save();
    if (on) playSound("listo", ctx.get());
  });
  volume.addEventListener("input", () => {
    ctx.get().soundVolume = Number(volume.value);
    void ctx.save();
  });
  // A sample at the new volume once the slider is let go.
  volume.addEventListener("change", () => playSound("recibido", ctx.get()));

  return h("section", {},
    h("h2", {}, h("span", { text: t("Sounds") })),
    h("div", { class: "row" }, h("label", { text: t("ARIA sounds") }), toggle),
    h("div", { class: "row" }, h("label", { text: t("Volume") }), volume),
    h("div", { class: "hint", text: t("They play when a Bot gets your message, asks you something, finishes or fails.") }),
  );
}

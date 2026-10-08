// «Sonidos» — Coucou's own sounds (recibido, pregunta, listo, error), played by
// Rust's play_sound (Aerys) at settings.soundVolume. Uses the settings Rust
// already carries, soundEnabled and soundVolume, saved with saveSettingsMerged.

import { playSound } from "../core/botcmds";
import type { Settings } from "../core/state";
import { h } from "../views/dom";

export function soundsSection(ctx: { get: () => Settings; save: () => Promise<void> }): HTMLElement {
  const s0 = ctx.get();
  const toggle = h("button", { class: s0.soundEnabled !== false ? "switch on" : "switch", title: "Sonidos de Coucou" });
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
    h("h2", {}, h("span", { text: "Sonidos" })),
    h("div", { class: "row" }, h("label", { text: "Sonidos de Coucou" }), toggle),
    h("div", { class: "row" }, h("label", { text: "Volumen" }), volume),
    h("div", { class: "hint", text: "Suenan cuando un Bot recibe tu mensaje, te pregunta algo, termina o falla." }),
  );
}

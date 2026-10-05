// save_settings replaces the whole settings.json. The front only knows its own
// fields and may hold a stale copy (Rust writes `voices` on its own, and may
// add fields the front has never heard of). So every save reads what Rust has
// now, lays the front's known fields over it, and sends the full object back.

import { Bridge } from "./bridge";
import { DEFAULT_SETTINGS, type Settings } from "./state";

/** Fields only Rust writes: always taken from its current copy. */
const RUST_OWNED = new Set<string>(["voices"]);

export async function saveSettingsMerged(local: Settings): Promise<void> {
  const current = (await Bridge.boot())?.settings as Record<string, unknown> | undefined;
  if (!current) {
    await Bridge.saveSettings(local);
    return;
  }
  const merged: Record<string, unknown> = { ...current };
  const mine = local as unknown as Record<string, unknown>;
  for (const key of Object.keys(DEFAULT_SETTINGS)) {
    if (!RUST_OWNED.has(key) && key in mine) merged[key] = mine[key];
  }
  // Keep the caller's copy in step with what was saved.
  for (const key of RUST_OWNED) if (key in current) mine[key] = current[key];
  await Bridge.saveSettings(merged as unknown as Settings);
}

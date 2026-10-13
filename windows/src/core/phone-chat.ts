// Settings → Android phone → chat: which models the phone may use. Pure helpers, so the
// rules can be tested without a webview. Rust cleans the list again (phone_link/chat_backend.rs);
// this only builds it. The ids are "provider/model", as the phone is told them.

import { AI_PROVIDERS, type AiProvider, type Settings } from "./state";

/** Most models the user can allow; Rust caps the list at the same number. */
export const MAX_ALLOWED = 50;

/** ARIA's providers the phone may chat with: Cursor runs its own agent loop, not a plain chat. */
export const PHONE_PROVIDERS: readonly AiProvider[] = AI_PROVIDERS.filter((p) => p.id !== "cursor");

export function modelId(provider: string, model: string): string {
  return `${provider}/${model}`;
}

/** The provider part of an id, or null when it is not one the phone may use. */
export function providerOf(id: string): string | null {
  const at = id.indexOf("/");
  const provider = at > 0 ? id.slice(0, at) : "";
  return PHONE_PROVIDERS.some((p) => p.id === provider) && id.length > at + 1 ? provider : null;
}

/** `allowed` with `id` ticked or unticked: no duplicates, order kept, capped. */
export function toggleModel(allowed: readonly string[], id: string, on: boolean): string[] {
  const rest = allowed.filter((x) => x !== id);
  if (!on) return rest;
  return [...rest, id].slice(0, MAX_ALLOWED);
}

/**
 * The providers worth offering: a cloud provider that has a key, a model server always (it has a
 * default address). `hasKey` maps a provider id to "its key is saved".
 */
export function usableProviders(_settings: Settings, hasKey: Readonly<Record<string, boolean>>): AiProvider[] {
  return PHONE_PROVIDERS.filter((p) => (p.key ? hasKey[p.id] === true : true));
}

/** The ids of `allowed` that belong to `provider`, without the provider part. */
export function modelsOf(allowed: readonly string[], provider: string): string[] {
  return allowed.filter((id) => providerOf(id) === provider).map((id) => id.slice(provider.length + 1));
}

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  consumeCursorMigrationFocusRequest,
  cursorSdkPreset,
  providerNeedsCursorSdkMigration,
  providerSetupGroups,
  savedProviderMessage,
} from "./provider-settings.js";
import type { KnownProvider } from "./types.js";

const providerSetupSource = readFileSync(
  new URL("./provider-settings-card.ts", import.meta.url),
  "utf8",
);

const knownProvider = (fields: Partial<KnownProvider>): KnownProvider =>
  ({ display_name: fields.id ?? "", ...fields }) as KnownProvider;

describe("provider setup", () => {
  it("legacy Cursor providers select the SDK migration path", () => {
    expect(providerNeedsCursorSdkMigration({ kind: "cursor-cli" })).toBe(true);
    expect(providerNeedsCursorSdkMigration({ kind: "cursor-sdk" })).toBe(false);
    expect(
      cursorSdkPreset([
        knownProvider({ id: "codex", kind: "codex-app-server" }),
        knownProvider({ id: "cursor", kind: "cursor-sdk" }),
      ])?.id,
    ).toBe("cursor");
  });

  it("provider saves do not report credential-free setup as ready", () => {
    expect(savedProviderMessage("Cursor (Agent SDK)", { has_credentials: false })).toBe(
      "Saved Cursor (Agent SDK), but provider credentials are still required",
    );
    expect(savedProviderMessage("Cursor (Agent SDK)", { has_credentials: true })).toBe(
      "Saved Cursor (Agent SDK)",
    );
  });

  it("provider setup keeps local presets available in review-ui", () => {
    const cursor = knownProvider({
      id: "cursor",
      kind: "cursor-sdk",
      category: "subscription",
      auth: "api-key",
    });
    const hosted = knownProvider({
      id: "openai",
      kind: "openai-compat",
      category: "api",
      auth: "api-key",
    });
    const local = knownProvider({
      id: "ollama",
      kind: "openai-compat",
      category: "local",
      auth: "none",
    });
    const groups = providerSetupGroups([cursor, hosted, local]);

    expect(groups.subscriptionProviders).toEqual([cursor]);
    expect(groups.apiProviders).toEqual([hosted, local]);
  });

  it("API key inputs expose their guidance to assistive technology", () => {
    for (const id of ["subscription-api-key-guidance", "provider-api-key-guidance"]) {
      expect(providerSetupSource).toMatch(new RegExp(`aria-describedby="${id}"`, "u"));
      expect(providerSetupSource).toMatch(new RegExp(`<small id="${id}">`, "u"));
    }
  });

  it("Cursor migration focus is one-shot and wired to the subscription API-key input", () => {
    let focusCount = 0;
    const input = {
      focus: () => {
        focusCount += 1;
      },
    };
    const cursor = { kind: "cursor-sdk", auth: "api-key" } as const;
    let request = consumeCursorMigrationFocusRequest(1, cursor, input);
    expect(request).toBe(0);
    expect(focusCount).toBe(1);

    request = consumeCursorMigrationFocusRequest(request, cursor, input);
    expect(request).toBe(0);
    expect(focusCount).toBe(1);
    expect(
      consumeCursorMigrationFocusRequest(1, { kind: "claude-cli", auth: "cli" }, input),
    ).toBe(1);
    expect(consumeCursorMigrationFocusRequest(1, cursor, null)).toBe(1);
    expect(focusCount).toBe(1);
    expect(providerSetupSource).toMatch(
      /consumeCursorMigrationFocusRequest\(\s*focusRequest,\s*selectedSubscription,\s*this\.subscriptionApiKeyInput\.value \?\? null,/u,
    );
  });
});

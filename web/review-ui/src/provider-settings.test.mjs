import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  cursorSdkPreset,
  providerNeedsCursorSdkMigration,
  providerSetupGroups,
  savedProviderMessage,
} from "./provider-settings.ts";

const providerSetupSource = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");

test("legacy Cursor providers select the SDK migration path", () => {
  assert.equal(providerNeedsCursorSdkMigration({ kind: "cursor-cli" }), true);
  assert.equal(providerNeedsCursorSdkMigration({ kind: "cursor-sdk" }), false);
  assert.equal(
    cursorSdkPreset([
      { id: "codex", kind: "codex-app-server" },
      { id: "cursor", kind: "cursor-sdk" },
    ]).id,
    "cursor",
  );
});

test("provider saves do not report credential-free setup as ready", () => {
  assert.equal(
    savedProviderMessage("Cursor (Agent SDK)", { has_credentials: false }),
    "Saved Cursor (Agent SDK), but provider credentials are still required",
  );
  assert.equal(
    savedProviderMessage("Cursor (Agent SDK)", { has_credentials: true }),
    "Saved Cursor (Agent SDK)",
  );
});

test("provider setup keeps local presets available in review-ui", () => {
  const cursor = { id: "cursor", kind: "cursor-sdk", category: "subscription", auth: "api-key" };
  const hosted = { id: "openai", kind: "openai-compat", category: "api", auth: "api-key" };
  const local = { id: "ollama", kind: "openai-compat", category: "local", auth: "none" };
  const groups = providerSetupGroups([cursor, hosted, local]);

  assert.deepEqual(groups.subscriptionProviders, [cursor]);
  assert.deepEqual(groups.apiProviders, [hosted, local]);
});

test("API key inputs expose their guidance to assistive technology", () => {
  for (const id of ["subscription-api-key-guidance", "provider-api-key-guidance"]) {
    assert.match(providerSetupSource, new RegExp(`aria-describedby="${id}"`, "u"));
    assert.match(providerSetupSource, new RegExp(`<small id="${id}">`, "u"));
  }
});

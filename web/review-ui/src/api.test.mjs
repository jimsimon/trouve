import assert from "node:assert/strict";
import test from "node:test";

import { saveReviewer } from "./api.ts";

const persona = {
  id: "existing-reviewer",
  display_name: "Existing reviewer",
  system_prompt: "Inspect carefully.",
  allowed_tools: ["read_file"],
  read_only: true,
};

test("new reviewers cannot overwrite an existing derived persona id", async (context) => {
  const requests = [];
  context.mock.method(globalThis, "fetch", async (url, init) => {
    requests.push({ url, init });
    return new Response(JSON.stringify([{ persona, origin: "custom" }]));
  });

  await assert.rejects(
    saveReviewer({
      id: "",
      name: "Existing reviewer",
      prompt: "Replacement prompt",
    }),
    /already exists/u,
  );
  assert.equal(requests.length, 1);
  assert.equal(requests[0].url, "/v1/persona-infos");
});

test("existing reviewers preserve persona policy when updated", async (context) => {
  const requests = [];
  context.mock.method(globalThis, "fetch", async (url, init) => {
    requests.push({ url, init });
    return url === "/v1/persona-infos"
      ? new Response(JSON.stringify([{ persona, origin: "custom" }]))
      : new Response(null, { status: 204 });
  });

  await saveReviewer({
    id: persona.id,
    name: "Renamed reviewer",
    prompt: "Updated prompt",
  });

  assert.equal(requests.length, 2);
  assert.equal(requests[1].url, "/v1/personas/existing-reviewer");
  assert.equal(requests[1].init.method, "PUT");
  assert.deepEqual(JSON.parse(requests[1].init.body), {
    display_name: "Renamed reviewer",
    group: "reviewer",
    system_prompt: "Updated prompt",
    allowed_tools: ["read_file"],
    read_only: true,
    default_permission_mode: null,
    default_model: null,
    default_thinking_level: null,
  });
});

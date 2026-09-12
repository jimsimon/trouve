import { afterEach, describe, expect, it, vi } from "vitest";

import { createReviewApi } from "./api.js";

const persona = {
  id: "existing-reviewer",
  display_name: "Existing reviewer",
  system_prompt: "Inspect carefully.",
  allowed_tools: ["read_file"],
  read_only: true,
};

interface RecordedRequest {
  url: string;
  init: RequestInit | undefined;
}

const recordFetch = (
  respond: (url: string) => Response,
): RecordedRequest[] => {
  const requests: RecordedRequest[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      requests.push({ url, init });
      return respond(url);
    }),
  );
  return requests;
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("review API client", () => {
  it("new reviewers cannot overwrite an existing derived persona id", async () => {
    const requests = recordFetch(
      () => new Response(JSON.stringify([{ persona, origin: "custom" }])),
    );

    await expect(
      createReviewApi().saveReviewer({
        id: "",
        name: "Existing reviewer",
        prompt: "Replacement prompt",
      }),
    ).rejects.toThrow(/already exists/u);
    expect(requests).toHaveLength(1);
    expect(requests[0]?.url).toBe("/v1/persona-infos");
  });

  it("existing reviewers preserve persona policy when updated", async () => {
    const requests = recordFetch((url) =>
      url === "/v1/persona-infos"
        ? new Response(JSON.stringify([{ persona, origin: "custom" }]))
        : new Response(null, { status: 204 }),
    );

    await createReviewApi().saveReviewer({
      id: persona.id,
      name: "Renamed reviewer",
      prompt: "Updated prompt",
    });

    expect(requests).toHaveLength(2);
    expect(requests[1]?.url).toBe("/v1/personas/existing-reviewer");
    expect(requests[1]?.init?.method).toBe("PUT");
    expect(JSON.parse(String(requests[1]?.init?.body))).toEqual({
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

  it("prefixes every request with the configured base URL", async () => {
    const requests = recordFetch(
      () =>
        new Response(JSON.stringify({ jobs: [] }), {
          headers: { "x-trouve-event-cursor": "7" },
        }),
    );
    const api = createReviewApi({ baseUrl: "http://127.0.0.1:7878/" });

    await api.getJobs("running", "");
    const snapshot = await api.getDashboard();

    expect(requests.map(({ url }) => url)).toEqual([
      "http://127.0.0.1:7878/v1/code-review/jobs?limit=250&status=running",
      "http://127.0.0.1:7878/v1/code-review",
    ]);
    expect(snapshot.cursor).toBe(7);
  });
});

import { describe, expect, it } from "vitest";

import type {
  ProtocolGithubPrList,
  ProtocolPrInfo,
} from "../services/protocol-client.js";
import {
  pullRequestsForSession,
  sessionPullRequestBadge,
  visibleSessionPullRequestBadge,
} from "./session-pull-request-badge.js";

const pr = (
  number: number,
  overrides: Partial<ProtocolPrInfo> = {},
): ProtocolPrInfo => ({
  host: "github.com",
  repository: "trouve-ai/trouve",
  workspace_id: "ws_1",
  number,
  url: `https://github.com/trouve-ai/trouve/pull/${number}`,
  title: `Pull request ${number}`,
  state: "open",
  draft: false,
  base: "main",
  head: "feature",
  checks: [],
  reviews: [],
  author: "octocat",
  ...overrides,
});

describe("session pull-request navigation badges", () => {
  it("matches only the exact workspace and branch and sorts open PRs first", () => {
    const list: ProtocolGithubPrList = {
      host: "github.com",
      viewer: "octocat",
      prs: [
        pr(4, { state: "merged" }),
        pr(3, { workspace_id: "ws_other" }),
        pr(2, { head: "other" }),
        pr(1),
      ],
    };
    expect(pullRequestsForSession(
      { workspaceId: "ws_1", branch: "feature" },
      [list],
    ).map(({ number }) => number)).toEqual([1, 4]);
  });

  it("requires every open PR to be GitHub-ready", () => {
    expect(sessionPullRequestBadge([
      pr(7, { merge_state_status: "clean" }),
      pr(6, { merge_state_status: "HAS_HOOKS" }),
    ])).toMatchObject({ tone: "ready", count: 2 });

    expect(sessionPullRequestBadge([
      pr(7, { merge_state_status: "clean" }),
      pr(6, { draft: true, merge_state_status: "clean" }),
    ])).toMatchObject({
      tone: "blocked",
      tooltip: "2 pull requests\n#7 · Ready to merge\n#6 · Unable to merge · Draft",
    });
  });

  it("uses terminal state when no open pull request remains", () => {
    expect(sessionPullRequestBadge([pr(5, { state: "merged" })])).toEqual({
      tone: "merged",
      label: "Merged",
      count: 1,
      tooltip: "Pull request\n#5 · Merged",
    });
    expect(sessionPullRequestBadge([pr(4, { state: "closed" })])?.tone).toBe("closed");
    expect(sessionPullRequestBadge([])).toBeUndefined();
  });

  it("uses the same session-state precedence in every navigator", () => {
    const ready = [pr(7, { merge_state_status: "clean" })];
    expect(visibleSessionPullRequestBadge(ready, "idle", false)?.tone).toBe("ready");
    expect(visibleSessionPullRequestBadge(ready, "done", true)?.tone).toBe("ready");
    expect(visibleSessionPullRequestBadge(ready, "running", true)).toBeUndefined();
    expect(visibleSessionPullRequestBadge(ready, "attention", true)).toBeUndefined();
    expect(visibleSessionPullRequestBadge(ready, "failed", true)).toBeUndefined();
    expect(visibleSessionPullRequestBadge(ready, "done", false)).toBeUndefined();
  });
});

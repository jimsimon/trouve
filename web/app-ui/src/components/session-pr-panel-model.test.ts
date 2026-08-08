import { describe, expect, it } from "vitest";

import type { ProtocolPrInfo } from "../services/protocol-client.js";
import {
  canMergePr,
  checkSummary,
  createPrRequest,
  githubIntegrationConfigured,
  mergeabilitySummary,
  mergeMethod,
  pullRequestsListHref,
  reviewSummary,
  safeSessionPrHref,
  sessionPullRequestsListHref,
} from "./session-pr-panel-model.js";

const pullRequest = (overrides: Partial<ProtocolPrInfo> = {}): ProtocolPrInfo => ({
  host: "github.com",
  repository: "trouve-ai/trouve",
  workspace_id: "ws_1",
  number: 42,
  url: "https://github.com/trouve-ai/trouve/pull/42",
  title: "Preserve frontend behavior",
  state: "open",
  draft: false,
  base: "main",
  head: "trouve/web-ui",
  head_sha: "abc123",
  checks: [],
  reviews: [],
  trouve_review: null,
  author: "octocat",
  requested_reviewers: [],
  comments: 0,
  last_comment_at: null,
  mergeable: true,
  merge_state_status: "clean",
  merged_at: null,
  ...overrides,
});

describe("session pull-request panel model", () => {
  it("accepts configuration from github.com or an enterprise host", () => {
    expect(githubIntegrationConfigured({ configured: true, source: "oauth" })).toBe(true);
    expect(githubIntegrationConfigured({
      configured: false,
      source: "",
      hosts: [
        { host: "github.com", configured: false, source: "", oauth_available: true, removable: false },
        { host: "github.example.com", configured: true, source: "oauth", oauth_available: true, removable: true },
      ],
    })).toBe(true);
    expect(githubIntegrationConfigured({
      configured: false,
      source: "",
      hosts: [{ host: "github.com", configured: false, source: "", oauth_available: true, removable: false }],
    })).toBe(false);
  });

  it("accepts only credential-free HTTPS links", () => {
    expect(safeSessionPrHref("https://github.com/org/repo/pull/1")).toBe(
      "https://github.com/org/repo/pull/1",
    );
    expect(safeSessionPrHref("http://github.com/org/repo/pull/1")).toBeUndefined();
    expect(safeSessionPrHref("https://token@github.com/org/repo/pull/1")).toBeUndefined();
    expect(safeSessionPrHref("javascript:alert(1)")).toBeUndefined();
    expect(safeSessionPrHref("/relative")).toBeUndefined();
  });

  it("resolves the repository pull-request list from an associated PR", () => {
    expect(pullRequestsListHref(pullRequest())).toBe(
      "https://github.com/trouve-ai/trouve/pulls",
    );
    expect(pullRequestsListHref(pullRequest({
      url: "https://github.example.com/platform/trouve/pull/42/files?diff=split#discussion",
    }))).toBe("https://github.example.com/platform/trouve/pulls");
    expect(pullRequestsListHref(pullRequest({
      url: "https://github.com/trouve-ai/trouve/issues/42",
    }))).toBeUndefined();
    expect(pullRequestsListHref(pullRequest({
      url: "http://github.com/trouve-ai/trouve/pull/42",
    }))).toBeUndefined();
  });

  it("falls back to another PR mapped to the session workspace", () => {
    const workspacePr = pullRequest({ head: "another-branch" });
    const lists = [{ host: "github.com", viewer: "octocat", prs: [workspacePr] }];
    expect(sessionPullRequestsListHref([], "ws_1", lists)).toBe(
      "https://github.com/trouve-ai/trouve/pulls",
    );
    expect(sessionPullRequestsListHref([], "ws_other", lists)).toBeUndefined();
  });

  it("summarizes passing, pending, and failing checks", () => {
    expect(checkSummary(pullRequest())).toEqual({ label: "No checks reported", tone: "muted" });
    expect(checkSummary(pullRequest({
      checks: [
        { name: "build", status: "completed", conclusion: "success" },
        { name: "test", status: "in_progress", conclusion: null },
      ],
    }))).toEqual({ label: "1 passing · 1 pending", tone: "pending" });
    expect(checkSummary(pullRequest({
      checks: [
        { name: "build", status: "completed", conclusion: "success" },
        { name: "test", status: "completed", conclusion: "failure" },
      ],
    }))).toEqual({ label: "1 failing · 1 passing", tone: "failed" });
  });

  it("prioritizes requested changes and outstanding reviewers", () => {
    expect(reviewSummary(pullRequest({
      reviews: [
        { reviewer: "alice", state: "approved" },
        { reviewer: "bob", state: "changes_requested" },
      ],
    }))).toEqual({ label: "1 requesting changes · 1 approved", tone: "failed" });
    expect(reviewSummary(pullRequest({
      reviews: [{ reviewer: "alice", state: "approved" }],
      requested_reviewers: ["carol"],
    }))).toEqual({ label: "1 approved · 1 awaiting review", tone: "pending" });
  });

  it("describes mergeability and prevents unsafe merge states", () => {
    expect(mergeabilitySummary(pullRequest())).toEqual({ label: "Ready to merge", tone: "ready" });
    expect(canMergePr(pullRequest())).toBe(true);
    expect(mergeabilitySummary(pullRequest({ mergeable: false, merge_state_status: "dirty" }))).toEqual({
      label: "Merge conflicts",
      tone: "failed",
    });
    expect(canMergePr(pullRequest({ mergeable: false }))).toBe(false);
    expect(canMergePr(pullRequest({ mergeable: true, merge_state_status: "blocked" }))).toBe(false);
    expect(canMergePr(pullRequest({ mergeable: null, merge_state_status: "dirty" }))).toBe(false);
    expect(canMergePr(pullRequest({ draft: true }))).toBe(false);
    expect(canMergePr(pullRequest({ state: "closed" }))).toBe(false);
  });

  it("builds normalized create requests and validates merge methods", () => {
    expect(createPrRequest({
      title: "  Web frontend  ",
      body: "  Details  ",
      base: " main ",
      draft: true,
    })).toEqual({ title: "Web frontend", body: "Details", base: "main", draft: true });
    expect(createPrRequest({ title: "Title", body: "", base: "", draft: false })).toEqual({
      title: "Title",
      body: "",
      draft: false,
    });
    expect(() => createPrRequest({ title: "   ", body: "", base: "", draft: false })).toThrow();
    expect(mergeMethod("squash")).toBe("squash");
    expect(mergeMethod("rebase")).toBe("rebase");
    expect(mergeMethod("unexpected")).toBe("merge");
  });
});

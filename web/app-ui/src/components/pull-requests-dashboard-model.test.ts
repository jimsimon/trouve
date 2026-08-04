import { describe, expect, it } from "vitest";

import type {
  ProtocolGithubPrList,
  ProtocolPrInfo,
} from "../services/protocol-client.js";
import type { SessionListItem } from "../state/app-store.js";
import {
  buildPullRequestGroups,
  classifyPullRequest,
  movePullRequestGroup,
  pullRequestApprovalPill,
  pullRequestCheckPill,
  pullRequestHumanAge,
  pullRequestMergePill,
  pullRequestRepositories,
  reconcilePullRequestGroupOrder,
  reorderPullRequestGroup,
} from "./pull-requests-dashboard-model.js";

const now = new Date("2026-07-18T12:00:00Z");

const pr = (patch: Partial<ProtocolPrInfo> = {}): ProtocolPrInfo => ({
  host: "github.com",
  repository: "acme/app",
  workspace_id: "ws_1",
  number: 42,
  url: "https://github.com/acme/app/pull/42",
  title: "Make it better",
  state: "open",
  draft: false,
  base: "main",
  head: "feature",
  checks: [],
  reviews: [],
  author: "author",
  requested_reviewers: [],
  comments: 0,
  last_comment_at: null,
  mergeable: null,
  merged_at: null,
  ...patch,
});

const passingCheck = () => ({
  name: "test",
  status: "completed",
  conclusion: "success",
});

describe("pull request dashboard model", () => {
  it("classifies every established dashboard category", () => {
    expect(classifyPullRequest(pr({ requested_reviewers: ["viewer"] }), "viewer", now))
      .toBe("review-requested");
    expect(classifyPullRequest(pr({ draft: true }), "viewer", now)).toBe("drafts");
    expect(classifyPullRequest(pr(), "viewer", now)).toBe("needs-reviewers");
    expect(classifyPullRequest(pr({ requested_reviewers: ["reviewer"] }), "viewer", now))
      .toBe("pending-review");
    expect(classifyPullRequest(pr({
      checks: [passingCheck()],
      reviews: [{ reviewer: "reviewer", state: "approved" }],
    }), "viewer", now)).toBe("ready-to-merge");
    expect(classifyPullRequest(pr({
      checks: [{ name: "test", status: "completed", conclusion: "failure" }],
    }), "viewer", now)).toBe("needs-attention");
    expect(classifyPullRequest(pr({
      state: "merged",
      merged_at: "2026-07-17T13:00:00Z",
    }), "viewer", now)).toBe("recently-merged");
    expect(classifyPullRequest(pr({
      state: "merged",
      merged_at: "2026-07-17T11:00:00Z",
    }), "viewer", now)).toBeUndefined();
    expect(classifyPullRequest(pr({ state: "closed" }), "viewer", now)).toBeUndefined();
  });

  it("preserves conflict priority while keeping conflicted drafts in Drafts", () => {
    expect(classifyPullRequest(pr({
      mergeable: false,
      requested_reviewers: ["viewer"],
    }), "viewer", now)).toBe("needs-attention");
    expect(classifyPullRequest(pr({ draft: true, mergeable: false }), "viewer", now))
      .toBe("drafts");
  });

  it("matches the Slint status-pill semantics and latest reviewer verdict", () => {
    expect(pullRequestCheckPill(pr())).toEqual({ label: "no checks", tone: "neutral" });
    expect(pullRequestCheckPill(pr({
      checks: [{ name: "test", status: "in_progress", conclusion: null }],
    }))).toEqual({ label: "checks running", tone: "warning" });
    expect(pullRequestCheckPill(pr({ checks: [passingCheck()] })))
      .toEqual({ label: "checks passing", tone: "ok" });

    const reviewed = pr({
      reviews: [
        { reviewer: "reviewer", state: "approved" },
        { reviewer: "reviewer", state: "changesrequested" },
      ],
    });
    expect(pullRequestApprovalPill(reviewed))
      .toEqual({ label: "changes requested", tone: "danger" });
    expect(pullRequestApprovalPill(pr({
      reviews: [{ reviewer: "reviewer", state: "approved" }],
      requested_reviewers: ["second"],
    }))).toEqual({ label: "review pending", tone: "warning" });
    expect(pullRequestMergePill(pr({ mergeable: false })))
      .toEqual({ label: "merge conflicts", tone: "danger" });
    expect(pullRequestMergePill(pr({ state: "merged" }))).toBeUndefined();
  });

  it("reconciles and reorders the seven stable groups", () => {
    const reconciled = reconcilePullRequestGroupOrder([
      "ready-to-merge",
      "missing",
      "drafts",
      "ready-to-merge",
    ]);
    expect(reconciled.changed).toBe(true);
    expect(reconciled.order).toEqual([
      "ready-to-merge",
      "drafts",
      "review-requested",
      "needs-reviewers",
      "pending-review",
      "needs-attention",
      "recently-merged",
    ]);
    expect(movePullRequestGroup(reconciled.order, "drafts", -1).slice(0, 2))
      .toEqual(["drafts", "ready-to-merge"]);
    expect(reorderPullRequestGroup(
      reconciled.order,
      "recently-merged",
      "drafts",
      false,
    ).slice(0, 3)).toEqual(["ready-to-merge", "recently-merged", "drafts"]);
  });

  it("builds filtered, newest-first rows with chat and first-party review data", () => {
    const lists: readonly ProtocolGithubPrList[] = [{
      host: "github.com",
      viewer: "viewer",
      prs: [
        pr({ number: 41, title: "Older" }),
        pr({
          number: 43,
          title: "Reviewed",
          head: "trouve/reviewed",
          comments: 1,
          last_comment_at: "2026-07-18T11:15:00Z",
          trouve_review: {
            bot_login: "trouve-review[bot]",
            job_id: "job-1",
            status: "completed",
            summary: "One issue found.",
            prompt_for_agents: "Fix every confirmed issue.",
            review_url: "https://github.com/acme/app/pull/43#pullrequestreview-1",
            findings: [
              {
                id: "finding-open",
                job_id: "job-1",
                path: "src/main.ts",
                line: 17,
                side: "RIGHT",
                severity: "high",
                body: "Handle the failure.",
                status: "open",
                prompt_for_agents: "Fix the failure path.",
              },
              {
                id: "finding-fixed",
                job_id: "job-1",
                path: "src/old.ts",
                line: 2,
                side: "RIGHT",
                severity: "low",
                body: "Already handled.",
                status: "fixed",
              },
            ],
          },
        }),
        pr({
          host: "github.example.com",
          repository: "other/service",
          workspace_id: "ws_2",
          number: 5,
          title: "Other repository",
        }),
      ],
    }];
    const sessions: readonly SessionListItem[] = [{
      id: "se_1",
      workspaceId: "ws_1",
      title: "Reviewed branch",
      branch: "trouve/reviewed",
      archived: false,
      active: false,
      attention: "none",
      outcome: "idle",
      latestThreadId: "th_1",
      updatedAt: "2026-07-18T11:30:00Z",
      state: "idle",
      unread: false,
    }];

    expect(pullRequestRepositories(lists)).toEqual([
      "github.com/acme/app",
      "github.example.com/other/service",
    ]);
    const groups = buildPullRequestGroups(lists, sessions, {
      repository: "github.com/acme/app",
      now,
    });
    const needsReviewers = groups.find(({ key }) => key === "needs-reviewers")!;
    expect(needsReviewers.pullRequests.map(({ number }) => number)).toEqual([43, 41]);
    expect(needsReviewers.pullRequests[0]).toMatchObject({
      hasChat: true,
      commentsLabel: "1 comment",
      lastComment: "last comment 45 mins ago",
      reviewSummary: "One issue found.",
      reviewPrompt: "Fix every confirmed issue.",
      reviewFindings: [{
        location: "src/main.ts:17",
        severity: "high",
        prompt: "Fix the failure path.",
      }],
    });
  });

  it("formats approachable relative ages", () => {
    expect(pullRequestHumanAge("2026-07-18T11:15:00Z", now)).toBe("45 mins ago");
    expect(pullRequestHumanAge("2026-07-17T12:00:00Z", now)).toBe("1 day ago");
  });
});

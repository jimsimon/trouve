import { describe, expect, it } from "vitest";

import {
  canRetryFinalEditor,
  codeReviewSettingsDraft,
  codeReviewSettingsRequest,
  codeReviewStatusClass,
  groupCodeReviewJobs,
  moveReviewGroup,
  orderReviewJobGroups,
  reconcileReviewGroupOrder,
  reorderReviewGroup,
  reviewGroupRepositoryKeys,
  safeCodeReviewHref,
  type ReviewJobSummary,
} from "./code-review-dashboard-model.js";

const job = (
  id: string,
  repository: string,
  status: string,
  createdAt: string,
): ReviewJobSummary => ({ id, repository, status, created_at: createdAt });

describe("code-review dashboard model", () => {
  it("groups by repository with active and newest jobs first", () => {
    const groups = groupCodeReviewJobs(
      [
        job("old", "trouve/zeta", "failed", "2026-08-01T10:00:00Z"),
        job("new", "trouve/zeta", "succeeded", "2026-08-01T12:00:00Z"),
        job("running", "trouve/zeta", "running", "2026-08-01T09:00:00Z"),
        job("queued", "trouve/alpha", "queued", "2026-08-01T11:00:00Z"),
      ],
      "all",
    );

    expect(groups.map(({ repository }) => repository)).toEqual([
      "trouve/alpha",
      "trouve/zeta",
    ]);
    expect(groups[1]?.jobs.map(({ id }) => id)).toEqual(["running", "new", "old"]);
  });

  it("filters exact statuses without hiding future statuses from the all view", () => {
    const jobs = [
      job("queued", "trouve/app", "queued", "2026-08-01T11:00:00Z"),
      job("future", "trouve/app", "paused", "2026-08-01T12:00:00Z"),
    ];

    expect(groupCodeReviewJobs(jobs, "queued")[0]?.jobs.map(({ id }) => id)).toEqual([
      "queued",
    ]);
    expect(groupCodeReviewJobs(jobs, "all")[0]?.jobs.map(({ id }) => id)).toContain(
      "future",
    );
    expect(codeReviewStatusClass("paused")).toBe("unknown");
  });

  it("reconciles saved repository groups like the desktop order", () => {
    expect(reconcileReviewGroupOrder(
      ["trouve/zeta", "missing/repository", "trouve/zeta"],
      ["trouve/alpha", "trouve/zeta", "trouve/new"],
    )).toEqual({
      order: ["trouve/zeta", "trouve/alpha", "trouve/new"],
      changed: true,
    });
    expect(reconcileReviewGroupOrder(
      ["trouve/zeta", "trouve/alpha"],
      ["trouve/alpha", "trouve/zeta"],
    )).toEqual({
      order: ["trouve/zeta", "trouve/alpha"],
      changed: false,
    });
  });

  it("applies repository order without changing attention-first job order", () => {
    const groups = groupCodeReviewJobs(
      [
        job("terminal", "trouve/zeta", "failed", "2026-08-01T12:00:00Z"),
        job("queued", "trouve/zeta", "queued", "2026-08-01T10:00:00Z"),
        job("running", "trouve/zeta", "running", "2026-08-01T09:00:00Z"),
        job("alpha", "trouve/alpha", "succeeded", "2026-08-01T11:00:00Z"),
      ],
      "all",
    );
    const ordered = orderReviewJobGroups(groups, ["trouve/zeta", "trouve/alpha"]);

    expect(ordered.map(({ repository }) => repository)).toEqual([
      "trouve/zeta",
      "trouve/alpha",
    ]);
    expect(ordered[0]?.jobs.map(({ id }) => id)).toEqual([
      "running",
      "queued",
      "terminal",
    ]);
  });

  it("supports before/after drops and moves against visible filtered groups", () => {
    const order = ["a", "b", "c", "d"];
    expect(reorderReviewGroup(order, "a", "c", true)).toEqual([
      "b",
      "c",
      "a",
      "d",
    ]);
    expect(reorderReviewGroup(order, "d", "b", false)).toEqual([
      "a",
      "d",
      "b",
      "c",
    ]);
    expect(moveReviewGroup(order, ["a", "c", "d"], "c", -1)).toEqual([
      "c",
      "a",
      "b",
      "d",
    ]);
    expect(moveReviewGroup(order, ["a", "c", "d"], "c", 1)).toEqual([
      "a",
      "b",
      "d",
      "c",
    ]);
    expect(moveReviewGroup(order, ["a", "c", "d"], "a", -1)).toBe(order);
    expect(reorderReviewGroup(order, "missing", "a", false)).toBe(order);
  });

  it("retains configured repositories while their recent-job group is absent", () => {
    const groups = groupCodeReviewJobs([
      job("active", "trouve/zeta", "running", "2026-08-01T12:00:00Z"),
    ], "all");
    expect(reviewGroupRepositoryKeys(groups, [
      "trouve/search",
      "trouve/zeta",
      " trouve/app ",
      "trouve/search",
    ])).toEqual([
      "trouve/zeta",
      "trouve/app",
      "trouve/search",
    ]);
  });

  it("allows only absolute credential-free HTTPS links", () => {
    expect(safeCodeReviewHref("https://github.com/trouve-ai/trouve/pull/42")).toBe(
      "https://github.com/trouve-ai/trouve/pull/42",
    );
    expect(safeCodeReviewHref("http://github.com/trouve-ai/trouve/pull/42")).toBeUndefined();
    expect(safeCodeReviewHref("javascript:alert(1)")).toBeUndefined();
    expect(safeCodeReviewHref("/relative/pull/42")).toBeUndefined();
    expect(safeCodeReviewHref("https://user:secret@github.com/pull/42")).toBeUndefined();
  });

  it("offers scoped final-editor retry only after every reviewer finishes", () => {
    expect(canRetryFinalEditor({
      status: "failed",
      progress: { completed_reviewers: 3, total_reviewers: 3 },
    })).toBe(true);
    expect(canRetryFinalEditor({
      status: "cancelled",
      progress: { completed_reviewers: 3, total_reviewers: 3 },
    })).toBe(true);
    expect(canRetryFinalEditor({
      status: "failed",
      progress: { completed_reviewers: 2, total_reviewers: 3 },
    })).toBe(false);
    expect(canRetryFinalEditor({
      status: "succeeded",
      progress: { completed_reviewers: 3, total_reviewers: 3 },
    })).toBe(false);
  });

  it("converts minute settings to protocol seconds", () => {
    expect(
      codeReviewSettingsRequest({
        maxParallel: "4",
        totalMinutes: "20",
        reviewerMinutes: "12",
        coordinatorMinutes: "6",
      }),
    ).toEqual({
      max_parallel_reviews: 4,
      total_timeout_seconds: 1_200,
      reviewer_timeout_seconds: 720,
      coordinator_timeout_seconds: 360,
    });
    const wholeSeconds = {
      max_parallel_reviews: 2,
      total_timeout_seconds: 100,
      reviewer_timeout_seconds: 100,
      coordinator_timeout_seconds: 100,
    };
    const roundTripDraft = codeReviewSettingsDraft(wholeSeconds);
    expect(roundTripDraft).toMatchObject({
      totalMinutes: String(100 / 60),
      reviewerMinutes: String(100 / 60),
      coordinatorMinutes: String(100 / 60),
    });
    expect(codeReviewSettingsRequest(roundTripDraft)).toMatchObject(wholeSeconds);
  });

  it("rejects invalid concurrency and deadlines", () => {
    expect(() =>
      codeReviewSettingsRequest({
        maxParallel: "1.5",
        totalMinutes: "10",
        reviewerMinutes: "5",
        coordinatorMinutes: "3",
      }),
    ).toThrow(/whole number from 1 to 32/);
    expect(() =>
      codeReviewSettingsRequest({
        maxParallel: "2",
        totalMinutes: "10",
        reviewerMinutes: "11",
        coordinatorMinutes: "3",
      }),
    ).toThrow(/Reviewer timeout cannot exceed/);
    expect(() =>
      codeReviewSettingsRequest({
        maxParallel: "2",
        totalMinutes: "10",
        reviewerMinutes: "5",
        coordinatorMinutes: "0.01",
      }),
    ).toThrow(/positive number of whole seconds/);
  });
});

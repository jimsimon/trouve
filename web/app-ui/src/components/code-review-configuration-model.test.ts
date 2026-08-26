import { describe, expect, it } from "vitest";

import type {
  ProtocolCodeReviewRepository,
  ProtocolGithubAppStatus,
  ProtocolReviewerProfile,
} from "../services/protocol-client.js";
import {
  repositoryDraft,
  repositoryUpdateRequest,
  reviewerDraft,
  sanitizeGithubAppStatus,
  type RepositoryDraft,
} from "./code-review-configuration-model.js";

const repository: ProtocolCodeReviewRepository = {
  installation_id: 42,
  repository: "trouve-ai/trouve",
  private: true,
  mode: "automatic",
  model: "provider/coordinator",
  coordinator_thinking_level: "high",
  router_model: "provider/router",
  router_thinking_level: "low",
  analyst_model: "provider/analyst",
  analyst_thinking_level: "minimal",
  prompt: " Preserve architectural boundaries. ",
  reviewer_ids: ["correctness", "security"],
  routing_mode: "additive",
  semantic_routing: true,
  included_reviewer_ids: ["correctness"],
  excluded_reviewer_ids: ["style"],
  reviewer_overrides: [
    {
      reviewer_id: "security",
      model: "provider/security",
      thinking_level: "2048",
      prompt_mode: "append",
      prompt: " Check trust boundaries. ",
    },
  ],
};

describe("code-review repository requests", () => {
  it("normalizes every server field into an editable draft", () => {
    expect(repositoryDraft(repository)).toEqual({
      mode: "automatic",
      model: "provider/coordinator",
      coordinatorThinkingLevel: "high",
      routerModel: "provider/router",
      routerThinkingLevel: "low",
      analystModel: "provider/analyst",
      analystThinkingLevel: "minimal",
      prompt: " Preserve architectural boundaries. ",
      reviewerIds: ["correctness", "security"],
      routingMode: "additive",
      semanticRouting: true,
      includedReviewerIds: ["correctness"],
      excludedReviewerIds: ["style"],
      reviewerOverrides: [
        {
          reviewerId: "security",
          model: "provider/security",
          thinkingLevel: "2048",
          promptMode: "append",
          prompt: " Check trust boundaries. ",
        },
      ],
    });
  });

  it("faithfully resends required fields, selections, and overrides", () => {
    const draft: RepositoryDraft = {
      ...repositoryDraft(repository),
      model: " provider/new-coordinator ",
      coordinatorThinkingLevel: " ",
      routerModel: " provider/new-router ",
      routerThinkingLevel: "medium",
      analystModel: " provider/new-analyst ",
      analystThinkingLevel: " ",
      prompt: " Review generated files too. ",
      reviewerIds: ["security", "security", "correctness"],
      includedReviewerIds: ["correctness", "performance"],
      excludedReviewerIds: ["style", "docs"],
      reviewerOverrides: [
        {
          reviewerId: "security",
          model: "",
          thinkingLevel: " 4096 ",
          promptMode: "replace",
          prompt: " Focus only on exploitable issues. ",
        },
      ],
    };

    expect(repositoryUpdateRequest(repository, draft)).toEqual({
      installation_id: 42,
      repository: "trouve-ai/trouve",
      mode: "automatic",
      model: "provider/new-coordinator",
      coordinator_thinking_level: null,
      router_model: "provider/new-router",
      router_thinking_level: "medium",
      analyst_model: "provider/new-analyst",
      analyst_thinking_level: null,
      prompt: " Review generated files too. ",
      reviewer_ids: ["security", "correctness"],
      routing_mode: "additive",
      semantic_routing: true,
      included_reviewer_ids: ["correctness", "performance"],
      excluded_reviewer_ids: ["style", "docs"],
      reviewer_overrides: [
        {
          reviewer_id: "security",
          model: null,
          thinking_level: "4096",
          prompt_mode: "replace",
          prompt: " Focus only on exploitable issues. ",
        },
      ],
    });
  });

  it("disables semantic routing for manual selection and validates enabled reviews", () => {
    const draft = repositoryDraft(repository);
    expect(repositoryUpdateRequest(repository, {
      ...draft,
      routingMode: "manual",
      semanticRouting: true,
    }).semantic_routing).toBe(false);

    expect(() => repositoryUpdateRequest(repository, {
      ...draft,
      model: "",
    })).toThrow("coordinator model required");
    expect(() => repositoryUpdateRequest(repository, {
      ...draft,
      routingMode: "manual",
      reviewerIds: [],
    })).toThrow("manual reviewer required");

    expect(repositoryUpdateRequest(repository, {
      ...draft,
      mode: "off",
      model: "",
      routingMode: "manual",
      reviewerIds: [],
    }).model).toBeNull();
  });
});

describe("GitHub App diagnostics", () => {
  it("retains only the presence of a diagnostic, never its text", () => {
    const status: ProtocolGithubAppStatus = {
      configured: true,
      app_id: 123,
      bot_login: "trouve-review[bot]",
      last_error: "sensitive vendor diagnostic",
    };
    const sanitized = sanitizeGithubAppStatus(status);

    expect(sanitized.needsAttention).toBe(true);
    expect(sanitized.status.last_error).toBe("");
    expect(JSON.stringify(sanitized)).not.toContain("sensitive vendor diagnostic");
  });

  it("does not request attention without a configured diagnostic", () => {
    for (const status of [{
      configured: true,
      app_id: 123,
      bot_login: "trouve-review[bot]",
      last_error: "",
    }, {
      configured: false,
      app_id: 0,
      bot_login: "",
      last_error: "stale diagnostic",
    }] satisfies ProtocolGithubAppStatus[]) {
      expect(sanitizeGithubAppStatus(status).needsAttention).toBe(false);
    }
  });
});

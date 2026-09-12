import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("session pull-request workspace", () => {
  const source = readFileSync(
    new URL("./session-pr-panel.ts", import.meta.url),
    "utf8",
  );

  it("centers the no-PR message in the space remaining below the toolbar", () => {
    expect(source).toContain(".panel {");
    expect(source).toContain("display: flex;");
    expect(source).toContain(".pr-empty { flex: 1; }");
    expect(source).toContain('class="empty pr-empty"');
    expect(source).toContain("No pull requests are associated with this session yet.");
  });

  it("keeps compact create and browser actions beside the visible heading", () => {
    expect(source).toContain('class="pr-toolbar-heading"');
    expect(source).toContain('<h2 id="session-pr-title">Pull requests</h2>');
    expect(source).toContain('aria-label="Create pull request"');
    expect(source).toContain('fontAwesomeIcon("code-pull-request")');
    expect(source).toContain('aria-label="Open repository pull requests on GitHub"');
    expect(source).toContain('fontAwesomeIcon("arrow-up-right-from-square")');
    expect(source).not.toContain(">Create PR</button>");
  });

  it("keeps multiple associated pull requests selectable", () => {
    expect(source).toContain('aria-label="Pull request"');
    expect(source).toContain("void this.#selectPr(");
    expect(source).toContain("${prs.map((pr) => html`");
  });

  it("uses icon-only copy and browser actions for the selected pull request", () => {
    expect(source).toContain('title="Copy pull request URL"');
    expect(source).toContain('aria-label="Copy pull request URL"');
    expect(source).toContain('fontAwesomeIcon("copy")');
    expect(source).toContain('title="Open on GitHub"');
    expect(source).toContain('aria-label="Open on GitHub"');
    expect(source).not.toContain(">Open on GitHub</button>");
  });

  it("provides the GitHub pull-request page sections", () => {
    expect(source).toContain('["conversation", "Conversation"');
    expect(source).toContain('["checks", "Checks"');
    expect(source).toContain('["commits", "Commits"');
    expect(source).toContain('["files", "Files"');
    expect(source).toContain('role="tablist"');
    expect(source).toContain('role="tabpanel"');
    expect(source).toContain("nextHorizontalTabIndex(event.key, index, tabs.length)");
    expect(source).toContain("rovingTabIndex(index, selectedIndex, tabs.length)");
    expect(source).toContain("services.protocol.sessionPrDetail(sessionId, number, section)");
    expect(source).toContain("detailSectionForTab");
    expect(source).toContain(
      "this.#loadDetail(selected.number, detailSectionForTab(this.#activeTab))",
    );
  });

  it("loads only the selected changed file into the diff workspace", () => {
    expect(source).toContain('class="pr-file-tree"');
    expect(source).toContain('role="tree"');
    expect(source).toContain("services.protocol.sessionPrFileDiff(sessionId, number, path)");
    expect(source).toContain("<trouve-diff-view");
    expect(source).toContain("void this.#selectFile(file.path)");
    expect(source).toContain("GitHub could not provide a bounded text preview");
  });

  it("supports reviews, conversations, metadata, merge queue, and stacks", () => {
    expect(source).toContain("Add a comment");
    expect(source).toContain("Submit a review");
    expect(source).toContain("request_reviewers");
    expect(source).toContain('name="bots"');
    expect(source).toContain("resolve_review_thread");
    expect(source).toContain("dismiss_review");
    expect(source).toContain("set_file_viewed");
    expect(source).toContain("set_merge_queue");
    expect(source).toContain("set_auto_merge");
    expect(source).toContain("Apply labels");
    expect(source).toContain("Apply assignees");
    expect(source).toContain("Stack · ${stack.size}");
  });

  it("uses the durable session projection without a panel-owned manual refresh", () => {
    const integration = source.indexOf(
      "const integration = await services.protocol.githubIntegration()",
    );
    const unconfigured = source.indexOf("if (!this.#githubConfigured)", integration);

    expect(integration).toBeGreaterThan(-1);
    expect(unconfigured).toBeGreaterThan(integration);
    expect(source).not.toContain("services.protocol.sessionPrs(sessionId)");
    expect(source).not.toContain("await services.protocol.refreshGithubPrs(true)");
    expect(source).toContain("this.#syncProjectedSelection()");
    expect(source).toContain("this.#scheduleLoadRetry()");
    expect(source).toContain("Connect GitHub to manage this session's pull requests");
    expect(source).toContain("Set up GitHub integration");
    expect(source).toContain('navigate({ kind: "settings", section: "integrations" })');
  });

  it("treats repository integration prerequisites as setup instead of retry errors", () => {
    expect(source).toContain(
      "cause instanceof ProtocolClientError",
    );
    expect(source).toContain("cause.status === 400");
    expect(source).toContain("this.#repositorySetupRequired = true");
    expect(source).toContain("this.#clearPrStateForSetup()");
  });

  it("pauses detail retries until missing GitHub permissions are re-authorized", () => {
    expect(source).toContain('cause.code === "github_reauthentication_required"');
    expect(source).toContain("this.#githubReauthenticationRequired = true");
    expect(source).toContain("Re-authenticate GitHub");
    expect(source).toContain(
      "this.#detailRetryTimer !== undefined || this.#githubReauthenticationRequired",
    );
  });

  it("keeps account sign-in separate from workspace repository setup", () => {
    expect(source).toContain(
      "Connect this workspace to a GitHub repository",
    );
    expect(source).toContain("A GitHub account is connected");
    expect(source).toContain("Open Terminal");
    expect(source).toContain('inspection: "terminal"');
    expect(source).toContain("GitHub settings");
  });
});

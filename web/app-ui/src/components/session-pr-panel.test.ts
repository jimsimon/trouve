import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("session pull-request panel empty-state parity", () => {
  const source = readFileSync(
    new URL("./session-pr-panel.ts", import.meta.url),
    "utf8",
  );

  it("centers the no-PR message in the space remaining below the toolbar", () => {
    expect(source).toContain(".panel { display: flex;");
    expect(source).toContain(".pr-empty { flex: 1;");
    expect(source).toContain('class="empty pr-empty"');
    expect(source).toContain("No pull requests for this session's branch yet.");
  });

  it("keeps compact create and browser actions beside the visible heading", () => {
    expect(source).toContain('class="pr-toolbar-heading"');
    expect(source).toContain('<h2 id="session-pr-title">Pull requests</h2>');
    expect(source).toContain('aria-label="Create pull request"');
    expect(source).toContain('fontAwesomeIcon("code-pull-request")');
    expect(source).toContain('aria-label="Open Pull Requests"');
    expect(source).toContain('fontAwesomeIcon("arrow-up-right-from-square")');
    expect(source).not.toContain(">Create PR</button>");
  });

  it("shows GitHub setup before making an authenticated pull-request request", () => {
    const integration = source.indexOf(
      "const integration = await services.protocol.githubIntegration()",
    );
    const unconfigured = source.indexOf("if (!this.#githubConfigured)", integration);
    const pullRequests = source.indexOf(
      "const prs = await services.protocol.sessionPrs(sessionId)",
      unconfigured,
    );

    expect(integration).toBeGreaterThan(-1);
    expect(unconfigured).toBeGreaterThan(integration);
    expect(pullRequests).toBeGreaterThan(unconfigured);
    expect(source.slice(unconfigured, pullRequests)).toContain("return;");
    expect(source).toContain("Connect GitHub to see this session's pull requests");
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

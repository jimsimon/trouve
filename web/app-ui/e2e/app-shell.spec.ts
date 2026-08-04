import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const pr = {
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
  checks: [{ name: "test", status: "completed", conclusion: "success" }],
  reviews: [{ reviewer: "reviewer", state: "approved" }],
  author: "octocat",
  requested_reviewers: [],
  comments: 0,
  last_comment_at: null,
  merge_state_status: "clean",
  mergeable: true,
  merged_at: null,
};

const githubSnapshot = {
  cursor: 5,
  scope: "server",
  ts: "2026-08-04T08:00:00Z",
  type: "github.pull_requests_updated",
  pull_requests: { host: "github.com", viewer: "octocat", prs: [pr] },
};

const installFixtureEventSource = async (page: Page): Promise<void> => {
  await page.addInitScript((event) => {
    class FixtureEventSource {
      static readonly CONNECTING = 0;
      static readonly OPEN = 1;
      static readonly CLOSED = 2;
      readonly url: string;
      readonly withCredentials = false;
      readyState = FixtureEventSource.CONNECTING;
      onopen: ((event: Event) => void) | null = null;
      onmessage: ((event: MessageEvent<string>) => void) | null = null;
      onerror: ((event: Event) => void) | null = null;

      constructor(url: string | URL) {
        this.url = String(url);
        setTimeout(() => {
          if (this.readyState === FixtureEventSource.CLOSED) return;
          this.readyState = FixtureEventSource.OPEN;
          this.onopen?.(new Event("open"));
          this.onmessage?.(new MessageEvent("message", {
            data: JSON.stringify(event),
            lastEventId: String(event.cursor),
          }));
        }, 10);
      }

      close(): void {
        this.readyState = FixtureEventSource.CLOSED;
      }
    }
    Object.defineProperty(globalThis, "EventSource", {
      configurable: true,
      value: FixtureEventSource,
    });
  }, githubSnapshot);
};

const installProtocolFixtures = async (page: Page): Promise<void> => {
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const key = `${request.method()} ${url.pathname}`;
    const responses: Record<string, unknown> = {
      "GET /v1/info": {
        name: "trouve-server",
        version: "3.7.0",
        protocol_version: "2.9",
        online: true,
      },
      "GET /v1/session-summaries": {
        cursor: 5,
        summaries: [{
          session_id: "se_1",
          workspace_id: "ws_1",
          archived: false,
          active: false,
          attention: "none",
          outcome: "idle",
          latest_cursor: 5,
          updated_at: "2026-08-04T08:00:00Z",
        }],
      },
      "GET /v1/sessions": [{
        id: "se_1",
        workspace_id: "ws_1",
        title: "Protocol ingress",
        branch: "feature",
        worktree_path: "/tmp/protocol-ingress",
        base_ref: "main",
        created_at: "2026-08-04T08:00:00Z",
      }],
      "GET /v1/threads": [],
      "GET /v1/sessions/se_1/prs": [pr],
      "GET /v1/workspaces": [{ id: "ws_1", name: "trouve", path: "/src/trouve" }],
      "GET /v1/providers": {
        default_model: "",
        default_permission_mode: "ask",
        default_thinking_level: null,
        providers: [],
      },
      "GET /v1/models": [],
      "GET /v1/mode-infos": [
        {
          origin: "builtin",
          mode: {
            id: "code",
            display_name: "Code",
            system_prompt: "Implement the user's request by editing files.",
          },
        },
        {
          origin: "builtin",
          mode: {
            id: "plan",
            display_name: "Plan",
            system_prompt: "Explore the workspace and produce a concrete plan.",
            read_only: true,
          },
        },
        {
          origin: "builtin",
          mode: {
            id: "review",
            display_name: "Review",
            system_prompt: "Review the current changes for correctness.",
            read_only: true,
          },
        },
      ],
      "GET /v1/automations": [],
      "GET /v1/automations/templates": [],
      "GET /v1/integrations/github": {
        configured: true,
        source: "oauth",
        oauth_available: true,
        hosts: [{
          host: "github.com",
          configured: true,
          oauth_available: true,
          removable: false,
          source: "oauth",
        }],
      },
    };
    if (key === "POST /v1/github/prs/refresh") {
      await route.fulfill({ status: 204 });
      return;
    }
    const response = responses[key];
    if (response === undefined) {
      await route.fulfill({
        status: 501,
        json: { code: "fixture_missing", message: `No browser fixture for ${key}` },
      });
      return;
    }
    await route.fulfill({ json: response });
  });
};

test.beforeEach(async ({ page }) => {
  await installFixtureEventSource(page);
  await installProtocolFixtures(page);
});

test("one durable pull-request projection drives the session badge and dashboard", async ({ browserName, page }, testInfo) => {
  test.skip(testInfo.project.name.startsWith("mobile"), "the session badge belongs to the desktop navigation surface");
  await page.goto("/");

  await expect(page.getByLabel("Pull request. #42 · Ready to merge")).toBeVisible();

  await page.getByRole("tab", { name: /Pull Requests/u }).click();
  await expect(page.locator("trouve-session-pr-panel").getByText("Make it better", {
    exact: true,
  })).toBeVisible();

  await page.getByRole("button", { name: "Pull Requests", exact: true }).first().click();

  await expect(page).toHaveURL(/\/reviews$/u);
  await expect(page.getByRole("heading", { name: "Pull Requests", exact: true })).toBeVisible();
  await expect(page.getByText("Make it better", { exact: true })).toBeVisible();

  if (browserName === "chromium") {
    await page.locator("trouve-pull-requests-dashboard").evaluate((dashboard) => {
      const status = dashboard.shadowRoot?.querySelector<HTMLElement>(".refresh-status");
      if (status !== null && status !== undefined) status.style.visibility = "hidden";
    });
    const screenshot = await page.locator("main").screenshot({ animations: "disabled" });
    expect(screenshot).toMatchSnapshot("pull-requests-dashboard.png", {
      maxDiffPixelRatio: 0.01,
    });
  }

  const result = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
    .analyze();
  expect(result.violations.filter(({ impact }) =>
    impact === "serious" || impact === "critical"
  )).toEqual([]);
});

test("the account pull-request dashboard does not require a registered workspace", async ({ page }) => {
  await page.route("**/v1/workspaces", async (route) => {
    await route.fulfill({ json: [] });
  });
  await page.goto("/reviews");

  await expect(page.getByRole("heading", { name: "Pull Requests", exact: true })).toBeVisible();
  await expect(page.getByText("Make it better", { exact: true })).toBeVisible();
  await expect(page.getByText(/Open a workspace first/u)).toHaveCount(0);
});

test("an unconfigured GitHub integration stays aligned with the session empty state", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name.startsWith("mobile"), "the desktop session pane owns this integration handoff");
  await page.route("**/v1/integrations/github", async (route) => {
    await route.fulfill({
      json: {
        configured: false,
        source: "",
        oauth_available: true,
        hosts: [{
          host: "github.com",
          configured: false,
          oauth_available: true,
          removable: false,
          source: "",
        }],
      },
    });
  });
  await page.goto("/");

  await page.getByRole("tab", { name: /Pull Requests/u }).click();
  await expect(page.getByText("Connect GitHub to see this session's pull requests", {
    exact: true,
  })).toBeVisible();
  await page.getByRole("button", { name: "Set up GitHub integration" }).click();

  await expect(page).toHaveURL(/\/settings\/integrations$/u);
  await expect(page.getByRole("heading", { name: "Integrations", exact: true })).toBeVisible();
  await expect(page.getByText("○ not configured", { exact: true })).toBeVisible();
});

test("the responsive dashboard remains available from the mobile PWA layout", async ({ page }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("mobile"), "mobile layout qualification case");
  await page.goto("/reviews");

  await expect(page.getByRole("heading", { name: "Pull Requests", exact: true })).toBeVisible();
  await expect(page.getByText("Make it better", { exact: true })).toBeVisible();

  const result = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
    .analyze();
  expect(result.violations.filter(({ impact }) =>
    impact === "serious" || impact === "critical"
  )).toEqual([]);
});

test("management screens retain their reviewed desktop geometry", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop-chromium", "desktop Chromium owns the management-screen pixel baseline");

  for (const screen of [
    { path: "/settings/general", heading: "General", snapshot: "settings-general.png" },
    { path: "/settings/modes", heading: "Modes & Models", snapshot: "settings-modes-models.png" },
    { path: "/automations", heading: "Automations", snapshot: "automations-empty.png" },
  ]) {
    await page.goto(screen.path);
    await expect(page.getByRole("heading", { name: screen.heading, exact: true })).toBeVisible();

    const result = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
      .analyze();
    expect(result.violations.filter(({ impact }) =>
      impact === "serious" || impact === "critical"
    )).toEqual([]);

    await expect(page.locator("main").first()).toHaveScreenshot(screen.snapshot);
  }
});

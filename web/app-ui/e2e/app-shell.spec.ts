import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page, type Route } from "@playwright/test";

import { stabilizeVisualFonts } from "./visual-fonts";

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
  let namingSettings = {
    derive_branch_name_from_session_title: false,
    title_model_load_behavior: "auto",
    title_model_resource_policy: "cpu_ram_only",
    title_model: {
      state: "not_installed",
      detail: "Built-in naming rules are active.",
      runtime_installed: false,
      model_downloaded: false,
    },
  };
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const key = `${request.method()} ${url.pathname}`;
    const responses: Record<string, unknown> = {
      "GET /v1/info": {
        name: "trouve-server",
        version: "3.7.0",
        protocol_version: "3.14",
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
    if (key === "GET /v1/config/git-worktrees") {
      await route.fulfill({
        headers: { "x-trouve-event-cursor": "6" },
        json: namingSettings,
      });
      return;
    }
    if (key === "PUT /v1/config/git-worktrees") {
      const update = request.postDataJSON() as Partial<typeof namingSettings>;
      namingSettings = { ...namingSettings, ...update };
      await route.fulfill({
        headers: { "x-trouve-event-cursor": "7" },
        json: namingSettings,
      });
      return;
    }
    if (key === "GET /v1/server-projection") {
      // Exercise the cursor-zero compatibility path; the fixture EventSource
      // supplies the durable GitHub projection used by these shell tests.
      await route.fulfill({
        status: 404,
        json: { code: "not_found", message: "fixture uses legacy projection replay" },
      });
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

test("session navigation uses compact one-line rows without branch names", async ({ page }, testInfo) => {
  await page.goto("/");
  if (testInfo.project.name.startsWith("mobile")) {
    await page.getByRole("button", { name: "Sessions", exact: true }).click();
  }

  const row = page.locator(".session-row").filter({ hasText: "Protocol ingress" });
  await expect(row).toBeVisible();
  await expect(row).not.toContainText("feature");
  await expect(row.locator(".session-copy small")).toHaveCount(0);
  await expect(row).toHaveCSS("height", "34px");
  await expect(row.locator(".session-copy strong")).toHaveCSS("white-space", "nowrap");
  const wrapper = row.locator("..");
  const age = row.locator(".session-age");
  const actions = wrapper.getByRole("button", { name: "Actions for Protocol ingress" });
  await expect(age).toHaveText(/^(?:now|\d+[mhdy])$/u);
  if (testInfo.project.name.startsWith("mobile")) {
    await expect(age).toHaveCSS("opacity", "0");
    await expect(actions).toHaveCSS("opacity", "1");
  } else {
    await expect(age).toHaveCSS("opacity", "1");
    await expect(actions).toHaveCSS("opacity", "0");
    await wrapper.hover();
    await expect(age).toHaveCSS("opacity", "0");
    await expect(actions).toHaveCSS("opacity", "1");
    await page.mouse.move(0, 0);
    await actions.focus();
    await expect(age).toHaveCSS("opacity", "0");
    await expect(actions).toHaveCSS("opacity", "1");

    const workspace = page.locator(".workspace-row").filter({ hasText: "trouve" }).first();
    const workspaceOrder = workspace.locator(".workspace-order-controls");
    const workspaceActions = workspace.locator(".workspace-actions-wrap");
    await page.mouse.move(0, 0);
    await expect(workspaceOrder).toHaveCSS("opacity", "0");
    await expect(workspaceActions).toHaveCSS("opacity", "0");
    await workspace.hover();
    await expect(workspaceOrder).toHaveCSS("opacity", "1");
    await expect(workspaceActions).toHaveCSS("opacity", "1");
    await page.mouse.move(0, 0);
    await workspaceOrder.getByRole("button").focus();
    await expect(workspaceOrder).toHaveCSS("opacity", "1");
    await expect(workspaceActions).toHaveCSS("opacity", "1");
  }
});

test("background session updates preserve command-palette scrolling", async ({ page }, testInfo) => {
  test.skip(
    testInfo.project.name.startsWith("mobile"),
    "the desktop keyboard palette owns this scrolling contract",
  );
  await page.goto("/");
  await page.addStyleTag({
    content: ".command-palette-results { max-height: 80px !important; }",
  });
  await page.keyboard.press("Control+k");

  const palette = page.locator("trouve-command-palette");
  const results = palette.locator("#command-palette-results");
  await expect(page.locator("#command-palette-dialog")).toBeVisible();
  await expect(palette.locator(".command-palette-state")).toHaveCount(0);
  await expect(palette.locator(".command-palette-current")).toHaveText("Current");
  const navigationPrBadge = page.locator(".session-row .session-pr-badge.ready");
  const palettePrBadge = palette.locator(".session-pr-badge.ready");
  await expect(palettePrBadge).toHaveAttribute("title", /#42 · Ready to merge/u);
  await expect(palettePrBadge.locator('[data-font-awesome-icon="code-pull-request"]'))
    .toBeVisible();
  await expect(navigationPrBadge.locator(".trouve-icon")).toHaveCSS("font-size", "15px");
  expect(await palettePrBadge.evaluate((element) => getComputedStyle(element).color))
    .toBe(await navigationPrBadge.evaluate((element) => getComputedStyle(element).color));
  await expect(palette.locator(".command-palette-copy small").filter({ hasText: "Current" }))
    .toHaveCount(0);
  const scrolledTop = await results.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
    element.dispatchEvent(new Event("scroll"));
    return element.scrollTop;
  });
  expect(scrolledTop).toBeGreaterThan(0);

  // Session/chat signals request the same component update while the palette
  // is open. A background render must not reveal its still-selected first row.
  await palette.evaluate(async (element) => {
    const component = element as HTMLElement & {
      requestUpdate(): void;
      readonly updateComplete: Promise<boolean>;
    };
    component.requestUpdate();
    await component.updateComplete;
  });

  await expect.poll(() => results.evaluate((element) => element.scrollTop))
    .toBe(scrolledTop);
});

test("one durable pull-request projection drives the session badge and dashboard", async ({ browserName, page }, testInfo) => {
  test.skip(testInfo.project.name.startsWith("mobile"), "the session badge belongs to the desktop navigation surface");
  await page.goto("/");

  await expect(page.getByLabel("Pull request. #42 · Ready to merge")).toBeVisible();

  await page.getByRole("tab", { name: /Pull Requests/u }).click();
  const sessionPanel = page.locator("trouve-session-pr-panel");
  await expect(sessionPanel.getByText("Make it better", {
    exact: true,
  })).toBeVisible();

  const headingActions = sessionPanel.locator(".pr-toolbar-heading");
  await expect(headingActions.getByRole("heading", { name: "Pull requests", exact: true }))
    .toBeVisible();
  const create = headingActions.getByRole("button", { name: "Create pull request" });
  await expect(sessionPanel.getByRole("button", { name: "Create PR", exact: true }))
    .toHaveCount(0);
  await expect(create.locator('[data-font-awesome-icon="code-pull-request"]')).toBeVisible();
  await create.click();
  await expect(sessionPanel.getByRole("heading", { name: "Create pull request", exact: true }))
    .toBeVisible();
  await create.click();

  const openPullRequests = headingActions.getByRole("button", { name: "Open Pull Requests" });
  await expect(openPullRequests).toBeEnabled();
  await expect(openPullRequests.locator(
    '[data-font-awesome-icon="arrow-up-right-from-square"]',
  )).toBeVisible();
  await page.evaluate(() => {
    const state = globalThis as typeof globalThis & { openedPullRequestsHref?: string };
    state.openedPullRequestsHref = "";
    globalThis.addEventListener("trouve-open-external", (event) => {
      state.openedPullRequestsHref = (event as CustomEvent<{ readonly href: string }>).detail.href;
      event.stopImmediatePropagation();
    }, { capture: true, once: true });
  });
  await openPullRequests.click();
  await expect.poll(() => page.evaluate(() =>
    (globalThis as typeof globalThis & { openedPullRequestsHref?: string })
      .openedPullRequestsHref,
  )).toBe("https://github.com/acme/app/pulls");

  await page.getByRole("button", { name: "Pull Requests", exact: true }).first().click();

  await expect(page).toHaveURL(/\/reviews$/u);
  await expect(page.getByRole("heading", { name: "Pull Requests", exact: true })).toBeVisible();
  await expect(page.getByText("Make it better", { exact: true })).toBeVisible();

  if (browserName === "chromium") {
    await stabilizeVisualFonts(page);
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
  const status = page.locator(".integration-status");
  await expect(status).toHaveText(/not configured/u);
  await expect(status.locator('[data-font-awesome-icon="circle-dot"]')).toBeVisible();
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

test("Sessions & Chat settings preserve grouping and branch-naming choices", async ({ page }) => {
  await page.goto("/settings/chat");

  await expect(page.getByRole("heading", { name: "Sessions & Chat", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Git & Worktrees", exact: true }))
    .toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Session naming", exact: true }))
    .toBeVisible();
  const branchNames = page.getByLabel("Use session names in branch names");
  await expect(branchNames).not.toBeChecked();
  await expect(page.getByText(/new branches use a compact name such as trouve\/abc123/u))
    .toBeVisible();
  const sequentialToggle = page.getByLabel("Collapse sequential tool calls.");
  await expect(sequentialToggle).toBeChecked();
  const toggle = page.getByLabel("Collapse thinking output with tool calls.");
  await expect(toggle).not.toBeChecked();
  await expect(page.getByText(
    "When off, thought output stays visible at the top level and separates the collapsible tool-call groups on either side.",
    { exact: true },
  )).toBeVisible();
  const compactionToggle = page.getByLabel("Collapse context compaction with tool calls.");
  await expect(compactionToggle).not.toBeChecked();
  await expect(page.getByText(
    "When off, context compaction remains a visible top-level boundary and separates the collapsible tool-call groups on either side.",
    { exact: true },
  )).toBeVisible();

  await page.locator('label[for="settings-collapse-sequential-tools"]').click();
  await expect(sequentialToggle).not.toBeChecked();
  await expect(toggle).toBeDisabled();
  await expect(compactionToggle).toBeDisabled();
  await page.locator('label[for="settings-collapse-sequential-tools"]').click();
  await expect(sequentialToggle).toBeChecked();
  await expect(toggle).toBeEnabled();
  await expect(compactionToggle).toBeEnabled();

  await page.getByText("Collapse thinking output with tool calls.", { exact: true }).click();
  await expect(toggle).toBeChecked();
  await page.getByText("Collapse context compaction with tool calls.", { exact: true }).click();
  await expect(compactionToggle).toBeChecked();
  const branchUpdate = page.waitForRequest((request) =>
    request.method() === "PUT" &&
    new URL(request.url()).pathname === "/v1/config/git-worktrees"
  );
  await branchNames.click();
  await expect(branchNames).toBeChecked();
  await expect((await branchUpdate).postDataJSON()).toMatchObject({
    derive_branch_name_from_session_title: true,
  });
  await expect.poll(() => page.evaluate(() => localStorage.getItem("trouve.chat.v1")))
    .toContain('"collapseSequentialToolCalls":true');
  await expect.poll(() => page.evaluate(() => localStorage.getItem("trouve.chat.v1")))
    .toContain('"collapseThinkingWithTools":true');
  await expect.poll(() => page.evaluate(() => localStorage.getItem("trouve.chat.v1")))
    .toContain('"collapseCompactionWithTools":true');

  await page.reload();
  await expect(page.getByLabel("Collapse sequential tool calls.")).toBeChecked();
  await expect(page.getByLabel("Collapse thinking output with tool calls.")).toBeChecked();
  await expect(page.getByLabel("Collapse context compaction with tool calls.")).toBeChecked();
  await expect(page.getByLabel("Use session names in branch names")).toBeChecked();
});

test("Settings reopens the last screen until the app restarts", async ({ page }) => {
  await page.goto("/settings/general");

  await page.getByRole("button", { name: "Sessions & Chat", exact: true }).click();
  await expect(page).toHaveURL(/\/settings\/chat$/u);
  await expect(page.getByRole("heading", { name: "Sessions & Chat", exact: true })).toBeVisible();

  await page.getByRole("button", { name: /Close/u }).click();
  await page.getByRole("button", { name: "Settings", exact: true }).first().click();
  await expect(page).toHaveURL(/\/settings\/chat$/u);
  await expect(page.getByRole("heading", { name: "Sessions & Chat", exact: true })).toBeVisible();

  await page.getByRole("button", { name: /Close/u }).click();
  await page.reload();
  await page.getByRole("button", { name: "Settings", exact: true }).first().click();
  await expect(page).toHaveURL(/\/settings$/u);
  await expect(page.getByRole("heading", { name: "General", exact: true })).toBeVisible();
});

test("Modes & Models uses provider-qualified model labels", async ({ page }) => {
  await page.route("**/v1/models", async (route) => {
    await route.fulfill({
      json: [{
        id: "codex/gpt-5.6-sol",
        display_name: "GPT-5.6 Sol",
        context_window: 500_000,
        supports_tools: true,
        options_schema: {},
      }],
    });
  });
  await page.goto("/settings/modes");

  await expect(page.getByRole("heading", { name: "Modes & Models", exact: true }))
    .toBeVisible();
  await expect(page.getByRole("option", { name: "codex/gpt-5.6-sol", exact: true }))
    .toHaveCount(4);
  await expect(page.getByRole("option", { name: "GPT-5.6 Sol", exact: true }))
    .toHaveCount(0);
});

test("automation create and edit preserve model and thinking choices", async ({ page }) => {
  await page.route("**/v1/models", async (route) => {
    await route.fulfill({
      json: [{
        id: "codex/gpt-5.6-sol",
        display_name: "GPT-5.6 Sol",
        context_window: 500_000,
        supports_tools: true,
        options_schema: {
          type: "object",
          properties: {
            reasoning_effort: {
              type: "string",
              enum: ["low", "medium", "high", "max", "ultra"],
              default: "medium",
            },
          },
        },
      }],
    });
  });

  const requests: Array<Record<string, unknown>> = [];
  let savedAutomation: Record<string, unknown> | undefined;
  const automationMutation = async (route: Route) => {
    const method = route.request().method();
    if (method === "GET" && new URL(route.request().url()).pathname === "/v1/automations") {
      await route.fulfill({ json: savedAutomation === undefined ? [] : [savedAutomation] });
      return;
    }
    if (method !== "POST" && method !== "PUT") {
      await route.fallback();
      return;
    }
    const request = route.request().postDataJSON() as Record<string, unknown>;
    requests.push(request);
    savedAutomation = {
      id: "auto_1",
      ...request,
      next_run_at: null,
      last_run_at: null,
      last_session_id: null,
      last_error: "",
      created_at: "2026-08-06T08:00:00Z",
    };
    await route.fulfill({ json: savedAutomation });
  };
  await page.route("**/v1/automations", automationMutation);
  await page.route("**/v1/automations/**", automationMutation);

  await page.goto("/automations");
  await page.getByRole("button", { name: "New automation", exact: false }).click();
  await page.getByLabel("Automation name").fill("Nightly checks");
  await page.getByLabel("Prompt to send").fill("Run all checks");

  const model = page.getByRole("combobox", { name: "Automation model" });
  const modelPicker = page.locator("trouve-automations-screen trouve-model-picker");
  await expect(model).toBeEnabled();
  await model.click();
  await expect(model).toHaveAttribute("aria-expanded", "true");
  await modelPicker
    .getByRole("option", { name: "codex/gpt-5.6-sol", exact: true })
    .click();
  const thinking = page.getByRole("combobox", { name: "Thinking", exact: true });
  await expect(thinking).toBeEnabled();
  await thinking.selectOption("max");
  await page.getByRole("button", { name: "Create", exact: true }).click();

  await expect.poll(() => requests.length).toBe(1);
  expect(requests[0]).toMatchObject({
    model: "codex/gpt-5.6-sol",
    thinking_level: "max",
  });

  await page.reload();
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await expect(page.getByRole("combobox", { name: "Automation model" }))
    .toContainText("codex/gpt-5.6-sol");
  await expect(page.getByRole("combobox", { name: "Thinking", exact: true }))
    .toHaveValue("max");
  await page.getByRole("combobox", { name: "Thinking", exact: true })
    .selectOption("ultra");
  await page.getByRole("button", { name: "Save", exact: true }).click();

  await expect.poll(() => requests.length).toBe(2);
  expect(requests[1]).toMatchObject({
    model: "codex/gpt-5.6-sol",
    thinking_level: "ultra",
  });
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

    await stabilizeVisualFonts(page);

    const result = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
      .analyze();
    expect(result.violations.filter(({ impact }) =>
      impact === "serious" || impact === "critical"
    )).toEqual([]);

    await expect(page.locator("main").first()).toHaveScreenshot(screen.snapshot);
  }
});

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

interface FixtureEvent extends Record<string, unknown> {
  readonly cursor: number;
}

const threadEvent = (
  cursor: number,
  event: Record<string, unknown>,
): FixtureEvent => ({
  cursor,
  scope: { thread: "th_fixture" },
  ts: new Date(Date.UTC(2026, 7, 4, 8, 0, cursor)).toISOString(),
  ...event,
});

const history: readonly FixtureEvent[] = [
  threadEvent(1, {
    type: "turn.started",
    turn: 7,
    mode: "code",
    model: "test/model",
  }),
  threadEvent(2, {
    type: "user.message",
    turn: 7,
    content: "Build the **migration**",
    attachments: [],
  }),
  threadEvent(3, { type: "assistant.thinking", turn: 7, text: "Compare both frontends" }),
  threadEvent(4, { type: "assistant.delta", turn: 7, text: "I'll update it." }),
  threadEvent(5, { type: "assistant.message", turn: 7, content: "I'll update it." }),
  threadEvent(6, {
    type: "tool.requested",
    turn: 7,
    call_id: "call_edit",
    tool: "Edit",
    args: {
      file_path: "src/app.ts",
      old_string: "const ready = false;",
      new_string: "const ready = true;",
      _line: 8,
    },
    requires_approval: false,
  }),
  threadEvent(7, { type: "tool.started", call_id: "call_edit" }),
  threadEvent(8, { type: "tool.output", call_id: "call_edit", chunk: "updated src/app.ts\n" }),
  threadEvent(9, {
    type: "tool.completed",
    call_id: "call_edit",
    status: "ok",
    result: { exit_code: 0, bytes_written: 19 },
  }),
  threadEvent(10, {
    type: "tool.requested",
    turn: 7,
    call_id: "call_read",
    tool: "read_file",
    args: { path: "src/app.ts", offset: 8, limit: 1 },
    requires_approval: false,
  }),
  threadEvent(11, { type: "tool.started", call_id: "call_read" }),
  threadEvent(12, {
    type: "tool.completed",
    call_id: "call_read",
    status: "ok",
    result: { content: "const ready = true;" },
  }),
  threadEvent(13, { type: "assistant.delta", turn: 7, text: "Done." }),
  threadEvent(14, { type: "assistant.message", turn: 7, content: "Done." }),
  threadEvent(15, {
    type: "turn.completed",
    turn: 7,
    usage: { input_tokens: 20, output_tokens: 8, cost_usd: 0.002 },
    checkpoint_id: null,
  }),
];

const installEventStream = async (page: Page): Promise<void> => {
  await page.addInitScript((seedEvents) => {
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
        sources.add(this);
        globalThis.setTimeout(() => {
          if (this.readyState === FixtureEventSource.CLOSED) return;
          this.readyState = FixtureEventSource.OPEN;
          this.onopen?.(new Event("open"));
          if (this.url.includes("/v1/threads/th_fixture/events")) {
            for (const event of seedEvents) this.emit(event);
          }
        }, 10);
      }

      emit(event: { readonly cursor: number }): void {
        if (this.readyState !== FixtureEventSource.OPEN) return;
        this.onmessage?.(new MessageEvent("message", {
          data: JSON.stringify(event),
          lastEventId: String(event.cursor),
        }));
      }

      close(): void {
        this.readyState = FixtureEventSource.CLOSED;
        sources.delete(this);
      }
    }

    const sources = new Set<FixtureEventSource>();
    Object.defineProperty(globalThis, "EventSource", {
      configurable: true,
      value: FixtureEventSource,
    });
    Object.defineProperty(globalThis, "__emitThreadEvent", {
      configurable: true,
      value: (event: { readonly cursor: number }) => {
        for (const source of sources) {
          if (source.url.includes("/v1/threads/th_fixture/events")) source.emit(event);
        }
      },
    });
    Object.defineProperty(globalThis, "__emitThreadEvents", {
      configurable: true,
      value: (events: readonly { readonly cursor: number }[]) => {
        for (const event of events) {
          for (const source of sources) {
            if (source.url.includes("/v1/threads/th_fixture/events")) source.emit(event);
          }
        }
      },
    });
    Object.defineProperty(globalThis, "__threadStreamReady", {
      configurable: true,
      value: () => [...sources].some((source) =>
        source.readyState === FixtureEventSource.OPEN
        && source.url.includes("/v1/threads/th_fixture/events")
      ),
    });
  }, history);
};

const installProtocolFixtures = async (
  page: Page,
  sentMessages: Array<Record<string, unknown>>,
  messageDelayMs = 0,
): Promise<void> => {
  let messageCount = 0;
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const key = `${request.method()} ${url.pathname}`;
    if (key === "POST /v1/threads/th_fixture/messages") {
      sentMessages.push(request.postDataJSON() as Record<string, unknown>);
      messageCount += 1;
      if (messageDelayMs > 0) {
        await new Promise((resolve) => globalThis.setTimeout(resolve, messageDelayMs));
      }
      await route.fulfill({
        json: messageCount === 1
          ? { thread_id: "th_fixture", turn: 8 }
          : { thread_id: "th_fixture", turn: 0, queued: true },
      });
      return;
    }
    if (key === "POST /v1/threads/th_fixture/cancel") {
      await route.fulfill({ status: 204 });
      return;
    }
    if (key === "GET /v1/threads/th_fixture/view") {
      await route.fulfill({
        headers: { "x-trouve-event-cursor": "0" },
        json: {
          item_offset: 0,
          total_items: 0,
          has_older: false,
          items: [],
        },
      });
      return;
    }

    const responses: Record<string, unknown> = {
      "GET /v1/info": {
        name: "trouve-server",
        version: "3.7.0",
        protocol_version: "2.9",
        online: true,
      },
      "GET /v1/session-summaries": {
        cursor: 0,
        summaries: [{
          session_id: "se_1",
          workspace_id: "ws_1",
          archived: false,
          active: false,
          attention: "none",
          outcome: "idle",
          latest_cursor: 15,
          updated_at: "2026-08-04T08:00:15Z",
        }],
      },
      "GET /v1/sessions": [{
        id: "se_1",
        workspace_id: "ws_1",
        title: "Chat rendering",
        branch: "feature/chat",
        worktree_path: "/tmp/chat-rendering",
        base_ref: "main",
        created_at: "2026-08-04T08:00:00Z",
      }],
      "GET /v1/threads": [{
        id: "th_fixture",
        session_id: "se_1",
        mode: "code",
        model: "test/model",
        permission_mode: "ask",
        created_at: "2026-08-04T08:00:00Z",
      }],
      "GET /v1/workspaces": [{ id: "ws_1", name: "trouve", path: "/src/trouve" }],
      "GET /v1/providers": {
        default_model: "test/model",
        default_permission_mode: "ask",
        default_thinking_level: null,
        providers: [],
      },
      "GET /v1/models": [{
        id: "test/model",
        display_name: "Test Model",
        context_window: 128_000,
        supports_tools: true,
        options_schema: { type: "object", properties: {} },
      }],
      "GET /v1/modes": [{
        id: "code",
        display_name: "Code",
        system_prompt: "Implement the request.",
      }],
      "GET /v1/mode-infos": [{
        origin: "builtin",
        mode: {
          id: "code",
          display_name: "Code",
          system_prompt: "Implement the request.",
        },
      }],
      "GET /v1/automations": [],
      "GET /v1/automations/templates": [],
      "GET /v1/integrations/github": {
        configured: false,
        source: "",
        oauth_available: true,
        hosts: [],
      },
      "GET /v1/sessions/se_1/usage": {
        input_tokens: 20,
        cached_input_tokens: 0,
        output_tokens: 8,
        cost_usd: 0.002,
        turns: 1,
      },
      "GET /v1/sessions/se_1/paths": [],
      "GET /v1/sessions/se_1/diff": { diff: "" },
      "GET /v1/sessions/se_1/prs": [],
    };
    const response = responses[key];
    if (response === undefined) {
      await route.fulfill({
        status: 501,
        json: { code: "fixture_missing", message: `No chat fixture for ${key}` },
      });
      return;
    }
    await route.fulfill({ json: response });
  });
};

const emit = async (page: Page, event: FixtureEvent): Promise<void> => {
  await page.evaluate((nextEvent) => {
    const scope = globalThis as typeof globalThis & {
      __emitThreadEvent(event: FixtureEvent): void;
    };
    scope.__emitThreadEvent(nextEvent);
  }, event);
};

const emitBatch = async (page: Page, events: readonly FixtureEvent[]): Promise<void> => {
  await page.evaluate((nextEvents) => {
    const scope = globalThis as typeof globalThis & {
      __emitThreadEvents(events: readonly FixtureEvent[]): void;
    };
    scope.__emitThreadEvents(nextEvents);
  }, events);
};

const replayHistory = async (page: Page): Promise<void> => {
  await expect.poll(() => page.evaluate(() => {
    const scope = globalThis as typeof globalThis & { __threadStreamReady(): boolean };
    return scope.__threadStreamReady();
  })).toBe(true);
  for (const event of history) await emit(page, event);
};

interface HorizontalOverflowFinding {
  readonly selector: string;
  readonly tag: string;
  readonly className: string;
  readonly clientWidth: number;
  readonly scrollWidth: number;
  readonly childWidths: readonly string[];
}

const horizontalOverflowFindings = async (
  page: Page,
): Promise<readonly HorizontalOverflowFinding[]> =>
  page.evaluate(() => {
    const selectors = [
      ".app-shell",
      ".chat-stream",
      ".chat-stream [data-virtual-id]",
      ".turn-card",
      ".turn-card .message-header",
      ".turn-card .message-body",
      ".agent-body-stream",
      ".agent-text-block",
      ".activity-group",
      ".activity-group-body",
      ".thinking-card",
      ".thinking-body",
      ".question-card",
      ".question-step",
      ".question-options",
      ".question-option",
      ".question-summary",
      ".tool-card",
      ".tool-card > summary",
      ".tool-todo-list",
      ".attachment-list",
      ".attachment-list li",
      ".pending-attachments",
      ".pending-attachments li",
      ".queue-panel",
      ".queue-panel ol",
      ".queue-panel li",
      ".queue-row",
      ".composer-entry",
    ] as const;
    const findings: HorizontalOverflowFinding[] = [];
    for (const selector of selectors) {
      for (const element of document.querySelectorAll<HTMLElement>(selector)) {
        if (element.clientWidth === 0 || element.clientHeight === 0) continue;
        if (element.scrollWidth <= element.clientWidth + 1) continue;
        findings.push({
          selector,
          tag: element.localName,
          className: element.className,
          clientWidth: element.clientWidth,
          scrollWidth: element.scrollWidth,
          childWidths: [...element.children].map((child) => {
            const htmlChild = child as HTMLElement;
            return `${htmlChild.localName}.${htmlChild.className}:${htmlChild.clientWidth}/${htmlChild.scrollWidth}`;
          }),
        });
      }
    }
    for (const markdown of document.querySelectorAll<HTMLElement>("trouve-markdown-view")) {
      if (markdown.clientWidth === 0 || markdown.clientHeight === 0) continue;
      if (markdown.scrollWidth <= markdown.clientWidth + 1) continue;
      findings.push({
        selector: "trouve-markdown-view",
        tag: markdown.localName,
        className: markdown.className,
        clientWidth: markdown.clientWidth,
        scrollWidth: markdown.scrollWidth,
        childWidths: [],
      });
    }
    return findings;
  });

test.beforeEach(async ({ page }, testInfo) => {
  test.skip(
    !["desktop-chromium", "mobile-chromium"].includes(testInfo.project.name),
    "Chromium desktop and mobile own the stateful chat DOM fixture",
  );
  await installEventStream(page);
});

test("chat cards unmount collapsed output and retain formatted/raw views", async ({ page }) => {
  await installProtocolFixtures(page, []);
  await page.goto("/");
  await replayHistory(page);

  await expect(page.locator(".user-message trouve-markdown-view strong")).toHaveText("migration");
  const activityGroup = page.locator(".activity-group");
  await expect(activityGroup.getByText("Edited 1 file, read 1 file", { exact: true })).toBeVisible();
  await expect(activityGroup.locator(".activity-group-body")).toHaveCount(0);

  await activityGroup.locator(":scope > summary").click();
  await expect(activityGroup.locator(".tool-card")).toHaveCount(2);
  await expect(page.getByLabel("Live tool output")).toHaveCount(0);

  const editCard = page.locator('.tool-card[data-call-id="call_edit"]');
  await editCard.locator(":scope > summary").click();
  await expect(editCard.getByLabel("Live tool output")).toContainText("updated src/app.ts");
  await expect(editCard.locator(".tool-inline-diff")).toBeVisible();
  await expect(editCard.locator(".tool-meta")).toContainText("exit 0 · 2s");

  await editCard.getByRole("button", { name: "Show raw tool output" }).click();
  await expect(editCard.getByLabel("Raw tool data")).toContainText('"old_string"');
  await expect(editCard.locator(".tool-inline-diff")).toHaveCount(0);
  await editCard.getByRole("button", { name: "Show formatted tool output" }).click();
  await expect(editCard.locator(".tool-inline-diff")).toBeVisible();

  await editCard.locator(":scope > summary").click();
  await expect(editCard.getByLabel("Live tool output")).toHaveCount(0);
  await expect(editCard.locator(".tool-inline-diff")).toHaveCount(0);
  await activityGroup.locator(":scope > summary").click();
  await expect(activityGroup.locator(".tool-card")).toHaveCount(0);

  await page.getByRole("button", { name: "Collapse thought process" }).click();
  await expect(page.locator(".thinking-body")).toHaveCount(0);
  await expect(page.locator(".thinking-card .message-collapsed-preview"))
    .toContainText("Compare both frontends");

  const agentCard = page.locator(".agent-turn-card").first();
  await agentCard.getByRole("button", { name: "Collapse agent message" }).click();
  await expect(agentCard.locator(":scope > .message-body")).toHaveCount(0);
  await expect(agentCard.locator(".agent-collapsed-preview")).toContainText("I'll update it.");
  await agentCard.getByRole("button", { name: "Expand agent message" }).click();

  const result = await new AxeBuilder({ page })
    .include(".chat-stream")
    .include(".composer")
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
    .analyze();
  expect(result.violations.filter(({ impact }) =>
    impact === "serious" || impact === "critical"
  )).toEqual([]);
});

test("chat surfaces contain pathological content from narrow to wide layouts", async ({
  page,
}, testInfo) => {
  test.skip(
    testInfo.project.name !== "desktop-chromium",
    "One Chromium project owns the multi-viewport pathological-content matrix",
  );
  await installProtocolFixtures(page, []);
  await page.goto("/");
  await replayHistory(page);

  const longToken = "unbroken-layout-token-".repeat(32);
  const longPath = `/home/jim/${"deeply-nested-worktree/".repeat(24)}source.ts`;
  await page.locator("trouve-thread-screen .attachment-button input").setInputFiles({
    name: `${longToken}.txt`,
    mimeType: "text/plain",
    buffer: Buffer.from("layout attachment"),
  });
  await expect(page.locator(".pending-attachments li")).toBeVisible();
  const markdown = [
    `A long token: ${longToken}`,
    "",
    `[A long link](https://example.com/${longToken})`,
    "",
    `| Column | Value |\n| --- | --- |\n| layout | ${longToken} |`,
    "",
    `\`\`\`text\n${longPath}\n\`\`\``,
  ].join("\n");
  await emitBatch(page, [
    threadEvent(16, {
      type: "turn.started",
      turn: 30,
      mode: "code",
      model: `provider/${longToken}`,
    }),
    threadEvent(17, {
      type: "user.message",
      turn: 30,
      content: markdown,
      attachments: [{
        id: "att_layout",
        name: `${longToken}.log`,
        mime: `application/x-${longToken}`,
        size_bytes: 4_096,
      }],
    }),
    threadEvent(18, {
      type: "assistant.message",
      turn: 30,
      content: markdown,
    }),
    threadEvent(19, {
      type: "assistant.thinking",
      turn: 30,
      text: `Thinking through ${longToken}`,
    }),
    threadEvent(20, {
      type: "tool.requested",
      turn: 30,
      call_id: "call_layout_command",
      tool: "CommandExecution",
      args: { command: `printf %s ${longToken}`, cwd: longPath },
      requires_approval: false,
    }),
    threadEvent(21, {
      type: "tool.output",
      call_id: "call_layout_command",
      chunk: `${longToken}\n${longPath}`,
    }),
    threadEvent(22, {
      type: "tool.completed",
      call_id: "call_layout_command",
      status: "ok",
      result: { exit_code: 0, output_path: longPath },
    }),
    threadEvent(23, {
      type: "tool.requested",
      turn: 30,
      call_id: "call_layout_todos",
      tool: "todo_write",
      args: { todos: [{ content: longToken, status: "in_progress" }] },
      requires_approval: false,
    }),
    threadEvent(24, {
      type: "tool.completed",
      call_id: "call_layout_todos",
      status: "ok",
      result: { todos: [{ content: longToken, status: "in_progress" }] },
    }),
    threadEvent(25, {
      type: "tool.requested",
      turn: 30,
      call_id: "call_layout_edit",
      tool: "Edit",
      args: {
        file_path: longPath,
        old_string: `const before = "${longToken}";`,
        new_string: `const after = "${longToken}";`,
        _line: 1,
      },
      requires_approval: false,
    }),
    threadEvent(26, {
      type: "tool.completed",
      call_id: "call_layout_edit",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(27, {
      type: "tool.requested",
      turn: 30,
      call_id: "call_layout_approval",
      tool: `mcp__${longToken}__dangerous_action`,
      args: { path: longPath },
      requires_approval: true,
    }),
    threadEvent(28, {
      type: "approval.requested",
      turn: 30,
      call_id: "call_layout_approval",
    }),
    threadEvent(29, {
      type: "question.requested",
      turn: 30,
      request_id: "question_layout",
      title: longToken,
      questions: [{
        id: "layout_question",
        prompt: `Choose how to handle ${longToken}`,
        options: [{ id: "layout_option", label: longToken }],
      }],
    }),
    threadEvent(30, {
      type: "thread.queue_updated",
      prompts: [{
        id: "queue_layout",
        thread_id: "th_fixture",
        content: longToken,
        position: 0,
        created_at: "2026-08-04T08:00:30Z",
        attachments: [{
          id: "queue_layout_attachment",
          name: `${longToken}.txt`,
          mime: "text/plain",
          size_bytes: 128,
        }],
      }],
    }),
    threadEvent(31, {
      type: "turn.failed",
      turn: 30,
      error: `Layout failure ${longToken}`,
    }),
  ]);

  await expect(page.locator('[data-question-request-id="question_layout"]')).toBeVisible();
  for (const callId of [
    "call_layout_command",
    "call_layout_todos",
    "call_layout_edit",
  ]) {
    const card = page.locator(`.tool-card[data-call-id="${callId}"]`);
    await expect(card).toBeVisible();
    if (!(await card.evaluate((element) => (element as HTMLDetailsElement).open))) {
      await card.locator(":scope > summary").click();
    }
  }

  const viewports = [
    { width: 2_560, height: 720 },
    { width: 1_920, height: 1_080 },
    { width: 1_440, height: 900 },
    { width: 1_180, height: 720 },
    { width: 1_151, height: 640 },
    { width: 1_150, height: 600 },
    { width: 900, height: 540 },
    { width: 761, height: 480 },
    { width: 760, height: 640 },
    { width: 412, height: 732 },
    { width: 360, height: 640 },
    { width: 320, height: 568 },
    { width: 280, height: 480 },
  ] as const;
  for (const viewport of viewports) {
    await page.setViewportSize(viewport);
    await page.evaluate(() => new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
    }));
    const label = `${viewport.width}x${viewport.height}`;
    expect(await horizontalOverflowFindings(page), `horizontal overflow at ${label}`).toEqual([]);
    expect(
      await page.locator(".tool-card > summary > strong").evaluateAll((labels) =>
        labels
          .filter((label) => (label as HTMLElement).clientWidth === 0)
          .map((label) => label.textContent ?? "")
      ),
      `tool titles hidden at ${label}`,
    ).toEqual([]);
    expect(await page.locator("trouve-thread-screen.thread-panel").evaluate((panel) => {
      const thread = panel.getBoundingClientRect();
      const chat = panel.querySelector<HTMLElement>(".chat-stream")?.getBoundingClientRect();
      const queue = panel.querySelector<HTMLElement>(".queue-panel")?.getBoundingClientRect();
      const composer = panel.querySelector<HTMLElement>(".composer")?.getBoundingClientRect();
      if (chat === undefined || composer === undefined) return ["missing chat or composer"];
      const findings: string[] = [];
      if (chat.height < 1) findings.push("chat viewport has no height");
      if (chat.top < thread.top - 1 || chat.bottom > thread.bottom + 1) {
        findings.push("chat viewport escapes thread panel");
      }
      if (queue !== undefined && chat.bottom > queue.top + 1) {
        findings.push("chat viewport overlaps prompt queue");
      }
      if (queue !== undefined && queue.bottom > composer.top + 1) {
        findings.push("prompt queue overlaps composer");
      }
      if (composer.bottom > thread.bottom + 1) findings.push("composer escapes thread panel");
      return findings;
    }), `vertical pane layout at ${label}`).toEqual([]);
  }

  const question = page.locator('[data-question-request-id="question_layout"]');
  await question.locator(".question-other-option").click();
  await question.locator(".question-other-input").fill(longToken);
  await question.locator(".question-navigation .primary").click();
  await expect(question.locator(".question-summary")).toBeVisible();
  for (const viewport of [
    { width: 1_920, height: 1_080 },
    { width: 761, height: 480 },
    { width: 360, height: 640 },
    { width: 280, height: 480 },
  ] as const) {
    await page.setViewportSize(viewport);
    await page.evaluate(() => new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
    }));
    expect(
      await horizontalOverflowFindings(page),
      `question review overflow at ${viewport.width}x${viewport.height}`,
    ).toEqual([]);
  }

  await emit(page, threadEvent(32, {
    type: "question.resolved",
    request_id: "question_layout",
    answers: [{
      question_id: "layout_question",
      selected_option_ids: [],
      other_text: longToken,
    }],
  }));
  await expect(page.locator(".question-resolved .question-summary")).toBeVisible();
  await emit(page, threadEvent(33, {
    type: "approval.resolved",
    call_id: "call_layout_approval",
    decision: "approve",
  }));
  const longAgentMessage = page.locator(".agent-turn-card").last();
  await longAgentMessage.getByRole("button", { name: "Collapse agent message" }).click();
  await expect(longAgentMessage.getByRole("button", { name: "Expand agent message" })).toBeVisible();
  await expect(longAgentMessage.locator(".agent-collapsed-preview")).toBeVisible();
  await page.getByRole("button", { name: "Use full history" }).focus();
  await page.keyboard.press("Enter");
  const longUserMessage = page.locator(".user-message").last();
  await longUserMessage.getByRole("button", { name: "Collapse your message" }).click();
  await expect(longUserMessage.locator(".message-collapsed-preview")).toBeVisible();
  expect(await horizontalOverflowFindings(page), "resolved and collapsed chat overflow").toEqual([]);
});

test("long chat history keeps a bounded DOM with an accessible full-history fallback", async ({ page }) => {
  await installProtocolFixtures(page, []);
  await page.addInitScript(() => {
    localStorage.setItem("trouve.resume.v1", JSON.stringify({
      selectedSessionId: "se_1",
      sessionThreads: { se_1: "th_fixture" },
      threadScroll: { th_fixture: { itemId: "removed-chat-row", offset: 12 } },
    }));
  });
  await page.goto("/");
  await replayHistory(page);
  await expect.poll(() => page.evaluate(() => {
    const raw = localStorage.getItem("trouve.resume.v1");
    if (raw === null) return false;
    const resume = JSON.parse(raw) as { threadScroll?: Record<string, unknown> };
    return resume.threadScroll?.["th_fixture"] === undefined;
  })).toBe(true);

  let cursor = 16;
  const events: FixtureEvent[] = [];
  for (let turn = 20; turn < 220; turn += 1) {
    events.push(
      threadEvent(cursor++, { type: "turn.started", turn, mode: "code", model: "test/model" }),
      threadEvent(cursor++, {
        type: "user.message",
        turn,
        content: `Virtual prompt ${turn}`,
        attachments: [],
      }),
      threadEvent(cursor++, {
        type: "assistant.message",
        turn,
        content: `Virtual response ${turn}`,
      }),
      threadEvent(cursor++, {
        type: "turn.completed",
        turn,
        usage: { input_tokens: 2, output_tokens: 2 },
        checkpoint_id: null,
      }),
    );
  }
  await emitBatch(page, events);

  await expect(page.getByText("Virtual response 219", { exact: true })).toBeVisible();
  await expect.poll(() => page.locator("[data-virtual-id]").count()).toBeLessThan(50);

  await page.locator(".chat-stream").evaluate((viewport) => {
    viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -1_000 }));
    viewport.scrollTop = 0;
    viewport.dispatchEvent(new Event("scroll"));
  });
  await expect(page.getByRole("log", { name: "Conversation" })).toHaveAttribute(
    "aria-live",
    "off",
  );
  await expect(page.getByRole("button", { name: "Jump to latest" })).toBeVisible();
  await page.getByRole("button", { name: "Jump to latest" }).click();
  await expect(page.getByRole("log", { name: "Conversation" })).toHaveAttribute(
    "aria-live",
    "polite",
  );
  await expect(page.getByText("Virtual response 219", { exact: true })).toBeVisible();
  await expect.poll(() => page.locator(".chat-stream").evaluate((viewport) =>
    viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop
  )).toBeLessThanOrEqual(1);

  await page.locator(".chat-stream").evaluate((viewport) => {
    viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -8 }));
    viewport.scrollTop = Math.max(0, viewport.scrollTop - 8);
    viewport.dispatchEvent(new Event("scroll"));
  });
  await page.evaluate(() => new Promise<void>((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
  }));
  await expect.poll(() => page.locator(".chat-stream").evaluate((viewport) =>
    viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop
  )).toBeGreaterThanOrEqual(7);
  await expect(page.getByRole("log", { name: "Conversation" })).toHaveAttribute(
    "aria-live",
    "off",
  );
  await expect(page.getByRole("button", { name: "Jump to latest" })).toBeVisible();

  const scrollRenderRequests = await page.locator("trouve-thread-screen").evaluate(
    async (element) => {
      const screen = element as HTMLElement & { requestUpdate: () => void };
      const viewport = screen.querySelector<HTMLElement>(".chat-stream");
      if (viewport === null) throw new Error("missing chat viewport");
      const requestUpdate = screen.requestUpdate.bind(screen);
      let requests = 0;
      screen.requestUpdate = () => {
        requests += 1;
        requestUpdate();
      };
      try {
        for (let index = 0; index < 12; index += 1) {
          viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -1 }));
          viewport.scrollTop = Math.max(0, viewport.scrollTop - 1);
          viewport.dispatchEvent(new Event("scroll"));
        }
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
        return requests;
      } finally {
        screen.requestUpdate = requestUpdate;
      }
    },
  );
  expect(scrollRenderRequests, "small scroll events should not force full transcript renders").toBeLessThanOrEqual(2);

  await page.locator(".chat-stream").evaluate((viewport) => {
    const jump = viewport.querySelector<HTMLElement>(".follow-tail");
    if (jump === null) throw new Error("missing jump-to-latest control");
    viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: 1_000 }));
    // WebKit includes the sticky control in scrollHeight even though it is
    // visually overlaid. Stop at the transcript tail, not after the control.
    viewport.scrollTop = Math.max(
      0,
      viewport.scrollHeight
        - viewport.clientHeight
        - jump.getBoundingClientRect().height,
    );
    viewport.dispatchEvent(new Event("scroll"));
  });
  await expect(page.getByRole("log", { name: "Conversation" })).toHaveAttribute(
    "aria-live",
    "polite",
  );
  await expect(page.getByRole("button", { name: "Jump to latest" })).toHaveCount(0);

  const historyMode = page.getByRole("button", { name: "Use full history" });
  await historyMode.focus();
  await historyMode.press("Enter");
  await expect.poll(() => page.locator("[data-virtual-id]").count()).toBeGreaterThan(400);
  await page.getByRole("button", { name: "Use windowed history" }).press("Enter");
  await expect.poll(() => page.locator("[data-virtual-id]").count()).toBeLessThan(50);
});

test("turn controls cover start, queue, cancel, and send-after-cancel races", async ({ page }) => {
  const sentMessages: Array<Record<string, unknown>> = [];
  await installProtocolFixtures(page, sentMessages, 250);
  await page.goto("/");
  await replayHistory(page);
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  const submit = page.locator("wa-button.composer-submit");

  await composer.fill("Start another turn");
  await expect(submit).toHaveText("Send");
  await submit.click();
  await expect(submit).toHaveText("Sending…");
  await expect(submit).toHaveText("Starting…");
  await expect(page.locator('[data-virtual-id="ephemeral:activity"] .agent-activity'))
    .toContainText("Starting turn…");

  await composer.fill("Queue this while startup is pending");
  await expect(submit).toHaveText("Queue");
  await submit.click();
  await expect(submit).toHaveText("Queueing…");
  await expect(submit).toHaveText("Starting…");

  await emit(page, threadEvent(16, {
    type: "turn.started",
    turn: 8,
    mode: "code",
    model: "test/model",
  }));
  await emit(page, threadEvent(17, {
    type: "user.message",
    turn: 8,
    content: "Start another turn",
    attachments: [],
  }));
  await emit(page, threadEvent(18, {
    type: "assistant.delta",
    turn: 8,
    text: "Working",
  }));
  await emit(page, threadEvent(19, {
    type: "thread.queue_updated",
    prompts: [{
      id: "qp_1",
      thread_id: "th_fixture",
      content: "Queue this while startup is pending",
      position: 0,
      created_at: "2026-08-04T08:00:19Z",
      attachments: [{
        id: "att_queue_1",
        name: "layout.png",
        mime: "image/png",
        size_bytes: 1024,
      }],
    }],
  }));

  await expect(page.locator(".queue-panel [role=status]")).toHaveText("1 queued prompt");
  await expect(page.locator(".queue-attachment-badge")).toHaveText("📎1");
  await expect(page.locator(".queue-attachment-badge")).toHaveAttribute(
    "aria-label",
    "1 attachment",
  );
  const activeAgent = page.locator(".agent-turn-card").last();
  await expect(activeAgent.locator(".agent-activity")).toContainText("Processing…");
  await expect.poll(() => page.locator(".chat-stream").evaluate((viewport) =>
    viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop
  )).toBeLessThanOrEqual(1);
  expect(await page.locator("trouve-thread-screen.thread-panel").evaluate((panel) => {
    const chat = panel.querySelector<HTMLElement>(".chat-stream")?.getBoundingClientRect();
    const queue = panel.querySelector<HTMLElement>(".queue-panel")?.getBoundingClientRect();
    const activity = panel.querySelector<HTMLElement>(".agent-activity")?.getBoundingClientRect();
    if (chat === undefined || queue === undefined || activity === undefined) {
      return ["missing chat, queue, or activity"];
    }
    const findings: string[] = [];
    if (chat.bottom > queue.top + 1) findings.push("queue overlaps chat viewport");
    if (activity.bottom > chat.bottom + 1) findings.push("activity is clipped below chat viewport");
    return findings;
  })).toEqual([]);
  await page.locator("trouve-app").evaluate((element) => {
    element.setAttribute("data-reduce-motion", "");
  });
  await expect.poll(() => activeAgent.locator("trouve-markdown-view[streaming]").evaluate(
    (element) => getComputedStyle(element, "::after").animationName,
  )).toBe("none");
  await expect(page.locator('[data-virtual-id="ephemeral:activity"]')).toHaveCount(0);
  await expect(submit).toHaveText("Cancel");
  await submit.click();
  await expect(submit).toHaveText("Stopping…");

  await composer.fill("Continue after the cancellation");
  await expect(submit).toHaveText("Send next");
  await submit.click();
  await expect(submit).toHaveText("Queueing…");
  await expect(submit).toHaveText("Stopping…");
  await emit(page, threadEvent(20, { type: "turn.cancelled", turn: 8 }));
  await emit(page, threadEvent(21, {
    type: "turn.started",
    turn: 9,
    mode: "code",
    model: "test/model",
  }));
  await emit(page, threadEvent(22, {
    type: "user.message",
    turn: 9,
    content: "Continue after the cancellation",
    attachments: [],
  }));
  await emit(page, threadEvent(23, { type: "assistant.thinking", turn: 9, text: "Resuming" }));

  await expect(page.getByText("Turn cancelled", { exact: true })).toBeVisible();
  await expect(submit).toHaveText("Cancel");
  expect(sentMessages.map((message) => message.content)).toEqual([
    "Start another turn",
    "Queue this while startup is pending",
    "Continue after the cancellation",
  ]);
});

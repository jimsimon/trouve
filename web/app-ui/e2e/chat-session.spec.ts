import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

interface FixtureEvent extends Record<string, unknown> {
  readonly cursor: number;
}

interface ThreadViewFixture {
  readonly cursor?: number;
  readonly snapshot: Record<string, unknown>;
}

type ThreadViewFixtureLoader = (
  before: number | undefined,
) => ThreadViewFixture | Promise<ThreadViewFixture>;

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
    checkpoint_id: "cp_turn_7",
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
      readonly listeners = new Map<string, Set<EventListenerOrEventListenerObject>>();

      constructor(url: string | URL) {
        this.url = String(url);
        sources.add(this);
        globalThis.setTimeout(() => {
          if (this.readyState === FixtureEventSource.CLOSED) return;
          this.readyState = FixtureEventSource.OPEN;
          const event = new Event("open");
          this.onopen?.(event);
          this.dispatch("open", event);
          if (this.url.includes("/v1/threads/th_fixture/events")) {
            for (const event of seedEvents) this.emit(event);
          }
        }, 10);
      }

      emit(event: { readonly cursor: number }): void {
        if (this.readyState !== FixtureEventSource.OPEN) return;
        const message = new MessageEvent<string>("message", {
          data: JSON.stringify(event),
          lastEventId: String(event.cursor),
        });
        this.onmessage?.(message);
        this.dispatch("message", message);
      }

      addEventListener(
        type: string,
        listener: EventListenerOrEventListenerObject | null,
      ): void {
        if (listener === null) return;
        const listeners = this.listeners.get(type) ?? new Set();
        listeners.add(listener);
        this.listeners.set(type, listeners);
      }

      removeEventListener(
        type: string,
        listener: EventListenerOrEventListenerObject | null,
      ): void {
        if (listener === null) return;
        this.listeners.get(type)?.delete(listener);
      }

      dispatch(type: string, event: Event): void {
        for (const listener of this.listeners.get(type) ?? []) {
          if (typeof listener === "function") listener.call(this, event);
          else listener.handleEvent(event);
        }
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

interface ProtocolFixtureOptions {
  readonly sentMessages?: Array<Record<string, unknown>>;
  readonly dispatchedQueuePromptIds?: string[];
  readonly messageDelayMs?: number;
  readonly beforeMessageResponse?: (messageCount: number) => Promise<void>;
  readonly threadViewFixture?: ThreadViewFixtureLoader;
  readonly permissionMode?: "ask" | "allow_list" | "yolo";
  readonly additionalThreads?: readonly Record<string, unknown>[];
  readonly additionalSessions?: readonly Record<string, unknown>[];
  readonly additionalSessionSummaries?: readonly Record<string, unknown>[];
  readonly restoredCheckpointIds?: string[];
}

const installProtocolFixtures = async (
  page: Page,
  {
    sentMessages = [],
    dispatchedQueuePromptIds = [],
    messageDelayMs = 0,
    beforeMessageResponse,
    threadViewFixture,
    permissionMode = "ask",
    additionalThreads = [],
    additionalSessions = [],
    additionalSessionSummaries = [],
    restoredCheckpointIds = [],
  }: ProtocolFixtureOptions = {},
): Promise<void> => {
  let messageCount = 0;
  let editedQueue: readonly Record<string, unknown>[] = [];
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const key = `${request.method()} ${url.pathname}`;
    if (key === "POST /v1/threads/th_fixture/messages") {
      sentMessages.push(request.postDataJSON() as Record<string, unknown>);
      messageCount += 1;
      await beforeMessageResponse?.(messageCount);
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
    const queuedPromptDispatchMatch = /^\/v1\/queue\/([^/]+)\/dispatch$/u.exec(url.pathname);
    if (request.method() === "POST" && queuedPromptDispatchMatch !== null) {
      dispatchedQueuePromptIds.push(queuedPromptDispatchMatch[1]!);
      await route.fulfill({
        status: 202,
        json: { thread_id: "th_fixture", turn: 0, queued: true },
      });
      return;
    }
    const checkpointRestoreMatch = /^\/v1\/checkpoints\/([^/]+)\/restore$/u.exec(url.pathname);
    if (request.method() === "POST" && checkpointRestoreMatch !== null) {
      restoredCheckpointIds.push(checkpointRestoreMatch[1]!);
      await route.fulfill({ status: 204 });
      return;
    }
    if (key === "PATCH /v1/queue/qp_1") {
      const update = request.postDataJSON() as {
        readonly content: string;
        readonly attachments?: readonly {
          readonly name: string;
          readonly mime: string;
          readonly data: string;
        }[];
      };
      editedQueue = [{
        id: "qp_1",
        thread_id: "th_fixture",
        content: update.content,
        position: 0,
        created_at: "2026-08-04T08:00:19Z",
        attachments: (update.attachments ?? []).map((attachment, index) => ({
          id: `att_edited_${index + 1}`,
          name: attachment.name,
          mime: attachment.mime,
          size_bytes: Math.max(1, Math.floor(attachment.data.length * 3 / 4)),
        })),
      }];
      await route.fulfill({ status: 204 });
      return;
    }
    if (key === "GET /v1/threads/th_fixture/queue") {
      await route.fulfill({ json: editedQueue });
      return;
    }
    if (
      key === "GET /v1/attachments/att_queue_1"
      || key === "GET /v1/attachments/att_preview_1"
    ) {
      await route.fulfill({
        contentType: "image/png",
        body: Buffer.from(
          "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
          "base64",
        ),
      });
      return;
    }
    const threadViewMatch = /^\/v1\/threads\/([^/]+)\/view$/u.exec(url.pathname);
    if (request.method() === "GET" && threadViewMatch !== null) {
      const beforeValue = url.searchParams.get("before");
      const fixture = threadViewMatch[1] !== "th_fixture" || threadViewFixture === undefined
        ? {
            cursor: 0,
            snapshot: {
              item_offset: 0,
              total_items: 0,
              has_older: false,
              items: [],
            },
          }
        : await threadViewFixture(
            beforeValue === null ? undefined : Number(beforeValue),
          );
      await route.fulfill({
        headers: { "x-trouve-event-cursor": String(fixture.cursor ?? 0) },
        json: fixture.snapshot,
      });
      return;
    }

    if (key === "GET /v1/threads") {
      const sessionId = url.searchParams.get("session_id");
      const threads = [{
        id: "th_fixture",
        session_id: "se_1",
        mode: "code",
        model: "test/model",
        model_options: { reasoning_effort: "max", context: "1m" },
        permission_mode: permissionMode,
        created_at: "2026-08-04T08:00:00Z",
      }, ...additionalThreads];
      await route.fulfill({
        json: sessionId === null
          ? threads
          : threads.filter((thread) => thread.session_id === sessionId),
      });
      return;
    }
    if (key === "GET /v1/server-projection") {
      await route.fulfill({
        headers: { "x-trouve-event-cursor": "0" },
        json: {
          github_pull_requests: [],
          session_pull_requests: [],
          git_worktree_settings: {
            derive_branch_name_from_session_title: false,
            title_model: {
              model_downloaded: false,
              runtime_installed: false,
              state: "not_installed",
            },
            title_model_load_behavior: "auto",
            title_model_resource_policy: "adaptive",
          },
        },
      });
      return;
    }

    const responses: Record<string, unknown> = {
      "GET /v1/info": {
        name: "trouve-server",
        version: "3.7.0",
        protocol_version: "3.14",
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
        }, ...additionalSessionSummaries],
      },
      "GET /v1/sessions": [{
        id: "se_1",
        workspace_id: "ws_1",
        title: "Chat rendering",
        branch: "feature/chat",
        worktree_path: "/tmp/chat-rendering",
        base_ref: "main",
        created_at: "2026-08-04T08:00:00Z",
      }, ...additionalSessions],
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
        options_schema: {
          type: "object",
          properties: {
            reasoning_effort: {
              type: "string",
              enum: ["none", "low", "medium", "high", "xhigh", "max"],
              default: "medium",
            },
            context: {
              type: "string",
              enum: ["300k", "1m"],
              default: "300k",
            },
          },
        },
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
      "GET /v1/sessions/se_2/usage": {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0,
        turns: 0,
      },
      "GET /v1/sessions/se_2/paths": [],
      "GET /v1/sessions/se_2/diff": { diff: "" },
      "GET /v1/sessions/se_2/prs": [],
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
      ".agent-activity-timeline",
      ".activity-group",
      ".activity-group-body",
      ".thinking-card",
      ".thinking-output",
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

const transcriptComposerGap = async (page: Page): Promise<number> =>
  page.locator("trouve-thread-screen.thread-panel").evaluate((panel) => {
    const composer = panel.querySelector<HTMLElement>(".composer")?.getBoundingClientRect();
    const rows = panel.querySelectorAll<HTMLElement>(
      ".chat-stream [data-virtual-id]:not(.chat-edge-spacer)",
    );
    const tail = rows.item(rows.length - 1)?.getBoundingClientRect();
    if (composer === undefined || tail === undefined) return Number.NaN;
    return Math.round(composer.top - tail.bottom);
  });

const VIRTUAL_DISCLOSURE_GEOMETRY_TEST =
  "resizing an anchored disclosure never overlaps adjacent virtual rows";

test.beforeEach(async ({ page }, testInfo) => {
  const ownsWebKitGeometryRegression =
    testInfo.project.name === "desktop-webkit"
    && testInfo.title === VIRTUAL_DISCLOSURE_GEOMETRY_TEST;
  test.skip(
    !["desktop-chromium", "mobile-chromium"].includes(testInfo.project.name)
      && !ownsWebKitGeometryRegression,
    "Chromium desktop and mobile own the stateful chat DOM fixture",
  );
  await installEventStream(page);
});

test("the TODO pane follows the thread's durable todo snapshot", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop-chromium", "Desktop owns the inspection layout");
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);

  const todoSwitch = page.locator(".inspection-todo-switch button");
  await expect(todoSwitch).toHaveCount(0);

  await emit(page, threadEvent(16, {
    type: "thread.todos_updated",
    todos: [
      { id: "inspect", content: "Inspect the adapter", status: "completed" },
      { id: "publish", content: "Publish the todo snapshot", status: "in_progress" },
    ],
  }));

  await expect(todoSwitch).toHaveText(/Todos\s+1\/2 complete/);
  await todoSwitch.click();
  const plan = page.locator("trouve-todo-plan-panel");
  await expect(plan.getByText("Thread todos", { exact: true })).toBeVisible();
  await expect(plan.locator("[data-todo-id=inspect]")).toContainText("Inspect the adapter");
  await expect(plan.locator("[data-todo-id=publish]")).toHaveAttribute(
    "aria-current",
    "step",
  );

  await emit(page, threadEvent(17, { type: "thread.todos_updated", todos: [] }));
  await expect(todoSwitch).toHaveCount(0);
  await expect(plan).toHaveCount(0);
  await expect(page.getByRole("tab", { name: "Diff" })).toHaveAttribute("aria-selected", "true");
});

test("turn separators retain Slint's even vertical spacing", async ({ page }) => {
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(16, {
      type: "turn.started",
      turn: 8,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(17, {
      type: "user.message",
      turn: 8,
      content: "Start the next turn",
      attachments: [],
    }),
  ]);

  const rule = page.locator(".turn-rule").last();
  await expect(rule).toBeVisible();
  const geometry = await rule.evaluate((element) => {
    const previous = [...document.querySelectorAll<HTMLElement>(".agent-turn-card")].at(-1);
    const user = element.parentElement?.querySelector<HTMLElement>(".user-message");
    if (previous === undefined || user === null || user === undefined) {
      throw new Error("missing adjacent turn geometry");
    }
    const previousBounds = previous.getBoundingClientRect();
    const ruleBounds = element.getBoundingClientRect();
    const userBounds = user.getBoundingClientRect();
    return {
      above: ruleBounds.top - previousBounds.bottom,
      below: userBounds.top - ruleBounds.bottom,
    };
  });
  expect(geometry.above).toBe(8);
  expect(geometry.below).toBe(8);
});

test("turn separators expose exact restore and session-fork actions", async ({ page }) => {
  const restoredCheckpointIds: string[] = [];
  await installProtocolFixtures(page, { restoredCheckpointIds });
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(16, {
      type: "turn.started",
      turn: 8,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(17, {
      type: "user.message",
      turn: 8,
      content: "Start the next turn",
      attachments: [],
    }),
  ]);

  const rule = page.locator(".turn-rule").last();
  const restore = rule.getByRole("button", {
    name: "Restore after turn 7 once the current turn finishes",
  });
  await expect(restore).toBeDisabled();
  await expect(rule.getByRole("button", {
    name: "Fork a new session from the checkpoint after turn 7",
  })).toBeVisible();

  await emit(page, threadEvent(18, {
    type: "turn.completed",
    turn: 8,
    usage: { input_tokens: 2, output_tokens: 1 },
    checkpoint_id: "cp_turn_8",
  }));
  const enabledRestore = rule.getByRole("button", {
    name: "Restore files to the checkpoint after turn 7",
  });
  await expect(enabledRestore).toBeEnabled();
  await enabledRestore.click();
  await expect.poll(() => restoredCheckpointIds).toEqual(["cp_turn_7"]);
});

test("the Agent header shows live token usage and elapsed time", async ({ page }) => {
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);
  const startedAt = new Date(Date.now() - 65_000).toISOString();
  await emitBatch(page, [
    {
      ...threadEvent(16, {
        type: "turn.started",
        turn: 8,
        mode: "code",
        model: "test/model",
        thinking_level: "max",
      }),
      ts: startedAt,
    },
    threadEvent(17, {
      type: "user.message",
      turn: 8,
      content: "Show progress while this turn runs",
      attachments: [],
    }),
    threadEvent(18, {
      type: "assistant.thinking",
      turn: 8,
      text: "Still working",
    }),
    threadEvent(19, {
      type: "turn.usage_updated",
      turn: 8,
      usage: { input_tokens: 900, output_tokens: 42, cost_usd: 0.01 },
    }),
  ]);

  const metadata = page.locator(".agent-turn-card").last().locator(".turn-metadata");
  await expect(page.locator(".agent-turn-card").last().locator(".agent-model-label"))
    .toHaveText("(test/model · Max)");
  await expect(metadata).toContainText("900 in / 42 out tokens");
  await expect(metadata).toContainText(/1m \d{2}s/u);
  const first = await metadata.textContent();
  await expect.poll(() => metadata.textContent(), { timeout: 3_000 }).not.toBe(first);
  await emit(page, {
    ...threadEvent(20, {
      type: "turn.completed",
      turn: 8,
      usage: { input_tokens: 1_000, output_tokens: 50, cost_usd: 0.012 },
      checkpoint_id: null,
    }),
    ts: new Date().toISOString(),
  });
  await expect(metadata).toContainText("1000 in / 50 out tokens");
  const completed = await metadata.textContent();
  await page.waitForTimeout(1_100);
  expect(await metadata.textContent()).toBe(completed);
});

test("model picker escapes the composer control strip", async ({ page }) => {
  await page.setViewportSize({ width: 780, height: 620 });
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);

  const picker = page.locator(".composer trouve-model-picker");
  const trigger = picker.getByRole("combobox", { name: "Model" });
  await expect(trigger).toContainText("test/model");
  await expect(trigger).not.toContainText("Test Model");
  await trigger.click();
  const popup = picker.locator(".model-picker-popup");
  await expect(popup).toBeVisible();
  await expect(popup.getByRole("option", { name: /test\/model/u })).toBeVisible();
  const geometry = await popup.evaluate((element) => {
    const controls = element.closest<HTMLElement>(".composer-controls");
    const pickerTrigger = element.parentElement?.querySelector<HTMLElement>(
      ".model-picker-trigger",
    );
    if (controls === null || pickerTrigger === null || pickerTrigger === undefined) {
      throw new Error("missing model picker geometry");
    }
    const popupBounds = element.getBoundingClientRect();
    const controlsBounds = controls.getBoundingClientRect();
    const triggerBounds = pickerTrigger.getBoundingClientRect();
    const paintedElement = document.elementFromPoint(
      popupBounds.left + popupBounds.width / 2,
      popupBounds.top + Math.min(20, popupBounds.height / 2),
    );
    return {
      controlsOverflowX: getComputedStyle(controls).overflowX,
      escapedControls: popupBounds.top < controlsBounds.top,
      opensAboveTrigger: popupBounds.bottom <= triggerBounds.top - 4,
      paintedAboveComposer: paintedElement !== null && element.contains(paintedElement),
      position: getComputedStyle(element).position,
      topLayer: typeof element.showPopover !== "function"
        || element.matches(":popover-open"),
    };
  });
  expect(geometry.controlsOverflowX).toBe("auto");
  expect(geometry.escapedControls).toBe(true);
  expect(geometry.opensAboveTrigger).toBe(true);
  expect(geometry.paintedAboveComposer).toBe(true);
  expect(geometry.position).toBe("fixed");
  expect(geometry.topLayer).toBe(true);

  await popup.getByRole("searchbox", { name: "Search models" }).press("Escape");
  await expect(popup).toHaveCount(0);
});

test("subscription status uses hover help without a click disclosure", async ({ page }) => {
  await installProtocolFixtures(page);
  await page.route("**/v1/subscriptions", async (route) => {
    await route.fulfill({
      json: [{
        provider_id: "test",
        status: "ok",
        plan: "pro",
        windows: [{ label: "Weekly", used_percent: 57, resets: "resets Monday" }],
        credits: "",
        note: "",
      }],
    });
  });
  await page.goto("/");
  await replayHistory(page);

  const status = page.locator(".composer .model-health-pill");
  await expect(page.locator(".composer .subscription-option > span")).toHaveText(
    "Subscription",
  );
  await expect(status).toContainText("Pro · 57% used");
  await expect(status).toHaveAttribute("title", /Weekly: 57% used · resets Monday/u);
  await expect(status).toHaveAttribute("tabindex", "0");
  await expect(status.locator("summary")).toHaveCount(0);
  await expect(page.locator(".model-health-detail")).toHaveCount(0);

  await status.click();
  await expect(status).toBeFocused();
  await expect(page.locator(".model-health-detail")).toHaveCount(0);
});

test("new-thread model choices do not wait for subscription health", async ({ page }) => {
  await installProtocolFixtures(page);
  let releaseHealth!: () => void;
  const healthPending = new Promise<void>((resolve) => {
    releaseHealth = resolve;
  });
  await page.route("**/v1/subscriptions", async (route) => {
    await healthPending;
    await route.fulfill({ json: [] });
  });

  try {
    await page.goto("/");
    await page.getByRole("button", { name: "New thread", exact: true }).click();

    const setup = page.locator("trouve-new-thread-setup");
    const picker = setup.getByRole("combobox", { name: "Model" });
    await expect(setup.getByText("Loading modes and models…", { exact: true })).toHaveCount(0);
    await expect(picker).toBeEnabled();
    await expect(picker).toContainText("test/model");
  } finally {
    releaseHealth();
  }
});

test("the YOLO warning remains centered and exposes its hover text", async ({ page }) => {
  await installProtocolFixtures(page, { permissionMode: "yolo" });
  await page.goto("/");
  await replayHistory(page);

  const permission = page.locator(".permission-option");
  const select = permission.getByRole("combobox", { name: "Permission mode" });
  const warning = permission.locator(".permission-warning");
  await expect(select).toHaveValue("yolo");
  await expect(warning).toHaveAttribute("title", "YOLO: changes run without approval");
  await expect(warning).toHaveAttribute(
    "aria-label",
    "Warning: YOLO changes run without approval",
  );
  await warning.hover();

  const alignment = await warning.evaluate((element) => {
    const warning = element.getBoundingClientRect();
    const icon = element.querySelector<HTMLElement>(".trouve-icon")?.getBoundingClientRect();
    const select = element.parentElement
      ?.querySelector<HTMLSelectElement>("select")
      ?.getBoundingClientRect();
    if (icon === undefined || select === undefined) throw new Error("missing warning geometry");
    const centerY = (bounds: DOMRect): number => bounds.top + bounds.height / 2;
    return {
      iconToWarning: Math.abs(centerY(icon) - centerY(warning)),
      warningToSelect: Math.abs(centerY(warning) - centerY(select)),
    };
  });
  expect(alignment.iconToWarning).toBeLessThanOrEqual(1);
  expect(alignment.warningToSelect).toBeLessThanOrEqual(1);
});

test("pending image and file attachments reuse submitted chip geometry", async ({ page }) => {
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);

  await page.locator("trouve-thread-screen .attachment-button input").setInputFiles([
    {
      name: "preview.png",
      mimeType: "image/png",
      buffer: Buffer.from(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
        "base64",
      ),
    },
    {
      name: "notes.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("attachment notes"),
    },
  ]);

  const attachments = page.locator(".composer .pending-attachments");
  const image = attachments.locator(".image-attachment");
  const file = attachments.locator(".file-attachment");
  await expect(image.locator("img")).toHaveAttribute("src", /^data:image\/png;base64,/u);
  await expect(image).toContainText("image/png");
  await expect(file.locator('[data-font-awesome-icon="file"]')).toBeVisible();
  await expect(file).toContainText("text/plain");

  const geometry = await attachments.evaluate((list) => {
    const image = list.querySelector<HTMLElement>(".image-attachment");
    const preview = image
      ?.querySelector<HTMLElement>("trouve-image-preview")
      ?.shadowRoot
      ?.querySelector<HTMLElement>(".image-preview-trigger img");
    const file = list.querySelector<HTMLElement>(".file-attachment");
    const icon = file?.querySelector<HTMLElement>(".attachment-icon");
    if (image === null || image === undefined || preview === null || preview === undefined
      || file === null || file === undefined || icon === null || icon === undefined) {
      throw new Error("missing attachment chip geometry");
    }
    const imageStyle = getComputedStyle(image);
    const fileStyle = getComputedStyle(file);
    return {
      icon: [icon.offsetWidth, icon.offsetHeight],
      preview: [preview.offsetWidth, preview.offsetHeight],
      imageBackground: imageStyle.backgroundColor,
      fileBackground: fileStyle.backgroundColor,
      imageBorderRadius: imageStyle.borderRadius,
      fileBorderRadius: fileStyle.borderRadius,
      imagePadding: imageStyle.padding,
      filePadding: fileStyle.padding,
    };
  });
  expect(geometry.preview).toEqual([64, 48]);
  expect(geometry.icon).toEqual(geometry.preview);
  expect(geometry.fileBackground).toBe(geometry.imageBackground);
  expect(geometry.fileBorderRadius).toBe(geometry.imageBorderRadius);
  expect(geometry.filePadding).toBe(geometry.imagePadding);
});

test("image attachment thumbnails open an accessible full-size preview", async ({ page }) => {
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(16, {
      type: "turn.started",
      turn: 8,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(17, {
      type: "user.message",
      turn: 8,
      content: "Preview this image",
      attachments: [{
        id: "att_preview_1",
        name: "full-size-preview.png",
        mime: "image/png",
        size_bytes: 68,
      }],
    }),
    threadEvent(18, {
      type: "turn.completed",
      turn: 8,
      usage: { input_tokens: 4, output_tokens: 0, cost_usd: 0 },
      checkpoint_id: null,
    }),
  ]);

  const message = page.locator(".user-message").filter({ hasText: "Preview this image" });
  const preview = message.locator("trouve-image-preview");
  const trigger = preview.getByRole("button", {
    name: "View full-size image: full-size-preview.png",
  });
  await expect(trigger).toBeVisible();
  await trigger.click();

  const dialog = preview.getByRole("dialog", {
    name: "Full-size preview of full-size-preview.png",
  });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator(".image-preview-full")).toHaveAttribute(
    "src",
    "/v1/attachments/att_preview_1",
  );
  await expect(dialog.locator(".image-preview-full")).toHaveCSS("object-fit", "contain");
  await expect(dialog.getByRole("button", { name: "Close image preview" })).toBeFocused();

  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(trigger).toBeFocused();

  await trigger.click();
  await dialog.getByRole("button", { name: "Close image preview" }).click();
  await expect(dialog).toBeHidden();
  await expect(trigger).toBeFocused();
});

test("unsubmitted composer drafts persist per thread across navigation and reload", async ({
  page,
}) => {
  const sentMessages: Array<Record<string, unknown>> = [];
  await installProtocolFixtures(
    page,
    {
      sentMessages,
      additionalThreads: [{
      id: "th_second",
      session_id: "se_1",
      mode: "code",
      model: "test/second",
      model_options: {},
      permission_mode: "ask",
      created_at: "2026-08-03T08:00:00Z",
    }, {
      id: "th_third",
      session_id: "se_2",
      mode: "code",
      model: "test/third",
      model_options: {},
      permission_mode: "ask",
      created_at: "2026-08-02T08:00:00Z",
      }],
      additionalSessions: [{
      id: "se_2",
      workspace_id: "ws_1",
      title: "Second session",
      branch: "feature/second",
      worktree_path: "/tmp/second-session",
      base_ref: "main",
      created_at: "2026-08-03T08:00:00Z",
      }],
      additionalSessionSummaries: [{
      session_id: "se_2",
      workspace_id: "ws_1",
      archived: false,
      active: false,
      attention: "none",
      outcome: "idle",
      latest_cursor: 0,
      updated_at: "2026-08-03T08:00:00Z",
      }],
    },
  );
  const openSession = async (name: RegExp): Promise<void> => {
    const session = page.getByRole("button", { name });
    const mobileSessions = page.getByRole("button", {
      name: "Sessions",
      exact: true,
    });
    await expect.poll(async () =>
      await session.isVisible() || await mobileSessions.isVisible()
    ).toBe(true);
    if (!(await session.isVisible())) {
      await mobileSessions.evaluate((button: HTMLButtonElement) => button.click());
    }
    await expect(session).toBeVisible();
    await session.click();
  };
  await page.goto("/");
  await openSession(/Chat rendering feature\/chat/u);
  await page.getByRole("tab", { name: "Code · model", exact: true }).click();

  const textarea = page.locator('textarea[name="message"]');
  await textarea.fill("Draft for the first thread");
  await page.locator("trouve-thread-screen .attachment-button input").setInputFiles({
    name: "draft-notes.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("pending attachment"),
  });
  await expect(page.locator(".pending-attachments")).toContainText("draft-notes.txt");

  await page.getByRole("tab", { name: "Code · second", exact: true }).click();
  await expect(textarea).toHaveValue("");
  await expect(page.locator(".pending-attachments")).toHaveCount(0);
  await textarea.fill("Draft for the second thread");

  await openSession(/Second session feature\/second/u);
  await expect(textarea).toHaveValue("");
  await textarea.fill("Draft in a different session");
  await openSession(/Chat rendering feature\/chat/u);
  await expect(textarea).toHaveValue("Draft for the second thread");

  await page.getByRole("tab", { name: "Code · model", exact: true }).click();
  await expect(textarea).toHaveValue("Draft for the first thread");
  await expect(page.locator(".pending-attachments")).toContainText("draft-notes.txt");

  await expect.poll(() => page.evaluate(async () => await new Promise<number>((resolve) => {
    const request = indexedDB.open("trouve-composer-drafts");
    request.onerror = () => resolve(0);
    request.onblocked = () => resolve(0);
    request.onsuccess = () => {
      const database = request.result;
      if (!database.objectStoreNames.contains("thread-attachments")) {
        database.close();
        resolve(0);
        return;
      }
      let transaction: IDBTransaction;
      try {
        transaction = database.transaction("thread-attachments", "readonly");
      } catch {
        database.close();
        resolve(0);
        return;
      }
      const stored = transaction.objectStore("thread-attachments").get("th_fixture");
      stored.onerror = () => resolve(0);
      stored.onsuccess = () => {
        const count = Array.isArray(stored.result) ? stored.result.length : 0;
        database.close();
        resolve(count);
      };
    };
  }))).toBe(1);

  await page.reload();
  await expect(textarea).toHaveValue("Draft for the first thread");
  await expect(page.locator(".pending-attachments")).toContainText("draft-notes.txt");
  await page.getByRole("tab", { name: "Code · second", exact: true }).click();
  await expect(textarea).toHaveValue("Draft for the second thread");
  await openSession(/Second session feature\/second/u);
  await expect(textarea).toHaveValue("Draft in a different session");

  await openSession(/Chat rendering feature\/chat/u);
  await page.getByRole("tab", { name: "Code · model", exact: true }).click();
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(textarea).toHaveValue("");
  await expect(page.locator(".pending-attachments")).toHaveCount(0);
  expect(sentMessages).toHaveLength(1);
  await page.reload();
  await expect(textarea).toHaveValue("");
  await expect(page.locator(".pending-attachments")).toHaveCount(0);
  await page.getByRole("tab", { name: "Code · second", exact: true }).click();
  await expect(textarea).toHaveValue("Draft for the second thread");
  await openSession(/Second session feature\/second/u);
  await expect(textarea).toHaveValue("Draft in a different session");
});

test("chat cards unmount collapsed output and expose response copy actions", async ({ page }) => {
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);

  await expect(page.locator('select[aria-label="Thinking level"]')).toHaveValue("max");
  await expect(page.locator('select[aria-label="Thinking level"] option:checked')).toHaveText("Max");
  await expect(page.locator('select[aria-label="Context size"]')).toHaveValue("1m");
  await expect(page.locator(".user-message trouve-markdown-view strong")).toHaveText("migration");
  const userBody = page.locator(".user-body-stream").first();
  await expect(userBody).toHaveCSS("padding", "8px 16px 10px");
  const userProseInset = await userBody.evaluate((body) => {
    const bodyBounds = body.getBoundingClientRect();
    const bodyStyle = getComputedStyle(body);
    const contentLeft = bodyBounds.left
      + Number.parseFloat(bodyStyle.borderLeftWidth)
      + Number.parseFloat(bodyStyle.paddingLeft);
    const paragraph = body
      .querySelector<HTMLElement>("trouve-markdown-view")
      ?.shadowRoot?.querySelector<HTMLElement>("p")
      ?.getBoundingClientRect();
    return paragraph === undefined ? Number.NaN : paragraph.left - contentLeft;
  });
  expect(Math.abs(userProseInset - 10)).toBeLessThanOrEqual(1);
  const activityGroup = page.locator(".activity-group");
  await expect(activityGroup.getByText("Edited 1 file, read 1 file", { exact: true })).toBeVisible();
  await expect(activityGroup.locator(".activity-group-body")).toHaveCount(0);
  await expect(page.locator(".agent-text-block > trouve-markdown-view").first())
    .toContainText("I'll update it.");
  const agentBodyGeometry = await activityGroup.evaluate((group) => {
    const body = group.closest<HTMLElement>(".agent-body-stream");
    if (body === null) throw new Error("missing agent body");
    const bodyBounds = body.getBoundingClientRect();
    const bodyStyle = getComputedStyle(body);
    const contentLeft = bodyBounds.left
      + Number.parseFloat(bodyStyle.borderLeftWidth)
      + Number.parseFloat(bodyStyle.paddingLeft);
    const contentRight = bodyBounds.right
      - Number.parseFloat(bodyStyle.borderRightWidth)
      - Number.parseFloat(bodyStyle.paddingRight);
    const surfaces = [...body.children]
      .filter((child) => child.matches(
        ".agent-activity-timeline, .question-card, .context-compaction-marker",
      ))
      .map((child) => {
        const bounds = child.getBoundingClientRect();
        return {
          left: bounds.left - contentLeft,
          right: contentRight - bounds.right,
        };
      });
    const paragraph = body
      .querySelector<HTMLElement>(".agent-text-block > trouve-markdown-view")
      ?.shadowRoot?.querySelector<HTMLElement>("p")
      ?.getBoundingClientRect();
    return {
      padding: [
        bodyStyle.paddingTop,
        bodyStyle.paddingRight,
        bodyStyle.paddingBottom,
        bodyStyle.paddingLeft,
      ],
      proseInset: paragraph === undefined ? Number.NaN : paragraph.left - contentLeft,
      surfaces,
    };
  });
  expect(agentBodyGeometry.padding).toEqual(["8px", "16px", "10px", "16px"]);
  expect(Math.abs(agentBodyGeometry.proseInset - 10)).toBeLessThanOrEqual(1);
  expect(agentBodyGeometry.surfaces.length).toBeGreaterThan(0);
  for (const inset of agentBodyGeometry.surfaces) {
    expect(Math.abs(inset.left)).toBeLessThanOrEqual(1);
    expect(Math.abs(inset.right)).toBeLessThanOrEqual(1);
  }

  await activityGroup.locator(":scope > summary").click();
  await expect(activityGroup.locator(".tool-card")).toHaveCount(2);
  await page.mouse.move(0, 0);
  const groupedHeaderStyle = await activityGroup.locator(":scope > summary").evaluate(
    (summary) => {
      const style = getComputedStyle(summary);
      return {
        backgroundColor: style.backgroundColor,
        backgroundImage: style.backgroundImage,
        borderLeftWidth: style.borderLeftWidth,
        borderTopWidth: style.borderTopWidth,
        borderRadius: style.borderRadius,
      };
    },
  );
  const toolCardStyle = await activityGroup.locator(".tool-card").first().evaluate(
    (card) => {
      const style = getComputedStyle(card);
      return {
        backgroundColor: style.backgroundColor,
        borderLeftWidth: style.borderLeftWidth,
        borderTopWidth: style.borderTopWidth,
        borderRadius: style.borderRadius,
      };
    },
  );
  const groupNodeStyle = await activityGroup.evaluate((group) => {
    const style = getComputedStyle(group, "::before");
    return { display: style.display };
  });
  const groupDisclosureStyle = await activityGroup.locator(
    ":scope > summary .disclosure-icon",
  ).evaluate((icon) => {
    const style = getComputedStyle(icon);
    return {
      position: style.position,
      width: style.width,
    };
  });
  const timelineRailStyle = await activityGroup.locator("..").evaluate((timeline) => {
    const style = getComputedStyle(timeline, "::before");
    return {
      backgroundColor: style.backgroundColor,
      display: style.display,
      height: style.height,
      width: style.width,
    };
  });
  const groupStatusGeometry = await activityGroup.evaluate((group) => {
    const timeline = group.parentElement;
    const status = group.querySelector<HTMLElement>(
      ":scope > summary > .activity-group-status",
    );
    if (timeline === null || status === null) {
      throw new Error("missing grouped activity status geometry");
    }
    const timelineBounds = timeline.getBoundingClientRect();
    const rail = getComputedStyle(timeline, "::before");
    const statusBounds = status.getBoundingClientRect();
    const railCenter = timelineBounds.left
      + Number.parseFloat(rail.left)
      + Number.parseFloat(rail.width) / 2;
    return {
      display: getComputedStyle(status).display,
      statusToRail: Math.abs(
        statusBounds.left + statusBounds.width / 2 - railCenter,
      ),
    };
  });
  const groupedBodyBackground = await activityGroup.locator(".activity-group-body").evaluate(
    (body) => getComputedStyle(body).backgroundColor,
  );
  const groupedToolGap = await activityGroup.locator(".tool-card").evaluateAll((cards) => {
    const first = cards[0]?.getBoundingClientRect();
    const second = cards[1]?.getBoundingClientRect();
    return first === undefined || second === undefined
      ? Number.NaN
      : Math.round(second.top - first.bottom);
  });
  expect(groupedHeaderStyle.backgroundColor).toBe("rgba(0, 0, 0, 0)");
  expect(groupedHeaderStyle.backgroundImage).toBe("none");
  expect(groupedHeaderStyle.borderLeftWidth).toBe("0px");
  expect(groupedHeaderStyle.borderTopWidth).toBe("0px");
  expect(groupedHeaderStyle.borderRadius).toBe("4px");
  expect(toolCardStyle.backgroundColor).toBe("rgba(0, 0, 0, 0)");
  expect(toolCardStyle.borderLeftWidth).toBe("0px");
  expect(toolCardStyle.borderTopWidth).toBe("0px");
  expect(toolCardStyle.borderRadius).toBe("0px");
  expect(groupNodeStyle.display).toBe("none");
  expect(groupDisclosureStyle.position).toBe("static");
  expect(groupDisclosureStyle.width).toBe("10px");
  expect(timelineRailStyle.display).not.toBe("none");
  expect(timelineRailStyle.width).toBe("1px");
  expect(timelineRailStyle.backgroundColor).not.toBe("rgba(0, 0, 0, 0)");
  expect(groupStatusGeometry.display).not.toBe("none");
  expect(groupStatusGeometry.statusToRail).toBeLessThanOrEqual(0.25);
  expect(groupedBodyBackground).toBe("rgba(0, 0, 0, 0)");
  expect(groupedToolGap).toBe(6);
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

  const visibleThought = page.locator(".thinking-output");
  await expect(visibleThought.getByText("Thought", { exact: true })).toBeVisible();
  await expect(visibleThought.locator(".thinking-body")).toContainText("Compare both frontends");
  await expect(visibleThought.getByRole("button", { name: /thought process/i })).toHaveCount(1);
  await expect(visibleThought.locator(
    '.thinking-rail-icon [data-font-awesome-icon="brain"]',
  )).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Collapse thought process" })).toHaveCount(0);
  await expect(page.locator(".thinking-card")).toHaveCount(0);

  const agentCard = page.locator(".agent-turn-card").first();
  await agentCard.getByRole("button", { name: "Collapse agent message" }).click();
  await expect(agentCard.locator(":scope > .message-body")).toHaveCount(0);
  await expect(agentCard.locator(".agent-collapsed-preview")).toContainText("I'll update it.");
  await agentCard.getByRole("button", { name: "Expand agent message" }).click();
  const assistantCopy = agentCard.getByRole("button", { name: "Copy assistant output" });
  await expect(assistantCopy).toHaveCount(1);
  await expect(agentCard.getByRole("button", { name: /raw assistant output/i })).toHaveCount(0);
  await page.evaluate(() => {
    Object.defineProperty(globalThis.navigator, "clipboard", {
      configurable: true,
      value: {
        writeText(value: string) {
          (globalThis as typeof globalThis & { __trouveCopiedMarkdown?: string })
            .__trouveCopiedMarkdown = value;
          return Promise.resolve();
        },
      },
    });
  });
  await agentCard.hover();
  await expect(agentCard.locator(".agent-copy-action")).toHaveCSS("opacity", "1");
  await assistantCopy.click();
  await expect.poll(() => page.evaluate(() =>
    (globalThis as typeof globalThis & { __trouveCopiedMarkdown?: string })
      .__trouveCopiedMarkdown
  )).toBe("I'll update it.\n\nDone.");
  await agentCard.locator(".agent-text-block").first().click({ button: "right" });
  const messageMenu = page.getByRole("menu", { name: "Message actions" });
  await expect(messageMenu).toBeVisible();
  await expect(messageMenu.getByRole("menuitem", { name: "Copy as markdown" })).toBeFocused();
  await messageMenu.getByRole("menuitem", { name: "Copy as markdown" }).click();
  await expect(messageMenu).toHaveCount(0);
  await expect.poll(() => page.evaluate(() =>
    (globalThis as typeof globalThis & { __trouveCopiedMarkdown?: string })
      .__trouveCopiedMarkdown
  )).toBe("I'll update it.\n\nDone.");
  const agentDisclosure = agentCard.getByRole("button", { name: "Collapse agent message" });
  await agentDisclosure.focus();
  await agentDisclosure.press("Shift+F10");
  await expect(messageMenu).toBeVisible();
  await messageMenu.press("Escape");
  await expect(messageMenu).toHaveCount(0);
  await expect(agentDisclosure).toBeFocused();
  await agentCard.locator(".agent-text-block").first().evaluate((block) => {
    const paragraph = block
      .querySelector("trouve-markdown-view")
      ?.shadowRoot?.querySelector("p");
    if (paragraph === undefined || paragraph === null) throw new Error("missing response text");
    const range = document.createRange();
    range.selectNodeContents(paragraph);
    const selection = globalThis.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    block.dispatchEvent(new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 80,
      clientY: 80,
      composed: true,
    }));
  });
  await expect(messageMenu.getByRole("menuitem", { name: "Copy", exact: true })).toBeVisible();
  await messageMenu.getByRole("menuitem", { name: "Copy as markdown" }).click();
  await expect.poll(() => page.evaluate(() =>
    (globalThis as typeof globalThis & { __trouveCopiedMarkdown?: string })
      .__trouveCopiedMarkdown
  )).toBe("I'll update it.");

  const result = await new AxeBuilder({ page })
    .include(".chat-stream")
    .include(".composer")
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
    .analyze();
  expect(result.violations.filter(({ impact }) =>
    impact === "serious" || impact === "critical"
  )).toEqual([]);
});

test("the Chat preference adds one disclosure around the standard activity timeline", async ({ page }) => {
  await installProtocolFixtures(page);
  await page.addInitScript(() => {
    localStorage.setItem("trouve.chat.v1", JSON.stringify({
      collapseThinkingWithTools: true,
    }));
  });
  await page.goto("/");
  await replayHistory(page);

  const historicalAgent = page.locator(".agent-turn-card").first();
  const thoughtGroup = historicalAgent.locator(".activity-group").filter({
    hasText: "Thought 1 time",
  });
  await expect(thoughtGroup.locator(":scope > .activity-group-body")).toHaveCount(0);
  await thoughtGroup.locator(":scope > summary").click();
  const historicalThought = thoughtGroup.locator(".thinking-output");
  await expect(historicalThought.locator(".thinking-body"))
    .toContainText("Compare both frontends");
  await expect(historicalThought.getByRole("button", {
    name: /^(?:Collapse|Expand) thought process$/u,
  }))
    .toHaveCount(0);
  await expect(thoughtGroup.locator(".thinking-card")).toHaveCount(0);
  await expect(thoughtGroup.locator(".activity-group-timeline")).toBeVisible();

  await emitBatch(page, [
    threadEvent(16, {
      type: "turn.started",
      turn: 8,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(17, {
      type: "user.message",
      turn: 8,
      content: "Collapse one contiguous activity run",
      attachments: [],
    }),
    threadEvent(18, {
      type: "assistant.thinking",
      turn: 8,
      text: "Inspect the grouped activity geometry",
    }),
    threadEvent(19, {
      type: "tool.requested",
      turn: 8,
      call_id: "collapsed_thinking_1",
      tool: "Bash",
      args: { command: "first" },
      requires_approval: false,
    }),
    threadEvent(20, { type: "tool.started", call_id: "collapsed_thinking_1" }),
    threadEvent(21, {
      type: "tool.completed",
      call_id: "collapsed_thinking_1",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(22, {
      type: "tool.requested",
      turn: 8,
      call_id: "collapsed_thinking_2",
      tool: "Bash",
      args: { command: "second" },
      requires_approval: false,
    }),
    threadEvent(23, { type: "tool.started", call_id: "collapsed_thinking_2" }),
    threadEvent(24, {
      type: "tool.completed",
      call_id: "collapsed_thinking_2",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(25, {
      type: "assistant.message",
      turn: 8,
      content: "The combined activity is complete.",
    }),
    threadEvent(26, {
      type: "turn.completed",
      turn: 8,
      usage: { input_tokens: 10, output_tokens: 4 },
      checkpoint_id: null,
    }),
  ]);

  const combinedAgent = page.locator(".agent-turn-card").last();
  const combinedTimeline = combinedAgent.locator(
    ":scope > .message-body > .agent-activity-timeline.single-activity",
  );
  const combinedGroup = combinedTimeline.locator(":scope > .activity-group");
  await expect(combinedGroup.getByText(
    "Ran 2 commands, thought 1 time",
    { exact: true },
  )).toBeVisible();
  await expect(combinedGroup.locator(".activity-group-body")).toHaveCount(0);

  const geometry = await combinedGroup.evaluate((group) => {
    const timeline = group.parentElement;
    const summary = group.querySelector<HTMLElement>(":scope > summary");
    if (summary === null) throw new Error("missing combined activity summary");
    const disclosure = summary.querySelector<HTMLElement>(".disclosure-icon");
    const status = summary.querySelector<HTMLElement>(".activity-group-status");
    const label = summary.querySelector<HTMLElement>("strong");
    const prose = timeline?.nextElementSibling
      ?.querySelector<HTMLElement>("trouve-markdown-view")
      ?.shadowRoot?.querySelector<HTMLElement>("p");
    if (timeline === null || disclosure === null || status === null
      || label === null || prose === null || prose === undefined) {
      throw new Error("missing combined activity geometry");
    }
    const timelineBounds = timeline.getBoundingClientRect();
    const disclosureBounds = disclosure.getBoundingClientRect();
    const statusBounds = status.getBoundingClientRect();
    const rail = getComputedStyle(timeline, "::before");
    const railCenter = timelineBounds.left
      + Number.parseFloat(rail.left)
      + Number.parseFloat(rail.width) / 2;
    return {
      statusToRail: Math.abs(
        statusBounds.left + statusBounds.width / 2 - railCenter,
      ),
      disclosureAfterRail: disclosureBounds.left - railCenter,
      groupNodeDisplay: getComputedStyle(group, "::before").display,
      labelToProse: label.getBoundingClientRect().left
        - prose.getBoundingClientRect().left,
      railHeight: Number.parseFloat(rail.height),
    };
  });
  expect(geometry.statusToRail).toBeLessThanOrEqual(0.25);
  expect(geometry.disclosureAfterRail).toBeGreaterThan(10);
  expect(geometry.groupNodeDisplay).toBe("none");
  expect(geometry.labelToProse).toBeLessThanOrEqual(40);
  expect(geometry.railHeight).toBeGreaterThan(10);

  await combinedGroup.locator(":scope > summary").click();
  await expect(combinedTimeline).toHaveClass(/has-expanded-group/u);
  await expect(combinedGroup.locator(".thinking-card")).toHaveCount(0);
  await expect(combinedGroup.locator(".thinking-output .thinking-body"))
    .toContainText("Inspect the grouped activity geometry");
  await expect(combinedGroup.getByRole("button", {
    name: /^(?:Collapse|Expand) thought process$/u,
  }))
    .toHaveCount(0);
  await page.mouse.move(0, 0);
  const expandedGeometry = await combinedGroup.evaluate((group) => {
    const timeline = group.parentElement;
    const summary = group.querySelector<HTMLElement>(":scope > summary");
    const body = group.querySelector<HTMLElement>(":scope > .activity-group-body");
    const nestedTimeline = body?.querySelector<HTMLElement>(
      ":scope > .activity-group-timeline",
    );
    const thought = nestedTimeline?.querySelector<HTMLElement>(":scope > .thinking-output");
    const thoughtIcon = thought?.querySelector<HTMLElement>(":scope > .thinking-rail-icon");
    const toolStatus = nestedTimeline?.querySelector<HTMLElement>(
      ":scope > .tool-card .tool-status",
    );
    const groupStatus = summary?.querySelector<HTMLElement>(
      ":scope > .activity-group-status",
    );
    const toolCard = toolStatus?.closest<HTMLElement>(".tool-card");
    if (timeline === null || summary === null || body === null
      || nestedTimeline === null || nestedTimeline === undefined
      || thought === null || thought === undefined
      || thoughtIcon === null || thoughtIcon === undefined
      || toolStatus === null || toolStatus === undefined
      || groupStatus === null || groupStatus === undefined
      || toolCard === null || toolCard === undefined) {
      throw new Error("missing expanded combined activity geometry");
    }
    const outerRail = getComputedStyle(timeline, "::before");
    const nestedRail = getComputedStyle(nestedTimeline, "::before");
    const timelineBounds = timeline.getBoundingClientRect();
    const nestedTimelineBounds = nestedTimeline.getBoundingClientRect();
    const thoughtIconBounds = thoughtIcon.getBoundingClientRect();
    const toolStatusBounds = toolStatus.getBoundingClientRect();
    const groupStatusBounds = groupStatus.getBoundingClientRect();
    const nestedRailCenter = nestedTimelineBounds.left
      + Number.parseFloat(nestedRail.left)
      + Number.parseFloat(nestedRail.width) / 2;
    const outerRailCenter = timelineBounds.left
      + Number.parseFloat(outerRail.left)
      + Number.parseFloat(outerRail.width) / 2;
    return {
      railDisplay: getComputedStyle(timeline, "::before").display,
      nestedRailDisplay: nestedRail.display,
      nestedRailHeight: Number.parseFloat(nestedRail.height),
      nestedRailIndent: nestedRailCenter - outerRailCenter,
      railWidthMatches: outerRail.width === nestedRail.width,
      railColorMatches: outerRail.backgroundColor === nestedRail.backgroundColor,
      railOpacityMatches: outerRail.opacity === nestedRail.opacity,
      timelineGap: getComputedStyle(timeline).rowGap,
      nestedTimelineGap: getComputedStyle(nestedTimeline).rowGap,
      groupStatusToRail: Math.abs(
        groupStatusBounds.left + groupStatusBounds.width / 2 - outerRailCenter,
      ),
      thoughtIconDisplay: getComputedStyle(thoughtIcon).display,
      thoughtIconToRail: Math.abs(
        thoughtIconBounds.left + thoughtIconBounds.width / 2 - nestedRailCenter,
      ),
      toolStatusToRail: Math.abs(
        toolStatusBounds.left + toolStatusBounds.width / 2 - nestedRailCenter,
      ),
      duplicateToolNodeDisplay: getComputedStyle(toolCard, "::before").display,
      summaryBackground: getComputedStyle(summary).backgroundColor,
      bodyPaddingLeft: getComputedStyle(body).paddingLeft,
      nestedMarginLeft: getComputedStyle(nestedTimeline).marginLeft,
    };
  });
  expect(expandedGeometry.railDisplay).not.toBe("none");
  expect(expandedGeometry.nestedRailDisplay).not.toBe("none");
  expect(expandedGeometry.nestedRailHeight).toBeGreaterThan(10);
  expect(expandedGeometry.nestedRailIndent).toBe(20);
  expect(expandedGeometry.railWidthMatches).toBe(true);
  expect(expandedGeometry.railColorMatches).toBe(true);
  expect(expandedGeometry.railOpacityMatches).toBe(true);
  expect(expandedGeometry.timelineGap).toBe("6px");
  expect(expandedGeometry.nestedTimelineGap).toBe("6px");
  expect(expandedGeometry.groupStatusToRail).toBeLessThanOrEqual(0.25);
  expect(expandedGeometry.thoughtIconDisplay).not.toBe("none");
  expect(expandedGeometry.thoughtIconToRail).toBeLessThanOrEqual(0.25);
  expect(expandedGeometry.toolStatusToRail).toBeLessThanOrEqual(0.25);
  expect(expandedGeometry.duplicateToolNodeDisplay).toBe("none");
  expect(expandedGeometry.summaryBackground).toBe("rgba(0, 0, 0, 0)");
  expect(expandedGeometry.bodyPaddingLeft).toBe("0px");
  expect(expandedGeometry.nestedMarginLeft).toBe("0px");
});

test("the Chat preference persists across a frontend reload", async ({ page }) => {
  await installProtocolFixtures(page);
  await page.goto("/settings/chat");

  const preference = page.getByRole("checkbox", {
    name: "Collapse thinking output with tool calls.",
  });
  await expect(preference).not.toBeChecked();
  await page.locator('label[for="settings-collapse-thinking"]').click();
  await expect(preference).toBeChecked();
  const compactionPreference = page.getByRole("checkbox", {
    name: "Collapse context compaction with tool calls.",
  });
  await expect(compactionPreference).not.toBeChecked();
  await page.locator('label[for="settings-collapse-compaction"]').click();
  await expect(compactionPreference).toBeChecked();
  await expect.poll(() => page.evaluate(() =>
    localStorage.getItem("trouve.chat.v1")
  )).toBe(JSON.stringify({
    collapseThinkingWithTools: true,
    collapseCompactionWithTools: true,
  }));

  await page.reload();
  await expect(page.getByRole("checkbox", {
    name: "Collapse thinking output with tool calls.",
  })).toBeChecked();
  await expect(page.getByRole("checkbox", {
    name: "Collapse context compaction with tool calls.",
  })).toBeChecked();
});

test("legacy context compaction tools stay outside collapsed-thinking groups", async ({
  page,
}) => {
  await installProtocolFixtures(page);
  await page.addInitScript(() => {
    localStorage.setItem("trouve.chat.v1", JSON.stringify({
      collapseThinkingWithTools: true,
    }));
  });
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(40, {
      type: "turn.started",
      turn: 11,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(41, {
      type: "user.message",
      turn: 11,
      content: "Continue through legacy compaction",
      attachments: [],
    }),
    threadEvent(42, {
      type: "assistant.thinking",
      turn: 11,
      text: "Prepare the earlier context",
    }),
    threadEvent(43, {
      type: "tool.requested",
      turn: 11,
      call_id: "before_legacy_compaction",
      tool: "Bash",
      args: { command: "before" },
      requires_approval: false,
    }),
    threadEvent(44, {
      type: "tool.completed",
      call_id: "before_legacy_compaction",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(45, {
      type: "tool.requested",
      turn: 11,
      call_id: "legacy_compaction",
      tool: "contextCompaction",
      args: {},
      requires_approval: false,
    }),
    threadEvent(46, {
      type: "tool.completed",
      call_id: "legacy_compaction",
      status: "ok",
      result: {},
    }),
    threadEvent(47, {
      type: "assistant.thinking",
      turn: 11,
      text: "Continue from the compacted context",
    }),
    threadEvent(48, {
      type: "tool.requested",
      turn: 11,
      call_id: "after_legacy_compaction",
      tool: "Bash",
      args: { command: "after" },
      requires_approval: false,
    }),
    threadEvent(49, {
      type: "tool.completed",
      call_id: "after_legacy_compaction",
      status: "ok",
      result: { exit_code: 0 },
    }),
  ]);

  const agent = page.locator(".agent-turn-card").last();
  await expect(agent.locator('.tool-card[data-call-id="legacy_compaction"]')).toHaveCount(0);
  await expect(agent.locator(".context-compaction-marker.completed"))
    .toContainText("Context compacted");
  const topLevelKinds = await agent.locator(":scope > .message-body > *").evaluateAll(
    (elements) => elements.map((element) =>
      element.classList.contains("agent-activity-timeline")
        ? "timeline"
        : element.classList.contains("context-compaction-marker")
          ? "compaction"
          : "other"),
  );
  expect(topLevelKinds.slice(0, 3)).toEqual(["timeline", "compaction", "timeline"]);

  const timelines = agent.locator(":scope > .message-body > .agent-activity-timeline");
  await expect(timelines).toHaveCount(2);
  await expect(timelines.nth(0)).toHaveClass(/compaction-connected-timeline/u);
  await expect(timelines.nth(1)).toHaveClass(/compaction-connected-timeline/u);
  const groups = timelines.locator(":scope > .activity-group");
  await expect(groups).toHaveCount(2);
  await groups.nth(0).locator(":scope > summary").click();
  await groups.nth(1).locator(":scope > summary").click();
  await expect(groups.nth(0)).toHaveAttribute("open", "");
  await expect(groups.nth(1)).toHaveAttribute("open", "");

  const connection = await agent.locator(
    ":scope > .message-body > .context-compaction-marker",
  ).evaluate((marker) => {
    const before = marker.previousElementSibling;
    const after = marker.nextElementSibling;
    const symbol = marker.querySelector<HTMLElement>(".context-compaction-symbol");
    const beforeNested = before?.querySelector<HTMLElement>(".activity-group-timeline");
    const afterNested = after?.querySelector<HTMLElement>(".activity-group-timeline");
    if (!(before instanceof HTMLElement) || !(after instanceof HTMLElement)
      || symbol === null || beforeNested === null || beforeNested === undefined
      || afterNested === null || afterNested === undefined) {
      throw new Error("missing connected compaction timeline geometry");
    }
    const segment = (element: HTMLElement, pseudo: string) => {
      const bounds = element.getBoundingClientRect();
      const style = getComputedStyle(element, pseudo);
      const top = bounds.top + Number.parseFloat(style.top);
      const left = bounds.left + Number.parseFloat(style.left);
      return {
        top,
        bottom: top + Number.parseFloat(style.height),
        center: left + Number.parseFloat(style.width) / 2,
        display: style.display,
      };
    };
    const beforeRail = segment(before, "::before");
    const bridge = segment(marker as HTMLElement, "::before");
    const afterRail = segment(after, "::before");
    const beforeNestedRail = segment(beforeNested, "::before");
    const afterNestedRail = segment(afterNested, "::before");
    const symbolBounds = symbol.getBoundingClientRect();
    return {
      beforeGap: Math.abs(beforeRail.bottom - bridge.top),
      afterGap: Math.abs(bridge.bottom - afterRail.top),
      centerDrift: Math.max(
        Math.abs(beforeRail.center - bridge.center),
        Math.abs(bridge.center - afterRail.center),
        Math.abs(
          symbolBounds.left + symbolBounds.width / 2 - bridge.center,
        ),
      ),
      bridgeDisplay: bridge.display,
      beforeNestedRail: beforeNestedRail.display,
      afterNestedRail: afterNestedRail.display,
      beforeNestedIndent: beforeNestedRail.center - beforeRail.center,
      afterNestedIndent: afterNestedRail.center - afterRail.center,
    };
  });
  expect(connection.beforeGap).toBeLessThanOrEqual(0.25);
  expect(connection.afterGap).toBeLessThanOrEqual(0.25);
  expect(connection.centerDrift).toBeLessThanOrEqual(0.25);
  expect(connection.bridgeDisplay).not.toBe("none");
  expect(connection.beforeNestedRail).not.toBe("none");
  expect(connection.afterNestedRail).not.toBe("none");
  expect(connection.beforeNestedIndent).toBe(20);
  expect(connection.afterNestedIndent).toBe(20);
});

test("running activity groups retain explicit disclosure state as tools arrive", async ({
  page,
}) => {
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(16, {
      type: "turn.started",
      turn: 8,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(17, {
      type: "user.message",
      turn: 8,
      content: "Run several commands",
      attachments: [],
    }),
    threadEvent(18, {
      type: "tool.requested",
      turn: 8,
      call_id: "call_group_1",
      tool: "Bash",
      args: { command: "first" },
      requires_approval: false,
    }),
    threadEvent(19, { type: "tool.started", call_id: "call_group_1" }),
    threadEvent(20, {
      type: "tool.completed",
      call_id: "call_group_1",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(21, {
      type: "tool.requested",
      turn: 8,
      call_id: "call_group_2",
      tool: "Bash",
      args: { command: "second" },
      requires_approval: false,
    }),
    threadEvent(22, { type: "tool.started", call_id: "call_group_2" }),
  ]);

  const group = page.locator(".activity-group").last();
  await expect(group).toBeVisible();
  await expect(group.getByText("Ran 2 commands", { exact: true })).toBeVisible();
  await expect(group.locator(".activity-group-body")).toHaveCount(0);

  await group.locator(":scope > summary").click();
  await expect(group.locator(".tool-card")).toHaveCount(2);
  await emitBatch(page, [
    threadEvent(23, {
      type: "tool.completed",
      call_id: "call_group_2",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(24, {
      type: "tool.requested",
      turn: 8,
      call_id: "call_group_3",
      tool: "Bash",
      args: { command: "third" },
      requires_approval: false,
    }),
    threadEvent(25, { type: "tool.started", call_id: "call_group_3" }),
  ]);
  await expect(group.getByText("Ran 3 commands", { exact: true })).toBeVisible();
  await expect(group.locator(".tool-card")).toHaveCount(3);

  await group.locator(":scope > summary").click();
  await expect(group.locator(".activity-group-body")).toHaveCount(0);
  await emitBatch(page, [
    threadEvent(26, {
      type: "tool.completed",
      call_id: "call_group_3",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(27, {
      type: "tool.requested",
      turn: 8,
      call_id: "call_group_4",
      tool: "Bash",
      args: { command: "fourth" },
      requires_approval: false,
    }),
    threadEvent(28, { type: "tool.started", call_id: "call_group_4" }),
  ]);
  await expect(group.getByText("Ran 4 commands", { exact: true })).toBeVisible();
  await expect(group.locator(".activity-group-body")).toHaveCount(0);
});

test("context compaction is an animated durable boundary between tool groups", async ({
  page,
}) => {
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(30, {
      type: "turn.started",
      turn: 10,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(31, {
      type: "user.message",
      turn: 10,
      content: "Continue through compaction",
      attachments: [],
    }),
    threadEvent(32, {
      type: "tool.requested",
      turn: 10,
      call_id: "before_compaction_1",
      tool: "Bash",
      args: { command: "first" },
      requires_approval: false,
    }),
    threadEvent(33, {
      type: "tool.requested",
      turn: 10,
      call_id: "before_compaction_2",
      tool: "Bash",
      args: { command: "second" },
      requires_approval: false,
    }),
    threadEvent(34, { type: "thread.compaction_started", turn: 10 }),
  ]);

  const agent = page.locator(".agent-turn-card").last();
  const running = agent.locator(".context-compaction-marker.running");
  await expect(running).toBeVisible();
  await expect(running.getByText("Compacting context", { exact: true })).toBeVisible();
  await expect(agent.locator(".activity-group")).toHaveCount(1);
  await expect(agent.locator(".activity-group .context-compaction-marker")).toHaveCount(0);
  expect(await running.locator(".trouve-icon-spin").evaluate(
    (element) => getComputedStyle(element).animationName,
  )).not.toBe("none");

  // Active compaction keeps the containing Agent card open until its
  // terminal state arrives.
  await expect(agent.getByRole("button", { name: "Collapse agent message" }))
    .toHaveAttribute("aria-disabled", "true");

  await emitBatch(page, [
    threadEvent(35, {
      type: "thread.compaction_completed",
      turn: 10,
      messages_compacted: 24,
    }),
    threadEvent(36, {
      type: "tool.requested",
      turn: 10,
      call_id: "after_compaction_1",
      tool: "Bash",
      args: { command: "third" },
      requires_approval: false,
    }),
    threadEvent(37, {
      type: "tool.requested",
      turn: 10,
      call_id: "after_compaction_2",
      tool: "Bash",
      args: { command: "fourth" },
      requires_approval: false,
    }),
  ]);

  const marker = agent.locator(".context-compaction-marker.completed");
  await expect(marker).toContainText("Context compacted");
  await expect(marker).toContainText("24 earlier transcript messages summarized");
  await expect(marker).toHaveClass(/timeline-connect-before/u);
  await expect(marker).toHaveClass(/timeline-connect-after/u);
  const completedMarkerStyle = await marker.evaluate((element) => {
    const style = getComputedStyle(element);
    const rule = getComputedStyle(element, "::after");
    return {
      backgroundImage: style.backgroundImage,
      borderTopWidth: style.borderTopWidth,
      borderBottomWidth: style.borderBottomWidth,
      ruleContent: rule.content,
      ruleHeight: rule.height,
    };
  });
  expect(completedMarkerStyle).toEqual({
    backgroundImage: "none",
    borderTopWidth: "0px",
    borderBottomWidth: "0px",
    ruleContent: '\"\"',
    ruleHeight: "1px",
  });
  const topLevelKinds = await agent.locator(":scope > .message-body > *").evaluateAll(
    (elements) => elements.map((element) =>
      element.classList.contains("agent-activity-timeline")
        ? "timeline"
        : element.classList.contains("context-compaction-marker")
          ? "compaction"
          : "other"),
  );
  expect(topLevelKinds.slice(0, 3)).toEqual(["timeline", "compaction", "timeline"]);
  await expect(agent.locator(":scope > .message-body > .agent-activity-timeline"))
    .toHaveCount(2);
  await expect(agent.locator(
    ":scope > .message-body > .agent-activity-timeline.compaction-connected-timeline",
  )).toHaveCount(2);
  await expect(agent.locator(
    ":scope > .message-body > .agent-activity-timeline > .activity-group",
  )).toHaveCount(2);
});

test("the Chat preference collapses context compaction into one tool activity run", async ({
  page,
}) => {
  await installProtocolFixtures(page);
  await page.addInitScript(() => {
    localStorage.setItem("trouve.chat.v1", JSON.stringify({
      collapseThinkingWithTools: false,
      collapseCompactionWithTools: true,
    }));
  });
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(60, {
      type: "turn.started",
      turn: 13,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(61, {
      type: "user.message",
      turn: 13,
      content: "Collapse the compaction boundary",
      attachments: [],
    }),
    threadEvent(62, {
      type: "tool.requested",
      turn: 13,
      call_id: "before_collapsed_compaction",
      tool: "Bash",
      args: { command: "before" },
      requires_approval: false,
    }),
    threadEvent(63, {
      type: "tool.completed",
      call_id: "before_collapsed_compaction",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(64, { type: "thread.compaction_started", turn: 13 }),
    threadEvent(65, {
      type: "thread.compaction_completed",
      turn: 13,
      messages_compacted: 12,
    }),
    threadEvent(66, {
      type: "tool.requested",
      turn: 13,
      call_id: "after_collapsed_compaction",
      tool: "Bash",
      args: { command: "after" },
      requires_approval: false,
    }),
    threadEvent(67, {
      type: "tool.completed",
      call_id: "after_collapsed_compaction",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(68, {
      type: "turn.completed",
      turn: 13,
      usage: { input_tokens: 10, output_tokens: 2 },
      checkpoint_id: null,
    }),
  ]);

  const agent = page.locator(".agent-turn-card").last();
  const topLevelTimeline = agent.locator(
    ":scope > .message-body > .agent-activity-timeline",
  );
  await expect(topLevelTimeline).toHaveCount(1);
  await expect(agent.locator(
    ":scope > .message-body > .context-compaction-marker",
  )).toHaveCount(0);

  const group = topLevelTimeline.locator(":scope > .activity-group");
  await expect(group.getByText(
    "Ran 2 commands, compacted context",
    { exact: true },
  )).toBeVisible();
  await expect(group.locator(":scope > .activity-group-body")).toHaveCount(0);

  await group.locator(":scope > summary").click();
  const marker = group.locator(
    ".activity-group-timeline > .context-compaction-marker.completed",
  );
  await expect(marker).toContainText("Context compacted");
  await expect(marker).toContainText("12 earlier transcript messages summarized");
  await expect(marker).toHaveClass(/nested-timeline-marker/u);
  await expect(group.locator(".tool-card")).toHaveCount(2);
  const markerAlignment = await marker.evaluate((element) => {
    const timeline = element.parentElement;
    const symbol = element.querySelector<HTMLElement>(".context-compaction-symbol");
    if (timeline === null || symbol === null) {
      throw new Error("missing nested compaction timeline geometry");
    }
    const timelineBounds = timeline.getBoundingClientRect();
    const rail = getComputedStyle(timeline, "::before");
    const symbolBounds = symbol.getBoundingClientRect();
    const railCenter = timelineBounds.left
      + Number.parseFloat(rail.left)
      + Number.parseFloat(rail.width) / 2;
    return {
      centerDrift: Math.abs(
        symbolBounds.left + symbolBounds.width / 2 - railCenter,
      ),
      horizontalOverflow: element.scrollWidth - element.clientWidth,
    };
  });
  expect(markerAlignment.centerDrift).toBeLessThanOrEqual(0.25);
  expect(markerAlignment.horizontalOverflow).toBeLessThanOrEqual(0);
});

test("standalone tool headers align their timeline node and disclosure controls", async ({
  page,
}) => {
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(50, {
      type: "turn.started",
      turn: 12,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(51, {
      type: "user.message",
      turn: 12,
      content: "Align one standalone tool call",
      attachments: [],
    }),
    threadEvent(52, {
      type: "tool.requested",
      turn: 12,
      call_id: "standalone_alignment",
      tool: "Bash",
      args: { command: "printf aligned" },
      requires_approval: false,
    }),
    threadEvent(53, { type: "tool.started", call_id: "standalone_alignment" }),
    threadEvent(54, {
      type: "tool.completed",
      call_id: "standalone_alignment",
      status: "ok",
      result: { exit_code: 0, stdout: "aligned" },
    }),
    threadEvent(55, {
      type: "assistant.message",
      turn: 12,
      content: "Aligned.",
    }),
    threadEvent(56, {
      type: "turn.completed",
      turn: 12,
      usage: { input_tokens: 12, output_tokens: 2 },
      checkpoint_id: null,
    }),
  ]);

  const tool = page.locator(
    '.agent-turn-card:last-of-type .agent-activity-timeline > .tool-card[data-call-id="standalone_alignment"]',
  );
  await expect(tool).toBeVisible();
  const alignment = async () => await tool.evaluate((card) => {
    const summary = card.querySelector<HTMLElement>(":scope > summary");
    const disclosure = summary?.querySelector<HTMLElement>(".tool-disclosure");
    const status = summary?.querySelector<HTMLElement>(".tool-status");
    const title = summary?.querySelector<HTMLElement>(":scope > strong");
    const timeline = card.parentElement;
    if (summary === null || summary === undefined || disclosure === null
      || disclosure === undefined || status === null || status === undefined
      || title === null || title === undefined || timeline === null) {
      throw new Error("missing standalone tool header geometry");
    }
    const timelineBounds = timeline.getBoundingClientRect();
    const rail = getComputedStyle(timeline, "::before");
    const railCenter = timelineBounds.left
      + Number.parseFloat(rail.left)
      + Number.parseFloat(rail.width) / 2;
    const center = (element: HTMLElement): number => {
      const bounds = element.getBoundingClientRect();
      return bounds.top + bounds.height / 2;
    };
    const statusBounds = status.getBoundingClientRect();
    const statusCenter = center(status);
    return {
      disclosure: Math.abs(center(disclosure) - statusCenter),
      title: Math.abs(center(title) - statusCenter),
      titleFontSize: getComputedStyle(title).fontSize,
      titleFontWeight: getComputedStyle(title).fontWeight,
      statusWidth: statusBounds.width,
      statusHeight: statusBounds.height,
      statusToRail: Math.abs(
        statusBounds.left + statusBounds.width / 2 - railCenter,
      ),
      duplicateNodeDisplay: getComputedStyle(card, "::before").display,
      summaryOverflow: getComputedStyle(summary).overflow,
      statusOutsideSummary: statusBounds.left < summary.getBoundingClientRect().left,
    };
  });

  const collapsedAlignment = await alignment();
  expect(collapsedAlignment.duplicateNodeDisplay).toBe("none");
  expect(collapsedAlignment.summaryOverflow).toBe("visible");
  expect(collapsedAlignment.statusOutsideSummary).toBe(true);
  expect(collapsedAlignment.titleFontSize).toBe("11px");
  expect(collapsedAlignment.titleFontWeight).toBe("600");
  expect(collapsedAlignment.statusWidth).toBe(10);
  expect(collapsedAlignment.statusHeight).toBe(10);
  expect(Math.max(
    collapsedAlignment.disclosure,
    collapsedAlignment.title,
    collapsedAlignment.statusToRail,
  )).toBeLessThanOrEqual(0.25);
  await tool.locator(":scope > summary").click();
  await expect(tool).toHaveAttribute("open", "");
  const expandedAlignment = await alignment();
  expect(expandedAlignment.duplicateNodeDisplay).toBe("none");
  expect(Math.max(
    expandedAlignment.disclosure,
    expandedAlignment.title,
    expandedAlignment.statusToRail,
  )).toBeLessThanOrEqual(0.25);
});

test("thought completion clears stale activity while standalone and grouped tools share one rail", async ({
  page,
}) => {
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(70, {
      type: "turn.started",
      turn: 14,
      mode: "code",
      model: "codex/gpt-5.6-sol",
    }),
    threadEvent(71, {
      type: "user.message",
      turn: 14,
      content: "Keep every activity on one rail",
      attachments: [],
    }),
    threadEvent(72, {
      type: "assistant.thinking",
      turn: 14,
      text: "Waiting for a provider boundary.",
    }),
  ]);

  const agent = page.locator(".agent-turn-card").last();
  await expect(agent.locator(".agent-activity")).toContainText("Thinking…");
  await emit(page, threadEvent(73, {
    type: "assistant.thinking_completed",
    turn: 14,
  }));
  await expect(agent.locator(".agent-activity")).toContainText("Processing…");

  await emitBatch(page, [
    threadEvent(74, {
      type: "tool.requested",
      turn: 14,
      call_id: "before_group",
      tool: "commandExecution",
      args: { command: "printf first" },
      requires_approval: false,
    }),
    threadEvent(75, { type: "tool.started", call_id: "before_group" }),
    threadEvent(76, {
      type: "tool.completed",
      call_id: "before_group",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(77, {
      type: "assistant.thinking",
      turn: 14,
      text: "Starting a command group.",
    }),
    threadEvent(78, {
      type: "assistant.thinking_completed",
      turn: 14,
    }),
    threadEvent(79, {
      type: "tool.requested",
      turn: 14,
      call_id: "group_one",
      tool: "commandExecution",
      args: { command: "printf one" },
      requires_approval: false,
    }),
    threadEvent(80, {
      type: "tool.completed",
      call_id: "group_one",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(81, {
      type: "tool.requested",
      turn: 14,
      call_id: "group_two",
      tool: "commandExecution",
      args: { command: "sleep 1" },
      requires_approval: false,
    }),
  ]);

  const timeline = agent.locator(":scope > .message-body > .agent-activity-timeline");
  await expect(timeline).toHaveCount(1);
  await expect(timeline.locator(
    ':scope > .tool-card[data-call-id="before_group"]',
  )).toBeVisible();
  await expect(timeline.locator(":scope > .activity-group")).toContainText("2 commands");
  await expect(agent.locator(".agent-activity")).toContainText("Running commands…");
});

test(VIRTUAL_DISCLOSURE_GEOMETRY_TEST, async ({
  page,
}, testInfo) => {
  test.skip(
    !["desktop-chromium", "desktop-webkit"].includes(testInfo.project.name),
    "Desktop Chromium and WebKit own the frame-level virtual-row geometry regression",
  );
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(16, {
      type: "turn.started",
      turn: 8,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(17, {
      type: "user.message",
      turn: 8,
      content: "Fill the history before the disclosure",
      attachments: [],
    }),
    threadEvent(18, {
      type: "assistant.message",
      turn: 8,
      content: Array.from(
        { length: 30 },
        (_, index) => `Earlier response paragraph ${index}: preserve its virtual height.`,
      ).join("\n\n"),
    }),
    threadEvent(19, {
      type: "turn.completed",
      turn: 8,
      usage: { input_tokens: 2, output_tokens: 2 },
      checkpoint_id: null,
    }),
    threadEvent(20, {
      type: "turn.started",
      turn: 9,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(21, {
      type: "user.message",
      turn: 9,
      content: "Run the grouped commands",
      attachments: [],
    }),
    threadEvent(22, {
      type: "tool.requested",
      turn: 9,
      call_id: "call_anchor_1",
      tool: "Bash",
      args: { command: "first" },
      requires_approval: false,
    }),
    threadEvent(23, { type: "tool.started", call_id: "call_anchor_1" }),
    threadEvent(24, {
      type: "tool.completed",
      call_id: "call_anchor_1",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(25, {
      type: "tool.requested",
      turn: 9,
      call_id: "call_anchor_2",
      tool: "Bash",
      args: { command: "second" },
      requires_approval: false,
    }),
    threadEvent(26, { type: "tool.started", call_id: "call_anchor_2" }),
    threadEvent(27, {
      type: "tool.completed",
      call_id: "call_anchor_2",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(28, {
      type: "turn.completed",
      turn: 9,
      usage: { input_tokens: 2, output_tokens: 2 },
      checkpoint_id: null,
    }),
    threadEvent(29, {
      type: "turn.started",
      turn: 10,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(30, {
      type: "user.message",
      turn: 10,
      content: "Keep content after the disclosure",
      attachments: [],
    }),
    threadEvent(31, {
      type: "assistant.message",
      turn: 10,
      content: Array.from(
        { length: 20 },
        (_, index) => `Later response paragraph ${index}: remain below the resized row.`,
      ).join("\n\n"),
    }),
    threadEvent(32, {
      type: "turn.completed",
      turn: 10,
      usage: { input_tokens: 2, output_tokens: 2 },
      checkpoint_id: null,
    }),
  ]);

  const group = page.locator(".activity-group").last();
  await expect(group.getByText("Ran 2 commands", { exact: true })).toBeVisible();
  const seams = await group.locator(":scope > summary").evaluate(async (summary) => {
    const viewport = summary.closest("trouve-thread-screen")
      ?.querySelector<HTMLElement>(".chat-stream");
    const row = summary.closest<HTMLElement>("[data-virtual-id]");
    if (viewport === undefined || viewport === null || row === null) {
      throw new Error("missing virtual disclosure geometry");
    }
    const rowStart = Number.parseFloat(row.style.insetBlockStart || "0");
    viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -12 }));
    viewport.scrollTop = Math.max(0, rowStart + 12);
    viewport.dispatchEvent(new Event("scroll"));
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
    });

    const clickAndMeasureSeams = async (): Promise<readonly number[]> => {
      const liveSummary = row.querySelector<HTMLElement>(".activity-group > summary");
      if (liveSummary === null) throw new Error("missing live disclosure summary");
      liveSummary.click();
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => globalThis.setTimeout(resolve, 0));
      });
      const bounds = [...viewport.querySelectorAll<HTMLElement>("[data-virtual-id]")]
        .map((element) => element.getBoundingClientRect())
        .filter(({ height }) => height > 0)
        .sort((left, right) => left.top - right.top);
      return bounds.slice(1).map(
        ({ top }, index) => top - (bounds[index]?.bottom ?? top),
      );
    };

    return {
      expanded: await clickAndMeasureSeams(),
      collapsed: await clickAndMeasureSeams(),
    };
  });
  for (const seam of [...seams.expanded, ...seams.collapsed]) {
    expect(Math.abs(seam)).toBeLessThanOrEqual(1);
  }
});

test("collapsing the bottom tool keeps the live tail stable", async ({ page }) => {
  await installProtocolFixtures(page);
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(16, {
      type: "turn.started",
      turn: 8,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(17, {
      type: "user.message",
      turn: 8,
      content: "Run a verbose command",
      attachments: [],
    }),
    threadEvent(18, {
      type: "assistant.message",
      turn: 8,
      content: Array.from(
        { length: 48 },
        (_, index) => `Preceding response line ${index}: keep the closed tool below the fold.`,
      ).join("\n\n"),
    }),
    threadEvent(19, {
      type: "tool.requested",
      turn: 8,
      call_id: "call_tail",
      tool: "Bash",
      args: { command: "verbose-command" },
      requires_approval: false,
    }),
    threadEvent(20, { type: "tool.started", call_id: "call_tail" }),
    threadEvent(21, {
      type: "tool.output",
      call_id: "call_tail",
      chunk: Array.from(
        { length: 48 },
        (_, index) => `Verbose output line ${index}: dynamic tail measurement.`,
      ).join("\n"),
    }),
    threadEvent(22, {
      type: "tool.completed",
      call_id: "call_tail",
      status: "ok",
      result: { exit_code: 0 },
    }),
    threadEvent(23, {
      type: "turn.completed",
      turn: 8,
      usage: { input_tokens: 2, output_tokens: 2 },
      checkpoint_id: null,
    }),
  ]);

  const card = page.locator('[data-call-id="call_tail"]');
  const summary = card.locator(":scope > summary");
  await expect(summary).toBeVisible();
  await expect.poll(() => transcriptComposerGap(page)).toBe(8);

  await summary.click();
  await expect(card.getByLabel("Live tool output")).toBeVisible();
  await expect.poll(() => transcriptComposerGap(page)).toBe(8);

  await summary.click();
  await expect(card.getByLabel("Live tool output")).toHaveCount(0);
  await expect.poll(() => transcriptComposerGap(page)).toBe(8);

  const collapsedGeometry = await page.locator(".chat-stream").evaluate((viewport) => ({
    canvasHeight: viewport.querySelector<HTMLElement>(".chat-virtual-canvas")
      ?.getBoundingClientRect().height ?? 0,
    scrollTop: viewport.scrollTop,
  }));
  const expectedParkedTop = await page.locator(".chat-stream").evaluate((viewport) => {
    viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -64 }));
    const target = Math.max(0, viewport.scrollTop - 64);
    viewport.scrollTop = target;
    viewport.dispatchEvent(new Event("scroll"));
    return target;
  });
  await expect(page.getByRole("log", { name: "Conversation" })).toHaveAttribute(
    "aria-live",
    "off",
  );
  await page.evaluate(() => new Promise<void>((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
  }));
  const parkedGeometry = await page.locator(".chat-stream").evaluate((viewport) => ({
    canvasHeight: viewport.querySelector<HTMLElement>(".chat-virtual-canvas")
      ?.getBoundingClientRect().height ?? 0,
    scrollTop: viewport.scrollTop,
  }));
  expect(parkedGeometry.canvasHeight).toBe(collapsedGeometry.canvasHeight);
  expect(Math.abs(parkedGeometry.scrollTop - expectedParkedTop)).toBeLessThanOrEqual(1);
  await page.getByRole("button", { name: "Jump to latest" }).click();
  await expect(page.getByRole("log", { name: "Conversation" })).toHaveAttribute(
    "aria-live",
    "polite",
  );
  await expect.poll(() => transcriptComposerGap(page)).toBe(8);

  const repinnedGeometry = await page.locator(".chat-stream").evaluate((viewport) => ({
    canvasHeight: viewport.querySelector<HTMLElement>(".chat-virtual-canvas")
      ?.getBoundingClientRect().height ?? 0,
    scrollTop: viewport.scrollTop,
  }));
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("Keep the tail still");
  await expect.poll(() => transcriptComposerGap(page)).toBe(8);
  const afterTypingGeometry = await page.locator(".chat-stream").evaluate((viewport) => ({
    canvasHeight: viewport.querySelector<HTMLElement>(".chat-virtual-canvas")
      ?.getBoundingClientRect().height ?? 0,
    scrollTop: viewport.scrollTop,
  }));
  expect(afterTypingGeometry.canvasHeight).toBe(repinnedGeometry.canvasHeight);
  expect(Math.abs(afterTypingGeometry.scrollTop - repinnedGeometry.scrollTop)).toBeLessThanOrEqual(1);
  await expect(page.getByRole("log", { name: "Conversation" })).toHaveAttribute(
    "aria-live",
    "polite",
  );
  await expect(page.getByRole("button", { name: "Jump to latest" })).toHaveCount(0);
});

test("chat surfaces contain pathological content from narrow to wide layouts", async ({
  page,
}, testInfo) => {
  test.skip(
    testInfo.project.name !== "desktop-chromium",
    "One Chromium project owns the multi-viewport pathological-content matrix",
  );
  await installProtocolFixtures(page);
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
  const attachmentInset = await page.locator(".user-body-stream").last().evaluate((body) => {
    const bodyBounds = body.getBoundingClientRect();
    const bodyStyle = getComputedStyle(body);
    const contentLeft = bodyBounds.left
      + Number.parseFloat(bodyStyle.borderLeftWidth)
      + Number.parseFloat(bodyStyle.paddingLeft);
    const attachments = body.querySelector<HTMLElement>(".attachment-list")?.getBoundingClientRect();
    return attachments === undefined ? Number.NaN : attachments.left - contentLeft;
  });
  expect(Math.abs(attachmentInset - 10)).toBeLessThanOrEqual(1);
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
  await installProtocolFixtures(page);
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
        content: turn % 7 === 0
          ? `Virtual response ${turn}\n\n${Array.from(
            { length: 18 },
            (_, line) => `Uneven virtual row ${turn}.${line}: dynamic height measurement coverage.`,
          ).join("\n")}`
          : `Virtual response ${turn}`,
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
  await expect(page.locator(".chat-scroll-indicator")).toHaveAttribute("data-scrollable", "");
  const scrollGeometry = await page.locator(".chat-stream").evaluate((viewport) => {
    const style = getComputedStyle(viewport);
    const webkitScrollbar = getComputedStyle(viewport, "::-webkit-scrollbar");
    const webkitThumb = getComputedStyle(viewport, "::-webkit-scrollbar-thumb");
    const canvas = viewport.querySelector<HTMLElement>(".chat-virtual-canvas");
    const indicator = viewport.parentElement?.querySelector<HTMLElement>(
      ".chat-scroll-indicator",
    );
    if (canvas === null) throw new Error("missing virtual scroll canvas");
    if (indicator === null || indicator === undefined) {
      throw new Error("missing passive scroll indicator");
    }
    const indicatorStyle = getComputedStyle(indicator);
    return {
      canvasHeight: canvas.getBoundingClientRect().height,
      canvasPosition: getComputedStyle(canvas).position,
      clientHeight: viewport.clientHeight,
      indicatorBackground: indicatorStyle.backgroundColor,
      indicatorHeight: indicator.getBoundingClientRect().height,
      indicatorOpacity: indicatorStyle.opacity,
      indicatorPointerEvents: indicatorStyle.pointerEvents,
      indicatorWidth: indicator.getBoundingClientRect().width,
      overflowY: style.overflowY,
      paddingLeft: style.paddingLeft,
      paddingRight: style.paddingRight,
      scrollHeight: viewport.scrollHeight,
      scrollbarGutter: style.scrollbarGutter,
      scrollbarWidth: style.scrollbarWidth,
      webkitScrollbarWidth: webkitScrollbar.width,
      webkitThumbColor: webkitThumb.backgroundColor,
    };
  });
  expect(scrollGeometry.overflowY).toBe("scroll");
  expect(scrollGeometry.paddingLeft).toBe(scrollGeometry.paddingRight);
  expect(scrollGeometry.scrollbarGutter).toBe("stable");
  expect(scrollGeometry.scrollbarWidth).toBe("thin");
  expect(scrollGeometry.webkitScrollbarWidth).toBe("10px");
  expect(scrollGeometry.webkitThumbColor).toBe("rgba(0, 0, 0, 0)");
  expect(scrollGeometry.indicatorBackground).not.toBe("rgba(0, 0, 0, 0)");
  expect(scrollGeometry.indicatorHeight).toBeGreaterThanOrEqual(32);
  expect(scrollGeometry.indicatorOpacity).toBe("1");
  expect(scrollGeometry.indicatorPointerEvents).toBe("none");
  expect(scrollGeometry.indicatorWidth).toBe(6);
  expect(scrollGeometry.canvasPosition).toBe("relative");
  expect(scrollGeometry.canvasHeight).toBeGreaterThan(scrollGeometry.clientHeight);
  expect(scrollGeometry.scrollHeight).toBeGreaterThan(scrollGeometry.clientHeight);

  await expect.poll(() => transcriptComposerGap(page), {
    message: "the transcript tail should use the established 8px composer separation",
  }).toBe(8);

  await page.locator(".chat-stream").evaluate((viewport) => {
    viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -1_000 }));
    viewport.scrollTop = 0;
    viewport.dispatchEvent(new Event("scroll"));
  });
  await expect(page.getByRole("log", { name: "Conversation" })).toHaveAttribute(
    "aria-live",
    "off",
  );
  await expect.poll(() => page.locator(".chat-scroll-indicator").evaluate((indicator) => {
    const shell = indicator.parentElement;
    if (shell === null) return Number.NaN;
    return Math.round(
      indicator.getBoundingClientRect().top - shell.getBoundingClientRect().top,
    );
  })).toBe(3);
  const wheelScrollBounds = await page.locator(".chat-stream").boundingBox();
  if (wheelScrollBounds === null) throw new Error("missing chat scroll bounds");
  await page.mouse.move(
    wheelScrollBounds.x + wheelScrollBounds.width / 2,
    wheelScrollBounds.y + wheelScrollBounds.height / 2,
  );
  await page.mouse.wheel(0, 1_000);
  await expect.poll(() => page.locator(".chat-stream").evaluate((viewport) =>
    viewport.scrollTop
  )).toBeGreaterThan(0);
  await expect.poll(() => page.locator(".chat-scroll-indicator").evaluate((indicator) => {
    const shell = indicator.parentElement;
    if (shell === null) return Number.NaN;
    return indicator.getBoundingClientRect().top - shell.getBoundingClientRect().top;
  })).toBeGreaterThan(3);
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
  await expect.poll(() => transcriptComposerGap(page), {
    message: "jumping to the tail should preserve the established 8px composer separation",
  }).toBe(8);

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

  const viewportBounds = await page.locator(".chat-stream").boundingBox();
  if (viewportBounds === null) throw new Error("missing chat viewport bounds");
  await page.locator(".chat-stream").evaluate((viewport) => {
    const samples: number[] = [viewport.scrollTop];
    const ranges: Array<{ readonly clientHeight: number; readonly scrollHeight: number }> = [{
      clientHeight: viewport.clientHeight,
      scrollHeight: viewport.scrollHeight,
    }];
    const diagnostics = globalThis as typeof globalThis & {
      __trouveScrollRanges?: typeof ranges;
      __trouveScrollSamples?: number[];
    };
    diagnostics.__trouveScrollRanges = ranges;
    diagnostics.__trouveScrollSamples = samples;
    viewport.addEventListener("scroll", () => {
      samples.push(viewport.scrollTop);
      ranges.push({
        clientHeight: viewport.clientHeight,
        scrollHeight: viewport.scrollHeight,
      });
    });
  });
  await page.mouse.move(
    viewportBounds.x + viewportBounds.width / 2,
    viewportBounds.y + viewportBounds.height / 2,
  );
  await page.mouse.wheel(0, -4_000);
  await expect.poll(() => page.evaluate(() => {
    const diagnostics = globalThis as typeof globalThis & {
      __trouveScrollSamples?: number[];
    };
    return diagnostics.__trouveScrollSamples?.length ?? 0;
  })).toBeGreaterThan(1);
  const fastScrollDiagnostics = await page.evaluate(() => {
    const diagnostics = globalThis as typeof globalThis & {
      __trouveScrollRanges?: Array<{
        readonly clientHeight: number;
        readonly scrollHeight: number;
      }>;
      __trouveScrollSamples?: number[];
    };
    return {
      ranges: diagnostics.__trouveScrollRanges ?? [],
      samples: diagnostics.__trouveScrollSamples ?? [],
    };
  });
  const upwardScrollSamples = fastScrollDiagnostics.samples;
  expect(upwardScrollSamples.length).toBeGreaterThan(1);
  expect(
    fastScrollDiagnostics.ranges.every(
      ({ clientHeight, scrollHeight }) => scrollHeight > clientHeight,
    ),
    "the native scrollbar must retain an overflowing scroll range",
  ).toBe(true);
  expect(upwardScrollSamples.at(-1)).toBeLessThan(upwardScrollSamples[0] ?? 0);
  const downwardCorrections = upwardScrollSamples.slice(1).map(
    (sample, index) => sample - (upwardScrollSamples[index] ?? sample),
  ).filter((delta) => delta > 1);
  expect(
    downwardCorrections,
    `fast upward scrolling reversed direction: ${upwardScrollSamples.join(", ")}`,
  ).toEqual([]);

  await page.locator(".chat-stream").evaluate((viewport) => {
    const jump = viewport.querySelector<HTMLElement>(".follow-tail");
    if (jump === null) throw new Error("missing jump-to-latest control");
    viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: 1_000 }));
    const transcriptTail = Math.max(
      0,
      viewport.scrollHeight
        - viewport.clientHeight
        - jump.getBoundingClientRect().height,
    );
    // One wheel tick can produce several native scroll events. The first one
    // must not consume the gesture intent before the final event reaches the
    // transcript tail.
    viewport.scrollTop = Math.max(0, transcriptTail - 100);
    viewport.dispatchEvent(new Event("scroll"));
    // WebKit includes the sticky control in scrollHeight even though it is
    // visually overlaid. Stop at the transcript tail, not after the control.
    viewport.scrollTop = transcriptTail;
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

test("prefetches older history before the reader reaches the loaded boundary", async ({ page }) => {
  const olderPageBoundaries = [150, 120, 90, 60, 30] as const;
  const historyPage = (start: number, end: number, hasOlder: boolean) => ({
    item_offset: start,
    total_items: 240,
    has_older: hasOlder,
    items: Array.from({ length: end - start }, (_, index) => ({
      kind: "user",
      turn: 1_000 + start + index,
      content: `Buffered prompt ${start + index}${
        start === 0
          ? ` ${"with deliberately variable-height history content ".repeat(12 + index % 5 * 4)}`
          : ""
      }`,
      attachments: [],
    })),
  });
  let olderRequests = 0;
  let olderResponses = 0;
  const olderBoundaries: number[] = [];
  const completedBoundaries: number[] = [];
  let releaseWarmPage: (() => void) | undefined;
  const warmPageReleased = new Promise<void>((resolve) => {
    releaseWarmPage = resolve;
  });
  let releaseSecondPage: (() => void) | undefined;
  const secondPageReleased = new Promise<void>((resolve) => {
    releaseSecondPage = resolve;
  });
  await installProtocolFixtures(page, { threadViewFixture: async (before) => {
    if (before === undefined) {
      return { snapshot: historyPage(150, 240, true) };
    }
    olderRequests += 1;
    olderBoundaries.push(before);
    if (before === 150) {
      await warmPageReleased;
    } else if (before === 120) {
      await secondPageReleased;
    }
    olderResponses += 1;
    completedBoundaries.push(before);
    const pageIndex = olderPageBoundaries.indexOf(
      before as typeof olderPageBoundaries[number],
    );
    if (pageIndex < 0) throw new Error(`unexpected history boundary ${before}`);
    const start = olderPageBoundaries[pageIndex + 1] ?? 0;
    return {
      snapshot: historyPage(start, before, pageIndex + 1 < olderPageBoundaries.length),
    };
  } });
  await page.goto("/");
  await replayHistory(page);

  await expect.poll(() => olderRequests).toBe(1);
  const heightBeforeWarmPage = await page.locator(".chat-stream").evaluate(
    (viewport) => viewport.scrollHeight,
  );
  releaseWarmPage?.();
  await expect.poll(() => olderResponses).toBe(1);
  await expect.poll(() => page.locator(".chat-stream").evaluate(
    (viewport) => viewport.scrollHeight,
  )).toBeGreaterThan(heightBeforeWarmPage);
  expect(olderRequests, "opening a thread should warm only one bounded page").toBe(1);
  await expect(page.getByText("Loading earlier messages…", { exact: true })).toHaveCount(0);

  await page.locator(".chat-stream").evaluate((viewport) => {
    viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -1_000 }));
    viewport.scrollTop = Math.min(
      viewport.scrollHeight - viewport.clientHeight,
      viewport.clientHeight * 4,
    );
    viewport.dispatchEvent(new Event("scroll"));
  });
  await expect.poll(() => olderRequests).toBe(2);
  await expect(page.getByText("Loading earlier messages…", { exact: true })).toHaveCount(0);
  const anchor = await page.locator(".chat-stream").evaluate(async (viewport) => {
    // Keep moving after the fetch starts. The response must preserve the
    // reader's latest position, not the position that triggered prefetch.
    viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -600 }));
    viewport.scrollTop = Math.max(0, viewport.scrollTop - viewport.clientHeight * 0.75);
    viewport.dispatchEvent(new Event("scroll"));
    await new Promise<void>((resolve) => requestAnimationFrame(() =>
      requestAnimationFrame(() => resolve())
    ));
    const viewportTop = viewport.getBoundingClientRect().top;
    const row = [...viewport.querySelectorAll<HTMLElement>("[data-virtual-id]")]
      .find((candidate) => candidate.getBoundingClientRect().bottom > viewportTop);
    if (row === undefined || row.dataset["virtualId"] === undefined) {
      throw new Error("missing visible history anchor");
    }
    return {
      id: row.dataset["virtualId"],
      offset: row.getBoundingClientRect().top - viewportTop,
    };
  });
  await page.locator(".chat-stream").evaluate((viewport, expected) => {
    type AnchorProbe = {
      active: boolean;
      maxDeviation: number;
      missingFrames: number;
    };
    const target = viewport as HTMLElement & { historyAnchorProbe?: AnchorProbe };
    const probe: AnchorProbe = { active: true, maxDeviation: 0, missingFrames: 0 };
    target.historyAnchorProbe = probe;
    const sample = (): void => {
      if (!probe.active) return;
      const row = [...viewport.querySelectorAll<HTMLElement>("[data-virtual-id]")]
        .find((candidate) => candidate.dataset["virtualId"] === expected.id);
      if (row === undefined) {
        probe.missingFrames += 1;
      } else {
        probe.maxDeviation = Math.max(
          probe.maxDeviation,
          Math.abs(
            row.getBoundingClientRect().top
              - viewport.getBoundingClientRect().top
              - expected.offset,
          ),
        );
      }
      requestAnimationFrame(sample);
    };
    requestAnimationFrame(sample);
  }, anchor);
  const heightBeforeSecondPage = await page.locator(".chat-stream").evaluate(
    (viewport) => viewport.scrollHeight,
  );
  releaseSecondPage?.();
  await expect.poll(() => olderResponses).toBe(2);
  await expect.poll(() => page.locator(".chat-stream").evaluate(
    (viewport) => viewport.scrollHeight,
  )).toBeGreaterThan(heightBeforeSecondPage);
  await expect.poll(() => page.locator(".chat-stream").evaluate((viewport, expected) => {
    const row = [...viewport.querySelectorAll<HTMLElement>("[data-virtual-id]")]
      .find((candidate) => candidate.dataset["virtualId"] === expected.id);
    if (row === undefined) return Number.POSITIVE_INFINITY;
    return Math.abs(
      row.getBoundingClientRect().top
        - viewport.getBoundingClientRect().top
        - expected.offset,
    );
  }, anchor)).toBeLessThanOrEqual(2);
  await page.waitForTimeout(100);
  const anchorProbe = await page.locator(".chat-stream").evaluate((viewport) => {
    type AnchorProbe = {
      active: boolean;
      maxDeviation: number;
      missingFrames: number;
    };
    const target = viewport as HTMLElement & { historyAnchorProbe?: AnchorProbe };
    if (target.historyAnchorProbe === undefined) {
      throw new Error("missing history anchor probe");
    }
    target.historyAnchorProbe.active = false;
    return target.historyAnchorProbe;
  });
  expect(anchorProbe.missingFrames).toBe(0);
  expect(anchorProbe.maxDeviation).toBeLessThanOrEqual(2);

  const heightBeforeThirdPage = await page.locator(".chat-stream").evaluate(
    (viewport) => viewport.scrollHeight,
  );
  await page.locator(".chat-stream").evaluate((viewport) => {
    viewport.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -1_000 }));
    viewport.scrollTop = 0;
    viewport.dispatchEvent(new Event("scroll"));
  });
  await expect.poll(() => olderResponses).toBeGreaterThanOrEqual(3);
  await expect(page.getByText("Loading earlier messages…", { exact: true })).toHaveCount(0);
  // The route fixture records its response before the browser has consumed
  // and rendered it. Wait for the newly prepended page itself instead of a
  // fixed number of animation frames, which can still race a loaded CI host.
  await expect.poll(() => page.locator(".chat-stream").evaluate(
    (viewport) => viewport.scrollHeight,
  )).toBeGreaterThan(heightBeforeThirdPage);
  // This is one deliberate reader gesture. A polling callback that emits a
  // wheel event can race a fast response and accidentally request every
  // remaining page, testing the poller rather than the prefetch boundary.
  const chatBounds = await page.locator(".chat-stream").boundingBox();
  if (chatBounds === null) throw new Error("missing chat scroll bounds");
  await page.mouse.move(
    chatBounds.x + chatBounds.width / 2,
    chatBounds.y + chatBounds.height / 2,
  );
  await page.mouse.wheel(0, -1_000);
  await expect.poll(() => new Set(olderBoundaries).has(60)).toBe(true);
  await expect.poll(() => completedBoundaries.includes(60)).toBe(true);
  const requestedBoundaries = [...new Set(olderBoundaries)];
  expect(requestedBoundaries).toContain(120);
  expect(olderBoundaries).toContain(60);
  expect(olderBoundaries.filter((boundary) => boundary === 60)).toHaveLength(1);
  // More older pages remain available than the reader requested. Re-fetching
  // a boundary after a route refresh is harmless; automatically walking every
  // distinct page from one scroll gesture is not.
  expect(requestedBoundaries.length).toBeLessThan(olderPageBoundaries.length);
});

test("keeps a nested thought anchored when history extends the same agent turn", async ({
  page,
}) => {
  const historyPage = (start: number, end: number, hasOlder: boolean) => ({
    item_offset: start,
    total_items: 90,
    has_older: hasOlder,
    items: Array.from({ length: end - start }, (_, index) => ({
      kind: "thinking",
      turn: 900,
      content: `Stable thought ${start + index}\n\n${
        "Variable-height thought content remains anchored while earlier history arrives. ".repeat(
          3 + index % 4,
        )
      }`,
      complete: true,
    })),
  });
  let oldestRequests = 0;
  let oldestResponses = 0;
  let releaseOldestPage: (() => void) | undefined;
  const oldestPageReleased = new Promise<void>((resolve) => {
    releaseOldestPage = resolve;
  });
  await installProtocolFixtures(page, { threadViewFixture: async (before) => {
    if (before === undefined) return { snapshot: historyPage(30, 90, true) };
    if (before === 30) {
      oldestRequests += 1;
      await oldestPageReleased;
      oldestResponses += 1;
      return { snapshot: historyPage(0, 30, false) };
    }
    throw new Error(`unexpected long-turn history boundary ${before}`);
  } });
  await page.goto("/");
  await replayHistory(page);
  await expect(page.getByText("Stable thought 30", { exact: true })).toBeVisible();
  await expect.poll(() => oldestRequests).toBeGreaterThanOrEqual(1);

  const viewport = page.locator(".chat-stream");
  await viewport.evaluate((element) => {
    element.dispatchEvent(new WheelEvent("wheel", { bubbles: true, deltaY: -1_000 }));
    element.scrollTop = Math.min(element.scrollHeight - element.clientHeight, 400);
    element.dispatchEvent(new Event("scroll"));
  });
  const anchor = await viewport.evaluate(async (element) => {
    await new Promise<void>((resolve) => requestAnimationFrame(() =>
      requestAnimationFrame(() => resolve())
    ));
    const viewportTop = element.getBoundingClientRect().top;
    const candidate = [...element.querySelectorAll<HTMLElement>("[data-chat-anchor-id]")]
      .filter((row) => row.dataset["chatAnchorId"]?.startsWith("item:snapshot:") === true)
      .find((row) => row.getBoundingClientRect().bottom > viewportTop);
    const id = candidate?.dataset["chatAnchorId"];
    if (candidate === undefined || id === undefined) {
      throw new Error("missing nested thought anchor");
    }
    return {
      id,
      offset: candidate.getBoundingClientRect().top - viewportTop,
    };
  });
  releaseOldestPage?.();
  await expect.poll(() => oldestResponses).toBeGreaterThanOrEqual(1);
  await expect.poll(() => viewport.evaluate((element, expected) => {
    const row = [...element.querySelectorAll<HTMLElement>("[data-chat-anchor-id]")]
      .find((candidate) => candidate.dataset["chatAnchorId"] === expected.id);
    return row === undefined
      ? Number.POSITIVE_INFINITY
      : Math.abs(
          row.getBoundingClientRect().top
            - element.getBoundingClientRect().top
            - expected.offset,
        );
  }, anchor)).toBeLessThanOrEqual(2);
  // Let the 500 ms CHAT_HISTORY_ANCHOR_SETTLE_MS correction window expire,
  // then prove the released off-screen page cannot move the preserved anchor.
  await page.waitForTimeout(600);
  expect(await viewport.evaluate((element, expected) => {
    const row = [...element.querySelectorAll<HTMLElement>("[data-chat-anchor-id]")]
      .find((candidate) => candidate.dataset["chatAnchorId"] === expected.id);
    return row === undefined
      ? Number.POSITIVE_INFINITY
      : Math.abs(
          row.getBoundingClientRect().top
            - element.getBoundingClientRect().top
            - expected.offset,
        );
  }, anchor)).toBeLessThanOrEqual(2);
});

test("a queued prompt can interrupt the active turn and run next", async ({ page }) => {
  const dispatchedQueuePromptIds: string[] = [];
  await installProtocolFixtures(page, { dispatchedQueuePromptIds });
  await page.goto("/");
  await replayHistory(page);
  await emitBatch(page, [
    threadEvent(16, {
      type: "turn.started",
      turn: 8,
      mode: "code",
      model: "test/model",
    }),
    threadEvent(17, {
      type: "assistant.delta",
      turn: 8,
      text: "Still working",
    }),
    threadEvent(18, {
      type: "thread.queue_updated",
      prompts: [{
        id: "qp_1",
        thread_id: "th_fixture",
        content: "Run this one immediately",
        position: 0,
        created_at: "2026-08-04T08:00:19Z",
        attachments: [],
      }],
    }),
  ]);

  const sendNow = page.getByRole("button", {
    name: "Send this queued prompt now and stop the current turn",
  });
  await expect(sendNow).toBeEnabled();
  await sendNow.click();

  await expect.poll(() => dispatchedQueuePromptIds).toEqual(["qp_1"]);
  await expect(page.locator("wa-button.composer-submit")).toHaveText("Stopping…");
});

test("queued prompts drag by the full row and adjacent drops always move", async ({ page }) => {
  const prompts = [
    {
      id: "qp_first",
      thread_id: "th_fixture",
      content: "First queued prompt",
      position: 0,
      created_at: "2026-08-04T08:00:19Z",
      attachments: [],
    },
    {
      id: "qp_second",
      thread_id: "th_fixture",
      content: "Second queued prompt",
      position: 1,
      created_at: "2026-08-04T08:00:20Z",
      attachments: [],
    },
  ];
  const submittedOrders: string[][] = [];
  await installProtocolFixtures(page);
  await page.route("**/v1/threads/th_fixture/queue", async (route) => {
    if (route.request().method() !== "PUT") {
      await route.fallback();
      return;
    }
    const { ids } = route.request().postDataJSON() as { readonly ids: string[] };
    submittedOrders.push(ids);
    await route.fulfill({
      json: ids.map((id, position) => ({
        ...prompts.find((prompt) => prompt.id === id)!,
        position,
      })),
    });
  });
  await page.goto("/");
  await replayHistory(page);
  await emit(page, threadEvent(16, {
    type: "thread.queue_updated",
    prompts,
  }));

  const rows = page.locator(".queue-panel li[data-queue-id]");
  await expect(rows).toHaveCount(2);
  await expect(rows.first()).toHaveAttribute("draggable", "true");
  await expect(page.locator(".queue-grip")).toHaveCount(0);

  const source = rows.filter({ hasText: "Second queued prompt" });
  const target = rows.filter({ hasText: "First queued prompt" });
  const dragProbeBounds = await target.boundingBox();
  if (dragProbeBounds === null) throw new Error("missing queued prompt row geometry");
  const dataTransfer = await page.evaluateHandle(() => new DataTransfer());
  await source.dispatchEvent("pointerdown");
  await source.dispatchEvent("dragstart", { dataTransfer });
  await target.dispatchEvent("dragover", {
    clientX: dragProbeBounds.x + Math.min(120, dragProbeBounds.width - 2),
    clientY: dragProbeBounds.y + dragProbeBounds.height - 2,
    dataTransfer,
  });

  const placeholder = page.locator('[data-drop-placeholder="queue"]');
  await expect(placeholder).toBeVisible();
  const placeholderStyle = await placeholder.evaluate((element) => {
    const style = getComputedStyle(element);
    return {
      borderStyle: style.borderTopStyle,
      borderWidth: style.borderTopWidth,
      height: element.getBoundingClientRect().height,
    };
  });
  expect(placeholderStyle.borderStyle).toBe("dashed");
  expect(Number.parseFloat(placeholderStyle.borderWidth)).toBeGreaterThanOrEqual(1);
  expect(placeholderStyle.height).toBeGreaterThanOrEqual(24);
  await source.dispatchEvent("dragend", { dataTransfer });
  await dataTransfer.dispose();
  await expect(placeholder).toHaveCount(0);

  const targetBounds = await target.boundingBox();
  if (targetBounds === null) throw new Error("missing queued prompt row geometry");
  await source.locator("p").dragTo(target, {
    targetPosition: {
      x: Math.min(120, targetBounds.width - 2),
      y: targetBounds.height - 2,
    },
  });

  await expect.poll(() => submittedOrders).toEqual([["qp_second", "qp_first"]]);
  await expect(rows.nth(0)).toContainText("Second queued prompt");
  await expect(rows.nth(1)).toContainText("First queued prompt");
});

test("queued prompts reorder from the keyboard without visible move arrows", async ({ page }) => {
  const prompts = [
    {
      id: "qp_first",
      thread_id: "th_fixture",
      content: "First queued prompt",
      position: 0,
      created_at: "2026-08-04T08:00:19Z",
      attachments: [],
    },
    {
      id: "qp_second",
      thread_id: "th_fixture",
      content: "Second queued prompt",
      position: 1,
      created_at: "2026-08-04T08:00:20Z",
      attachments: [],
    },
  ];
  const submittedOrders: string[][] = [];
  await installProtocolFixtures(page);
  await page.route("**/v1/threads/th_fixture/queue", async (route) => {
    if (route.request().method() !== "PUT") {
      await route.fallback();
      return;
    }
    const { ids } = route.request().postDataJSON() as { readonly ids: string[] };
    submittedOrders.push(ids);
    await route.fulfill({
      json: ids.map((id, position) => ({
        ...prompts.find((prompt) => prompt.id === id)!,
        position,
      })),
    });
  });
  await page.goto("/");
  await replayHistory(page);
  await emit(page, threadEvent(16, {
    type: "thread.queue_updated",
    prompts,
  }));

  const rows = page.locator(".queue-panel li[data-queue-id]");
  const first = page.locator('[data-queue-id="qp_first"]');
  const status = page.locator(".queue-panel > p[role=status]");
  await expect(page.locator('[data-queue-action="earlier"]')).toHaveCount(0);
  await expect(page.locator('[data-queue-action="later"]')).toHaveCount(0);
  await expect(first).toHaveAttribute("tabindex", "0");
  await expect(first).toHaveAttribute(
    "aria-keyshortcuts",
    "Space Enter ArrowUp ArrowDown Home End Escape",
  );

  await first.focus();
  await first.press("Space");
  await expect(first).toHaveAttribute("data-keyboard-reordering", "true");
  await expect(first.locator(".queue-reorder-badge")).toHaveText("Reordering");
  await expect(status).toContainText("Picked up queued prompt 1 of 2.");
  await first.press("End");
  await expect(rows.nth(1)).toHaveAttribute("data-queue-id", "qp_first");
  await expect(status).toContainText("Queued prompt moved to position 2 of 2.");
  await first.press("Escape");
  await expect(rows.nth(0)).toHaveAttribute("data-queue-id", "qp_first");
  await expect(first).not.toHaveAttribute("data-keyboard-reordering", "true");
  expect(submittedOrders).toEqual([]);

  await first.press("Space");
  await first.press("End");
  await first.press("Space");
  await expect.poll(() => submittedOrders).toEqual([["qp_second", "qp_first"]]);
  await expect(rows.nth(1)).toHaveAttribute("data-queue-id", "qp_first");
  await expect(first).toBeFocused();
  await expect(status).toContainText("Queued prompt dropped at position 2 of 2.");
});

test("turn controls cover start, queue, cancel, and send-after-cancel races", async ({ page }) => {
  const sentMessages: Array<Record<string, unknown>> = [];
  const messageReleases: Array<() => void> = [];
  await installProtocolFixtures(page, {
    sentMessages,
    beforeMessageResponse: () => new Promise<void>((resolve) => {
      messageReleases.push(resolve);
    }),
  });
  await page.goto("/");
  await replayHistory(page);
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  const submit = page.locator("wa-button.composer-submit");

  await composer.fill("Start another turn");
  await expect(submit).toHaveText("Send");
  await submit.click();
  await expect(submit).toHaveText("Sending…");
  expect(messageReleases).toHaveLength(1);
  messageReleases.shift()?.();
  await expect(submit).toHaveText("Starting…");
  await expect(page.locator('[data-virtual-id="ephemeral:activity"] .agent-activity'))
    .toContainText("Starting turn…");

  await composer.fill("Queue this while startup is pending");
  await expect(submit).toHaveText("Queue");
  await submit.click();
  await expect(submit).toHaveText("Queueing…");
  expect(messageReleases).toHaveLength(1);
  messageReleases.shift()?.();
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
  const queuedAttachment = page.locator(".queue-attachment-badge");
  await expect(queuedAttachment).toContainText("1");
  await expect(queuedAttachment.locator('[data-font-awesome-icon="paperclip"]')).toBeVisible();
  await expect(queuedAttachment).toHaveAttribute(
    "aria-label",
    "1 attachment",
  );
  await composer.fill("Keep this local draft");
  await page.getByRole("button", { name: "Edit queued prompt" }).click();
  await expect(page.locator(".queue-edit-indicator")).toContainText("Editing queued prompt");
  await expect(submit).toHaveText("Update");
  await expect(composer).toHaveValue("Queue this while startup is pending");
  await expect(page.locator('.pending-attachments img[alt="Preview of layout.png"]')).toBeVisible();
  await page.getByRole("button", { name: "Remove layout.png" }).click();
  await page.locator("form.composer .attachment-button input").setInputFiles({
    name: "notes.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("updated queue attachment"),
  });
  await expect(page.locator(".pending-attachments")).toContainText("notes.txt");
  await composer.fill("Queue this after editing");
  const updateRequest = page.waitForRequest((request) =>
    request.method() === "PATCH" && request.url().endsWith("/v1/queue/qp_1")
  );
  await submit.click();
  const updateBody = (await updateRequest).postDataJSON() as Record<string, unknown>;
  expect(updateBody).toMatchObject({
    content: "Queue this after editing",
    retained_attachment_ids: [],
  });
  expect(updateBody["attachments"]).toMatchObject([{
    name: "notes.txt",
    mime: "text/plain",
  }]);
  await expect(page.locator(".queue-edit-indicator")).toHaveCount(0);
  await expect(composer).toHaveValue("Keep this local draft");
  await expect(submit).toHaveText("Queue");
  await page.getByRole("button", { name: "Edit queued prompt" }).click();
  await expect(composer).toHaveValue("Queue this after editing");
  await expect(page.locator(".pending-attachments")).toContainText("notes.txt");
  await page.getByRole("button", { name: "Cancel queued prompt edit" }).click();
  await expect(page.locator(".queue-edit-indicator")).toHaveCount(0);
  await expect(composer).toHaveValue("Keep this local draft");
  await composer.fill("");
  await expect(submit).toHaveText("Cancel");
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
  expect(messageReleases).toHaveLength(1);
  messageReleases.shift()?.();
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

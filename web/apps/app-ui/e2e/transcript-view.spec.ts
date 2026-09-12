import { createRequire } from "node:module";

import { expect, test, type Page } from "@playwright/test";

// The protocol client and transcript view live in linked workspace packages,
// which Vite serves through its filesystem namespace rather than under /src.
const packageModule = (specifier: string): string =>
  `/@fs${createRequire(import.meta.url).resolve(specifier)}`;
const protocolClientModule = packageModule("@trouve-ai/protocol/client");
const transcriptViewModule = packageModule("@trouve-ai/transcript/transcript-view");

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
    thinking_level: "max",
  }),
  threadEvent(2, {
    type: "user.message",
    turn: 7,
    content: "Review the **migration**",
    attachments: [],
  }),
  threadEvent(3, { type: "assistant.thinking", turn: 7, text: "Compare both frontends" }),
  threadEvent(4, { type: "assistant.progress", turn: 7, text: "Reading the diff first." }),
  threadEvent(5, {
    type: "tool.requested",
    turn: 7,
    call_id: "call_read",
    tool: "read_file",
    args: { path: "src/app.ts", offset: 8, limit: 1 },
    requires_approval: false,
  }),
  threadEvent(6, { type: "tool.started", call_id: "call_read" }),
  threadEvent(7, {
    type: "tool.completed",
    call_id: "call_read",
    status: "ok",
    result: { content: "const ready = true;" },
  }),
  threadEvent(8, { type: "assistant.delta", turn: 7, text: "Looks good." }),
  threadEvent(9, { type: "assistant.message", turn: 7, content: "Looks good." }),
  threadEvent(10, {
    type: "turn.completed",
    turn: 7,
    usage: { input_tokens: 20, output_tokens: 8, cost_usd: 0.002 },
    checkpoint_id: "cp_turn_7",
  }),
];

const installEventStream = async (
  page: Page,
  seed: readonly FixtureEvent[] = history,
): Promise<void> => {
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
  }, seed);
};

const olderPage = {
  item_offset: 0,
  total_items: 3,
  has_older: false,
  items: [
    {
      kind: "user",
      turn: 6,
      content: "Earlier prompt",
      attachments: [],
      background: false,
    },
    {
      kind: "assistant",
      turn: 6,
      content: "Earlier answer",
      complete: true,
    },
    {
      kind: "turn_status",
      turn: 6,
      state: { state: "completed", usage: { input_tokens: 12, output_tokens: 4 } },
    },
  ],
};

const installProtocolFixtures = async (
  page: Page,
  options: { readonly hasOlder?: boolean } = {},
): Promise<{ readonly viewRequests: string[] }> => {
  const viewRequests: string[] = [];
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const key = `${request.method()} ${url.pathname}`;
    if (key === "GET /v1/threads/th_fixture/view") {
      const before = url.searchParams.get("before");
      viewRequests.push(before ?? "tail");
      const older = before !== null;
      await route.fulfill({
        headers: { "x-trouve-event-cursor": "0" },
        json: older
          ? olderPage
          : {
              item_offset: options.hasOlder === true ? 3 : 0,
              total_items: options.hasOlder === true ? 3 : 0,
              has_older: options.hasOlder === true,
              items: [],
            },
      });
      return;
    }
    await route.fulfill({ status: 404, json: { code: "not_found", message: key } });
  });
  return { viewRequests };
};

const mountTranscript = async (page: Page): Promise<void> => {
  await page.goto("/gallery.html");
  await page.locator("trouve-component-gallery").waitFor();
  await page.evaluate(async ({ clientModule, viewModule }) => {
    const [{ ProtocolClient }] = await Promise.all([
      import(clientModule) as Promise<typeof import("@trouve-ai/protocol/client")>,
      import(viewModule),
    ]);
    document.body.replaceChildren();
    const host = document.createElement("div");
    host.style.cssText = "display:grid;height:600px;width:900px;";
    const view = document.createElement("trouve-transcript-view");
    view.setAttribute("thread-id", "th_fixture");
    Object.assign(view, {
      client: new ProtocolClient(globalThis.location.origin),
      models: [{ id: "test/model", label: "Test model", context_window: 200_000 }],
    });
    host.append(view);
    document.body.append(host);
  }, { clientModule: protocolClientModule, viewModule: transcriptViewModule });
};

test("renders a thread transcript from the snapshot and live event stream", async ({ page }) => {
  await installEventStream(page);
  await installProtocolFixtures(page);
  await mountTranscript(page);

  const view = page.locator("trouve-transcript-view");
  const turnCard = view.locator(".turn-card");
  await expect(turnCard).toHaveCount(1);
  await expect(turnCard.locator("#turn-heading-turn\\:7")).toHaveText("Turn 7");
  await expect(turnCard.locator(".agent-model-label")).toContainText("test/model");
  await expect(turnCard.locator(".turn-prompt-node trouve-markdown-view")).toContainText("Review the");
  await expect(turnCard.locator(".thinking-output:not(.progress-output)")).toContainText("Reasoning");
  await expect(turnCard.locator(".progress-output")).toContainText("Progress");
  await expect(turnCard.locator(".progress-output")).toContainText("Reading the diff first.");
  const tool = turnCard.locator(".tool-card[data-call-id=call_read]");
  await expect(tool).toHaveCount(1);
  await expect(tool.locator("summary")).toContainText("Read:");
  await expect(tool.locator("summary")).toContainText("app.ts");
  await expect(tool.locator("summary")).toContainText("Completed");
  await expect(turnCard.locator(".agent-text-block.complete")).toContainText("Looks good.");
  await expect(turnCard.locator(".tool-approval-actions")).toHaveCount(0);
  await expect(view.locator(".turn-rule-actions")).toHaveCount(0);

  await tool.locator("summary").click();
  await expect(tool).toHaveJSProperty("open", true);
  await expect(tool.locator("trouve-tool-detail-view")).toHaveCount(1);

  await page.evaluate(() => {
    const emit = (globalThis as { __emitThreadEvent?: (event: unknown) => void }).__emitThreadEvent;
    emit?.({
      cursor: 11,
      scope: { thread: "th_fixture" },
      ts: "2026-08-04T08:00:11Z",
      type: "turn.started",
      turn: 8,
      mode: "code",
      model: "test/model",
    });
    emit?.({
      cursor: 12,
      scope: { thread: "th_fixture" },
      ts: "2026-08-04T08:00:12Z",
      type: "turn.admitted",
      turn: 8,
      provider_wait_ms: 0,
    });
    emit?.({
      cursor: 13,
      scope: { thread: "th_fixture" },
      ts: "2026-08-04T08:00:13Z",
      type: "user.message",
      turn: 8,
      content: "Second prompt",
      attachments: [],
    });
    emit?.({
      cursor: 14,
      scope: { thread: "th_fixture" },
      ts: "2026-08-04T08:00:14Z",
      type: "assistant.delta",
      turn: 8,
      text: "Streaming reply",
    });
  });
  await expect(view.locator(".turn-card")).toHaveCount(2);
  const secondTurn = view.locator(".turn-card").nth(1);
  await expect(secondTurn).toHaveClass(/turn-running/u);
  await expect(secondTurn.locator(".agent-text-block")).toContainText("Streaming reply");
  await expect(view.locator(".chat-stream")).toHaveAttribute("aria-busy", "true");

  // The log is deliberately not a live region: streamed Markdown would be
  // announced piecemeal (or all at once when `aria-busy` clears). The
  // running activity has its own status region, and completion is published
  // once through a dedicated one.
  const stream = view.locator(".chat-stream");
  await expect(stream).toHaveAttribute("aria-live", "off");
  expect(await stream.getAttribute("aria-relevant")).toBeNull();
  const announcement = view.locator(".transcript-announcement");
  await expect(announcement).toHaveAttribute("role", "status");
  await expect(announcement).toHaveText("");
  await page.evaluate(() => {
    const emit = (globalThis as { __emitThreadEvent?: (event: unknown) => void }).__emitThreadEvent;
    for (const [cursor, text] of [[15, " continues"], [16, " and continues."]] as const) {
      emit?.({
        cursor,
        scope: { thread: "th_fixture" },
        ts: `2026-08-04T08:00:${cursor}Z`,
        type: "assistant.delta",
        turn: 8,
        text,
      });
    }
  });
  await expect(secondTurn.locator(".agent-text-block")).toContainText("Streaming reply continues and continues.");
  await expect(announcement).toHaveText("");
  await page.evaluate(() => {
    const emit = (globalThis as { __emitThreadEvent?: (event: unknown) => void }).__emitThreadEvent;
    emit?.({
      cursor: 17,
      scope: { thread: "th_fixture" },
      ts: "2026-08-04T08:00:17Z",
      type: "assistant.message",
      turn: 8,
      content: "Streaming reply continues and continues.",
    });
    emit?.({
      cursor: 18,
      scope: { thread: "th_fixture" },
      ts: "2026-08-04T08:00:18Z",
      type: "turn.completed",
      turn: 8,
      usage: { input_tokens: 2, output_tokens: 1 },
    });
  });
  await expect(secondTurn).toHaveClass(/turn-completed/u);
  await expect(announcement).toHaveText("Turn 8 complete");
  await expect(stream).toHaveAttribute("aria-busy", "false");
});

test("pages older history when the reader scrolls to the top", async ({ page }) => {
  await installEventStream(page);
  const { viewRequests } = await installProtocolFixtures(page, { hasOlder: true });
  await mountTranscript(page);

  const view = page.locator("trouve-transcript-view");
  await expect(view.locator(".turn-card")).toHaveCount(2);
  await expect(view.locator("#turn-heading-turn\\:6")).toHaveText("Turn 6");
  await expect(view.locator(".turn-card").first().locator(".agent-text-block")).toContainText(
    "Earlier answer",
  );
  expect(viewRequests).toEqual(["tail", "3"]);
  await expect(view.locator(".chat-history-sentinel")).toHaveCount(0);
});

test("loads deferred details for an approval-gated tool without a disclosure click", async ({ page }) => {
  // Persisted tool rows carry only compact arguments (`details_deferred`).
  // A tool awaiting approval is forced open and its summary cannot be
  // toggled, so the transcript must request the details itself.
  await installEventStream(page, []);
  const detailRequests: string[] = [];
  await page.route("**/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const key = `${request.method()} ${url.pathname}`;
    if (key === "GET /v1/threads/th_fixture/view") {
      await route.fulfill({
        headers: { "x-trouve-event-cursor": "0" },
        json: {
          item_offset: 0,
          total_items: 3,
          has_older: false,
          items: [
            {
              kind: "user",
              turn: 9,
              content: "Clear the build cache",
              attachments: [],
              background: false,
            },
            {
              kind: "tool_call",
              call_id: "call_rm",
              tool: "shell",
              args: { command: "rm -rf …" },
              details_deferred: true,
              status: "awaiting_approval",
            },
            { kind: "turn_status", turn: 9, state: { state: "running" } },
          ],
        },
      });
      return;
    }
    if (key === "GET /v1/threads/th_fixture/tools/call_rm") {
      detailRequests.push(key);
      await route.fulfill({
        json: { call_id: "call_rm", args: { command: "rm -rf target/debug/incremental" } },
      });
      return;
    }
    await route.fulfill({ status: 404, json: { code: "not_found", message: key } });
  });
  await mountTranscript(page);

  const tool = page.locator("trouve-transcript-view .tool-card[data-call-id=call_rm]");
  await expect(tool).toHaveCount(1);
  await expect(tool).toHaveJSProperty("open", true);
  await expect(tool.locator("trouve-tool-detail-view")).toHaveCount(1);
  await expect(tool.locator(".tool-detail-loading")).toHaveCount(0);
  await expect(tool.locator("trouve-tool-detail-view")).toContainText("target/debug/incremental");
  expect(detailRequests).toEqual(["GET /v1/threads/th_fixture/tools/call_rm"]);
});

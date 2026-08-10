import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const read = (path: string): string =>
  readFileSync(new URL(path, import.meta.url), "utf8");

const numberFrom = (source: string, expression: RegExp, label: string): number => {
  const value = expression.exec(source)?.[1];
  if (value === undefined) throw new Error(`missing visual contract: ${label}`);
  return Number(value);
};

const relativeLuminance = (hex: string): number => {
  const channels = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  if (channels === null) throw new Error(`expected a six-digit hex color, received ${hex}`);
  const linear = channels.slice(1).map((channel) => {
    const value = Number.parseInt(channel!, 16) / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * linear[0]! + 0.7152 * linear[1]! + 0.0722 * linear[2]!;
};

const contrastRatio = (foreground: string, background: string): number => {
  const first = relativeLuminance(foreground);
  const second = relativeLuminance(background);
  return (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05);
};

describe("Trouve visual contract", () => {
  const desktopHost = read("../../../../crates/trouve-app/src/web_preview.rs");
  const tokens = read("./tokens.css");
  const themes = read("./themes.css");
  const app = read("./app.css");
  const shell = read("../app/trouve-app.ts");
  const sessionList = read("../components/session-list.ts");
  const sessionIndicators = read("../state/session-indicator-model.ts");
  const icons = read("../components/font-awesome-icon.ts");
  const imagePreview = read("../components/image-preview.ts");
  const attachments = read("../services/attachments.ts");
  const codeView = read("../components/code-view.ts");
  const thread = read("../components/thread-screen.ts");
  const markdown = read("../components/markdown-view.ts");
  const newThread = read("../components/new-thread-setup.ts");
  const settings = read("../components/settings-screen.ts");
  const automations = read("../components/automations-screen.ts");
  const pullRequests = read("../components/pull-requests-dashboard.ts");
  const review = read("../components/code-review-dashboard.ts");
  const providerSettings = read("../components/provider-settings.ts");
  const managementSettings = read("../components/management-settings-panels.ts");
  const cliSettings = read("../components/cli-settings.ts");
  const main = read("../main.ts");
  const gallery = read("../gallery.ts");

  it("keeps the authoritative desktop geometry and density", () => {
    expect(numberFrom(tokens, /--trouve-navigation-width:\s*(\d+)px/, "left pane")).toBe(260);
    expect(numberFrom(tokens, /--trouve-inspection-width:\s*(\d+)px/, "right pane")).toBe(460);
    expect(numberFrom(tokens, /--trouve-font-size:\s*(\d+)px/, "font size")).toBe(13);
    expect(desktopHost).toContain(".with_inner_size(LogicalSize::new(1_400, 900))");
    expect(desktopHost).toContain(".with_min_inner_size(LogicalSize::new(900, 560))");
    expect(app).toContain(
      "grid-template-columns: var(--trouve-navigation-width) 5px minmax(420px, 1fr) 5px var(--trouve-inspection-width)",
    );
    expect(app).toMatch(/\.app-shell \{[^}]*grid-template-rows:\s*minmax\(0, 1fr\)/s);
    expect(shell.match(/role="separator"/g)).toHaveLength(2);
    expect(shell).toContain("#persistPanelWidths");
    expect(shell).toContain("@pointerdown=");
    expect(shell).toContain("@keydown=");
  });

  it("uses the system UI font and sizes the custom-element host", () => {
    expect(tokens).toContain("--trouve-font-sans: system-ui");
    expect(tokens).not.toContain("Inter");
    expect(tokens).toContain("--trouve-line-height: 1.35");
    expect(app).toMatch(
      /trouve-app \{[^}]*display:\s*block[^}]*width:\s*100%;\s*height:\s*100%/s,
    );
    expect(app).toContain(
      "font: var(--trouve-font-size)/var(--trouve-line-height) var(--trouve-font-sans)",
    );
  });

  it("loads WebAwesome's self-hosted base before compact Trouve overrides", () => {
    const baseTheme = '@awesome.me/webawesome/dist/styles/themes/default.css';
    for (const entry of [main, gallery]) {
      expect(entry).toContain(baseTheme);
      expect(entry.indexOf(baseTheme)).toBeLessThan(entry.indexOf("./styles/tokens.css"));
    }
    for (const contract of [
      "--wa-form-control-height: 30px",
      "--wa-form-control-padding-inline: 8px",
      "--wa-form-control-border-radius: var(--trouve-radius)",
      "--wa-focus-ring-width: 2px",
      "--wa-color-brand-fill-loud: var(--trouve-primary-bg)",
      "--wa-color-neutral-fill-loud: var(--trouve-control-bg)",
    ]) {
      expect(tokens).toContain(contract);
    }
  });

  it("defines every Trouve semantic variable used by shared visual components", () => {
    const visualSources = [
      app,
      codeView,
      read("../components/markdown-view.ts"),
      read("../components/terminal-view.ts"),
      read("../components/management-settings-panels.ts"),
      read("../components/mode-settings-panel.ts"),
    ].join("\n");
    const used = new Set(
      [...visualSources.matchAll(/var\((--trouve-[a-z0-9-]+)/g)].map(
        (match) => match[1],
      ),
    );
    const defined = new Set(
      [...`${tokens}\n${themes}`.matchAll(/(--trouve-[a-z0-9-]+)\s*:/g)].map(
        (match) => match[1],
      ),
    );
    expect([...used].filter((name) => !defined.has(name)).sort()).toEqual([]);
  });

  it("keeps every product theme on one semantic role contract", () => {
    const palettes = [...themes.matchAll(/(?:^:root,\n)?\[data-theme="([^"]+)"\] \{\n([\s\S]*?)\n\}/gmu)]
      .map((match) => ({
        name: match[1]!,
        roles: [...match[2]!.matchAll(/(--trouve-[a-z0-9-]+)\s*:/g)].map((role) => role[1]!),
      }));
    expect(palettes.map((palette) => palette.name)).toEqual([
      "dark",
      "light",
      "high-contrast-dark",
      "colorblind-dark",
      "colorblind-light",
    ]);
    const contract = [...palettes[0]!.roles].sort();
    for (const palette of palettes) expect([...palette.roles].sort()).toEqual(contract);
  });

  it("keeps Files-view selections readable in every product theme", () => {
    expect(codeView).toContain('view.EditorView.outerDecorations.compute(\n    ["selection"]');
    expect(codeView).toContain('backgroundColor: "var(--trouve-selection-bg) !important"');
    expect(codeView).toContain('color: "var(--trouve-selection-fg) !important"');

    const palettes = [...themes.matchAll(
      /(?:^:root,\n)?\[data-theme="([^"]+)"\] \{\n([\s\S]*?)\n\}/gmu,
    )].map((match) => ({
      name: match[1]!,
      roles: new Map(
        [...match[2]!.matchAll(/(--trouve-[a-z0-9-]+):\s*(#[0-9a-f]{6});/gi)]
          .map((role) => [role[1]!, role[2]!] as const),
      ),
    }));
    for (const palette of palettes) {
      const background = palette.roles.get("--trouve-selection");
      const foreground = palette.roles.get("--trouve-selection-fg");
      expect(background, `${palette.name} selection background`).toBeDefined();
      expect(foreground, `${palette.name} selection foreground`).toBeDefined();
      expect(
        contrastRatio(foreground!, background!),
        `${palette.name} selected text contrast`,
      ).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("keeps emphasized diff text readable in every product theme", () => {
    const palettes = [...themes.matchAll(
      /(?:^:root,\n)?\[data-theme="([^"]+)"\] \{\n([\s\S]*?)\n\}/gmu,
    )].map((match) => ({
      name: match[1]!,
      roles: new Map(
        [...match[2]!.matchAll(/(--trouve-[a-z0-9-]+):\s*(#[0-9a-f]{6});/gi)]
          .map((role) => [role[1]!, role[2]!] as const),
      ),
    }));
    for (const palette of palettes) {
      const foreground = palette.roles.get("--trouve-code-fg");
      expect(foreground, `${palette.name} code foreground`).toBeDefined();
      for (const role of ["--trouve-diff-add-text-bg", "--trouve-diff-del-text-bg"]) {
        const background = palette.roles.get(role);
        expect(background, `${palette.name} ${role}`).toBeDefined();
        expect(
          contrastRatio(foreground!, background!),
          `${palette.name} ${role} contrast`,
        ).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("keeps the established desktop navigation hierarchy and density", () => {
    expect(shell).not.toContain('class="brand-row"');
    expect(shell).not.toContain(">Inbox</button>");
    const pullRequests = shell.indexOf("<strong>Pull Requests</strong>");
    const automations = shell.indexOf("<strong>Automations</strong>");
    const settings = shell.indexOf("<strong>Settings</strong>");
    const workspaces = shell.indexOf("<strong>Workspaces</strong>");
    expect(pullRequests).toBeGreaterThan(-1);
    expect(pullRequests).toBeLessThan(automations);
    expect(automations).toBeLessThan(settings);
    expect(settings).toBeLessThan(workspaces);
    expect(shell).toContain('class="workspace-row"');
    expect(shell).toContain("data-controls-visible=${");
    expect(shell).toContain('class="workspace-toggle"');
    expect(shell).toContain('class="workspace-new-session"');
    expect(shell).toContain("#toggleWorkspace");
    expect(app).toMatch(/\.primary-links button \{[^}]*height:\s*34px/s);
    expect(app).toMatch(/\.workspace-row \{[^}]*height:\s*34px/s);
    expect(app).toMatch(/\.workspace-toggle > span \{[^}]*inset-inline-start:\s*3px/s);
    expect(app).toMatch(/\.session-row-wrap \{[^}]*height:\s*34px/s);
    expect(app).toMatch(/\.session-row \{[^}]*height:\s*34px/s);
    expect(app).toMatch(/\.session-copy strong \{[^}]*font-size:\s*13px/s);
    expect(app).toMatch(/\.session-copy strong \{[^}]*color:\s*var\(--trouve-text-mid\)/s);
    expect(app).toMatch(/\.session-age \{[^}]*font-size:\s*11px/s);
    expect(app).toContain(".workspace-row:hover .workspace-order-controls");
    expect(app).toContain(".workspace-row:focus-within .workspace-actions-wrap");
    expect(app).not.toContain(".session-copy small");
  });

  it("renders an outlined destination slot for every reorderable drag surface", () => {
    expect(shell).toContain('data-drop-placeholder="workspace"');
    expect(thread).toContain('data-drop-placeholder="queue"');
    expect(pullRequests).toContain('data-drop-placeholder="pull-request-group"');
    expect(review).toContain('data-drop-placeholder="code-review-group"');

    expect(app).toMatch(
      /\.workspace-drop-placeholder\s*\{[^}]*border:\s*1px dashed var\(--trouve-accent\)/s,
    );
    expect(app).toMatch(
      /\.queue-panel li\.queue-drop-placeholder\s*\{[^}]*border:\s*1px dashed var\(--trouve-accent\)/s,
    );
    expect(pullRequests).toMatch(
      /\.group-drop-placeholder\s*\{[^}]*border:\s*1px dashed var\(--trouve-accent\)/s,
    );
    expect(review).toMatch(
      /\.review-group-drop-placeholder\s*\{[^}]*border:\s*1px dashed var\(--trouve-accent\)/s,
    );

    expect(app).not.toContain(".workspace-group.drop-before::before");
    expect(app).not.toContain('li[data-queue-drop="before"]::before');
    expect(pullRequests).not.toContain(".group-card.drop-target");
    expect(review).not.toContain(".review-job-group.drop-before");
  });

  it("renders settings, automations, and pull requests as dedicated full-window screens", () => {
    expect(shell).toContain('class="app-shell mobile-pane-${this.#mobilePane} ${fullScreenRoute');
    expect(shell).toContain("@trouve-close-full-screen=${this.#closeFullScreenRoute}");
    expect(app).toContain(".app-shell.full-screen-route");
    expect(app).toMatch(/\.full-screen-route > \.navigation-panel,[\s\S]*\.full-screen-route > \.status-bar \{ display: none; \}/);
    expect(app).toMatch(
      /\.app-shell\.full-screen-route > \.thread-panel:not\(\.new-session-screen\) \{[^}]*grid-column:\s*1/s,
    );
    for (const screen of [settings, automations, pullRequests]) {
      expect(screen).toContain("trouve-close-full-screen");
      expect(screen).toContain('fontAwesomeIcon("xmark")');
      expect(screen).toContain("Close</button>");
    }
  });

  it("keeps the established settings hierarchy and centered 170/640 geometry", () => {
    for (const section of [
      '"general"',
      '"chat"',
      '"providers"',
      '"modes"',
      '"mcp"',
      '"integrations"',
      '"appearance"',
      '"notifications"',
      '"about"',
    ]) {
      expect(settings).toContain(section);
    }
    expect(settings).toContain("Modes & Models");
    expect(settings).toContain("Sessions & Chat");
    expect(settings).not.toContain('return "Git & Worktrees"');
    expect(settings).toContain("MCP Servers");
    expect(app).toMatch(/\.settings-screen \{[^}]*grid-template-rows:\s*44px minmax\(0, 1fr\)/s);
    expect(app).toMatch(/\.settings-layout \{[^}]*width:\s*810px[^}]*grid-template-columns:\s*170px 640px/s);
    expect(app).toMatch(/\.settings-nav \{[^}]*padding:\s*20px 12px 0 0/s);
    expect(app).toMatch(/\.settings-content \{[^}]*width:\s*640px[^}]*padding:\s*16px/s);
    expect(settings).not.toContain("<h2>Agent activity</h2>");
    expect(settings).not.toContain('class="theme-preview"');
    expect(settings).toMatch(/id="settings-font-family"[\s\S]*?<option value="">System default<\/option>/);
    expect(settings).not.toMatch(/<input\s+id="settings-font-family"/);
    expect(settings).not.toContain("AboutSlint");
    expect(settings).not.toContain("MadeWithSlint");
    expect(settings).not.toContain("slint.dev");
  });

  it("keeps mode rows compact on desktop and touch-safe when stacked", () => {
    const modes = read("../components/mode-settings-panel.ts");
    expect(modes).toMatch(
      /\.mode-row \{[^}]*box-sizing:\s*border-box[^}]*height:\s*52px[^}]*padding:\s*0 6px 0 10px/s,
    );
    expect(modes).toMatch(/\.mode-row-copy \{[^}]*line-height:\s*1\.2/s);
    expect(modes).toMatch(
      /@media \(max-width:\s*620px\)[\s\S]*\.mode-row \{[^}]*height:\s*auto[^}]*min-height:\s*68px/s,
    );
  });

  it("keeps automations in the centered list and inline-form flow", () => {
    expect(automations).toContain("Scheduled prompts");
    expect(automations).toContain('class="body-column"');
    expect(automations).toMatch(/\.body-column \{[^}]*width:\s*min\(680px, 100%\)/s);
    expect(automations).toContain('class="automation-card"');
    expect(automations).toContain('class="template-card"');
    expect(automations).not.toContain('class="layout"');
    expect(automations).not.toContain('class="list-panel"');
    expect(automations).not.toContain('class="detail-panel"');
  });

  it("keeps the pull-request dashboard hierarchy free of a persistent redesign tab strip", () => {
    expect(pullRequests).toMatch(/\.screen \{[^}]*grid-template-rows:\s*52px minmax\(0, 1fr\)/s);
    expect(pullRequests).toContain('class="account-body"');
    expect(pullRequests).toContain('class="groups-grid"');
    expect(pullRequests).not.toContain('class="view-tabs"');
    expect(pullRequests).not.toContain('role="tablist"');
  });

  it("distinguishes a connected empty inbox from protocol bootstrap", () => {
    expect(shell).toContain("#protocolReady = false");
    expect(shell).toContain('aria-label="No active session"');
    expect(shell).toContain('session-id=""');
    expect(shell).not.toContain('this.#protocolReady ? "No session selected"');
  });

  it("keeps the primary inspection order and desktop-height contract", () => {
    expect(shell).toMatch(
      /const INSPECTION_PANELS = \[\s*"info",\s*"diff",\s*"files",\s*"pr",\s*"terminal",/,
    );
    for (const [panel, icon, label] of [
      ["info", "circle-info", "Details"],
      ["diff", "code-compare", "Diff"],
      ["files", "file-lines", "Files"],
      ["pr", "code-pull-request", "Pull Requests"],
      ["terminal", "terminal", "Terminal"],
    ]) {
      expect(shell).toContain(`${panel}: { icon: "${icon}", label: "${label}" }`);
    }
    expect(app).toMatch(/\.status-bar \{[^}]*display:\s*none/s);
    expect(app).toMatch(/\.status-bar\.actionable \{[^}]*display:\s*flex/s);
    expect(app).toMatch(
      /@media \(max-width: 760px\)[\s\S]*\.status-bar \{[^}]*display:\s*flex/,
    );
  });

  it("keeps retained terminal sessions hidden behind other inspection tabs", () => {
    expect(shell).toContain('class="inspection-content retained-terminal-panel"');
    expect(shell).toContain("?hidden=${!visible}");
    expect(app).toMatch(
      /\.retained-terminal-panel\[hidden\] \{[^}]*display:\s*none\s*!important/s,
    );
  });

  it("constrains queued prompts to the thread column", () => {
    expect(app).toMatch(
      /\.queue-panel \{[^}]*min-width:\s*0[^}]*overflow-x:\s*hidden[^}]*overflow-y:\s*auto/s,
    );
    expect(app).toMatch(
      /\.queue-panel li \{[^}]*min-width:\s*0[^}]*grid-template-columns:\s*minmax\(0, 1fr\)/s,
    );
    expect(app).toMatch(
      /\.queue-row \{[^}]*width:\s*100%[^}]*min-width:\s*0[^}]*max-width:\s*100%/s,
    );
    expect(app).toMatch(
      /\.queue-drag-image \{[^}]*width:\s*min\(320px, calc\(100vw - 32px\)\)[^}]*height:\s*32px[^}]*text-overflow:\s*ellipsis[^}]*pointer-events:\s*none/s,
    );
    expect(app).toMatch(/\.queue-row p \{[^}]*flex:\s*1 1 0[^}]*text-overflow:\s*ellipsis/s);
  });

  it("keeps new-session setup inline in the center column", () => {
    expect(shell).toContain('id="new-session-screen"');
    expect(shell).toContain('class="thread-panel new-session-screen"');
    expect(shell).not.toContain('id="new-session-dialog"');
    expect(shell).toContain("Pick where to work, what to branch from, and how the agent should run.");
    expect(shell).toContain("Use latest remote branch");
    expect(app).toMatch(/\.new-session-screen \{[^}]*grid-column:\s*3[^}]*grid-row:\s*1/s);
    expect(app).toMatch(/\.new-session-screen form \{[^}]*align-content:\s*center[^}]*padding:\s*40px/s);
    expect(app).toMatch(/\.new-session-screen\[hidden\] \{[^}]*display:\s*none\s*!important/s);
  });

  it("keeps the thread, turn-card, and composer geometry", () => {
    expect(app).toMatch(/\.thread-tabs button \{[^}]*width:\s*145px[^}]*height:\s*30px/s);
    expect(app).toMatch(/\.thread-header \{[^}]*padding:\s*10px[^}]*box-shadow:/s);
    expect(app).toMatch(/\.thread-tab-header \{[^}]*z-index:\s*8/s);
    expect(app).toMatch(
      /\.thread-tab-title \{[^}]*overflow:\s*hidden[^}]*text-overflow:\s*ellipsis[^}]*white-space:\s*nowrap/s,
    );
    expect(app).toMatch(
      /\.thread-panel \{[^}]*grid-template-columns:\s*minmax\(0, 1fr\)[^}]*overflow:\s*hidden/s,
    );
    expect(app).toMatch(
      /\.chat-stream \{[^}]*padding-inline:\s*10px/s,
    );
    expect(app).toMatch(
      /\.chat-stream \{[^}]*overflow-y:\s*scroll[^}]*scrollbar-gutter:\s*stable[^}]*scrollbar-width:\s*thin/s,
    );
    expect(app).toMatch(
      /\.chat-scroll-indicator \{[^}]*background:\s*var\(--trouve-scroll-thumb\)[^}]*opacity:\s*0[^}]*pointer-events:\s*none/s,
    );
    expect(app).toMatch(/\.chat-scroll-indicator\[data-scrollable\] \{[^}]*opacity:\s*1/s);
    expect(app).toMatch(/\.message \{[^}]*margin:\s*0 0 10px/s);
    expect(app).toMatch(/\.turn-rule \{[^}]*margin:\s*4px 0 14px/s);
    expect(app).toMatch(
      /\.turn-rule-actions button \{[^}]*width:\s*24px[^}]*height:\s*24px[^}]*font-size:\s*12px/s,
    );
    expect(app).toMatch(
      /\.agent-activity \{[^}]*margin:\s*4px 0 10px/s,
    );
    expect(app).toContain(".user-message .message-header");
    expect(app).toContain(".assistant-message .message-header");
    expect(app).toContain(".todo-rail-item");
    expect(app).toMatch(
      /\.turn-body-stream \{[^}]*min-width:\s*0[^}]*grid-template-columns:\s*minmax\(0, 1fr\)/s,
    );
    expect(app).toMatch(
      /\.turn-card \.turn-body-stream \{[^}]*padding:\s*8px 16px 10px/s,
    );
    expect(app).toMatch(
      /\.user-body-stream > \.attachment-list \{[^}]*margin:\s*0/s,
    );
    expect(app).toMatch(
      /\.agent-body-stream > \.message \{[^}]*width:\s*100%[^}]*margin:\s*0/s,
    );
    expect(app).toMatch(
      /\.activity-group \{[^}]*width:\s*100%[^}]*margin:\s*0/s,
    );
    expect(app).toMatch(
      /\.activity-group-body \{[^}]*min-width:\s*0[^}]*grid-template-columns:\s*minmax\(0, 1fr\)[^}]*padding:\s*3px 0 6px/s,
    );
    expect(app).toMatch(
      /\.activity-group-timeline \{[^}]*width:\s*100%[^}]*max-width:\s*100%[^}]*gap:\s*6px[^}]*margin-inline-start:\s*0/s,
    );
    expect(app).toMatch(
      /\.tool-card \{[^}]*min-width:\s*0[^}]*max-width:\s*100%[^}]*overflow:\s*visible/s,
    );
    expect(app).toMatch(
      /\.tool-card summary \{[^}]*position:\s*relative[^}]*min-width:\s*0[^}]*max-width:\s*100%[^}]*overflow:\s*visible/s,
    );
    expect(app).toMatch(
      /\.tool-card pre \{[^}]*min-width:\s*0[^}]*max-width:\s*100%[^}]*overflow-x:\s*hidden[^}]*overflow-wrap:\s*anywhere[^}]*white-space:\s*pre-wrap/s,
    );
    expect(app).toMatch(
      /\.composer-entry \{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) auto/s,
    );
    expect(app).toMatch(
      /\.composer-entry-actions \{[^}]*display:\s*flex[^}]*align-items:\s*end[^}]*gap:\s*6px/s,
    );
    expect(app).toMatch(
      /\.composer textarea \{[^}]*min-height:\s*34px[^}]*max-height:\s*162px/s,
    );
    expect(app).toMatch(/\.composer \{[^}]*margin:\s*8px 10px 10px/s);
    expect(app).toMatch(
      /\.activity-group \{[^}]*border:\s*0[^}]*background:\s*transparent/s,
    );
    expect(app).toMatch(
      /\.activity-group > summary \{[^}]*border:\s*0[^}]*color:\s*var\(--trouve-text-mid\)[^}]*background:\s*transparent/s,
    );
    expect(app).toMatch(
      /\.activity-group-body \{[^}]*border:\s*0[^}]*background:\s*transparent/s,
    );
    expect(app).toMatch(
      /\.tool-card \{[^}]*border:\s*0[^}]*border-radius:\s*0[^}]*background:\s*transparent/s,
    );
    expect(app).toMatch(
      /\.agent-activity-timeline \{[^}]*display:\s*grid[^}]*grid-template-columns:\s*minmax\(0, 1fr\)[^}]*gap:\s*6px[^}]*padding-inline-start:\s*20px/s,
    );
    expect(app).toMatch(
      /\.agent-activity-timeline::before \{[^}]*inset-block:\s*15px[^}]*width:\s*var\(--trouve-chat-timeline-rail-width\)[^}]*background:\s*var\(--trouve-border-strong\)[^}]*opacity:\s*\.65/s,
    );
    expect(app).toMatch(
      /\.agent-activity-timeline\.single-activity::before \{[^}]*inset-block:\s*6px/s,
    );
    expect(app).toMatch(
      /\.agent-activity-timeline\.ends-with-expanded-tool-group::before \{[^}]*inset-block-end:\s*20\.5px/s,
    );
    expect(app).toMatch(
      /@media \(max-width: 760px\)[\s\S]*\.agent-activity-timeline\.ends-with-expanded-tool-group::before \{[^}]*inset-block-end:\s*26px/s,
    );
    expect(app).not.toMatch(
      /\.agent-activity-timeline\.has-expanded-group::before \{[^}]*display:\s*none/s,
    );
    expect(app).toMatch(
      /\.agent-activity-timeline > \.activity-group::before, \.agent-activity-timeline > \.tool-card::before \{[^}]*display:\s*none/s,
    );
    expect(app).toMatch(
      /\.thinking-rail-icon \{[^}]*inset-block-start:\s*8px[^}]*inset-inline-start:\s*-20\.5px[^}]*width:\s*16px[^}]*height:\s*16px[^}]*color:\s*var\(--trouve-text-faint\)[^}]*background:\s*var\(--trouve-win-bg\)/s,
    );
    expect(app).toMatch(
      /\.thinking-rail-icon \.trouve-icon \{[^}]*--trouve-icon-width:\s*14px/s,
    );
    expect(app).toMatch(
      /\.thinking-output > \.thinking-rail-icon \{[^}]*inset-block-start:\s*5\.5px/s,
    );
    expect(app).toMatch(
      /@media \(max-width: 760px\)[\s\S]*\.thinking-card > \.thinking-rail-icon \{[^}]*inset-block-start:\s*13px[^}]*\}[\s\S]*\.thinking-output > \.thinking-rail-icon \{[^}]*inset-block-start:\s*12px/s,
    );
    expect(app).toMatch(
      /\.activity-rail-disclosure \{[^}]*position:\s*absolute[^}]*inset-block-start:\s*50%[^}]*inset-inline-start:\s*-20\.5px[^}]*width:\s*16px[^}]*height:\s*16px[^}]*background:\s*var\(--trouve-win-bg\)[^}]*transform:\s*translateY\(-50%\)/s,
    );
    expect(app).toMatch(
      /\.activity-rail-disclosure-icon \{[^}]*--trouve-icon-width:\s*14px/s,
    );
    expect(app).toMatch(
      /\.activity-group-timeline > \.tool-card::before, \.activity-group-timeline > \.todo-rail-item::before \{[^}]*inset-block-start:\s*50%[^}]*inset-inline-start:\s*-12\.5px[^}]*width:\s*20\.5px[^}]*background:\s*var\(--trouve-border-strong\)/s,
    );
    expect(app).toMatch(
      /\.activity-group-timeline \{[^}]*padding-inline-start:\s*0/s,
    );
    expect(app).toMatch(
      /\.activity-group-timeline::before \{[^}]*display:\s*none/s,
    );
    expect(app).toMatch(
      /\.activity-group-timeline > \.tool-card > summary > \.activity-rail-disclosure \{[^}]*position:\s*static[^}]*transform:\s*none/s,
    );
    expect(app).toMatch(
      /\.activity-group > summary:focus-visible \{[^}]*outline:\s*2px solid var\(--trouve-focus\)[^}]*outline-offset:\s*-2px/s,
    );
    expect(app).toMatch(
      /\.tool-card summary::before \{[^}]*inset-inline:\s*0[^}]*border-radius:\s*var\(--trouve-radius-sm\)[^}]*background:\s*transparent/s,
    );
    expect(app).toMatch(
      /\.activity-group-timeline > \.tool-card > summary::before \{[^}]*inset-inline-start:\s*22px/s,
    );
    expect(app).toMatch(
      /\.tool-card summary:focus-visible::before \{[^}]*box-shadow:\s*inset 0 0 0 2px var\(--trouve-focus\)/s,
    );
    expect(app).toMatch(
      /\.tool-inline-status \{[^}]*width:\s*12px[^}]*height:\s*12px[^}]*display:\s*inline-grid/s,
    );
    expect(app).toMatch(
      /\.tool-card summary > strong \{[^}]*font-size:\s*11px[^}]*font-weight:\s*600/s,
    );
    expect(app).toMatch(
      /\.agent-activity-timeline\.compaction-connected-timeline::before \{[^}]*inset-block:\s*6px[^}]*display:\s*block/s,
    );
    expect(app).toMatch(
      /\.context-compaction-marker\.timeline-connect-before::before, \.context-compaction-marker\.timeline-connect-after::before \{[^}]*inset-block:\s*50%[^}]*inset-inline-start:\s*7px[^}]*width:\s*var\(--trouve-chat-timeline-rail-width\)/s,
    );
    expect(app).toMatch(
      /\.context-compaction-marker\.timeline-connect-before \.context-compaction-symbol, \.context-compaction-marker\.timeline-connect-after \.context-compaction-symbol \{[^}]*inset-block-start:\s*50%[^}]*inset-inline-start:\s*-1\.5px[^}]*translateY\(-50%\)/s,
    );
    expect(app).toMatch(
      /\.agent-activity-timeline \+ \.agent-text-block \{[^}]*margin-block-start:\s*6px/s,
    );
    expect(app).toMatch(
      /\.agent-text-block \{[^}]*max-inline-size:\s*120ch/s,
    );
    expect(app).toMatch(
      /\.agent-copy-action \{[^}]*display:\s*inline-flex[^}]*flex:\s*none/s,
    );
    expect(app).toMatch(
      /\.assistant-message:hover > \.agent-header > \.agent-copy-action[^}]*opacity:\s*1/s,
    );
    expect(app).toMatch(
      /\.thinking-output trouve-markdown-view \{[^}]*max-width:\s*120ch/s,
    );
    expect(app).toMatch(
      /\.conversation-turn \{[^}]*border:\s*1px solid var\(--trouve-border\)[^}]*background:\s*transparent/s,
    );
    expect(app).toMatch(
      /\.conversation-turn > \.turn-header \{[^}]*border-bottom:\s*1px solid var\(--trouve-agent-border\)[^}]*background:\s*var\(--trouve-agent-bg\)/s,
    );
    expect(app).toMatch(
      /\.turn-timeline::before \{[^}]*inset-block:\s*23px[^}]*inset-inline-start:\s*23px[^}]*width:\s*var\(--trouve-chat-timeline-rail-width\)/s,
    );
    expect(app).not.toContain(".turn-timeline > .turn-rail-node:last-child::after");
    expect(app).toMatch(
      /\.turn-rail-marker\.transient \{[^}]*color:\s*var\(--trouve-accent\)/s,
    );
    expect(app).toMatch(
      /\.turn-transient-activity \.turn-node-header strong \{[^}]*color:\s*var\(--trouve-text-dim\)[^}]*font-size:\s*11px/s,
    );
    expect(app).toMatch(
      /\.turn-header-metadata-slot \{[^}]*width:\s*34ch[^}]*min-width:\s*34ch[^}]*justify-content:\s*flex-end/s,
    );
    expect(app).toMatch(
      /\.turn-header-metadata-slot \.turn-metadata \{[^}]*font-variant-numeric:\s*tabular-nums/s,
    );
    expect(app).toMatch(
      /\.composer-context-usage\.turn-context-usage \{[^}]*width:\s*20px[^}]*height:\s*20px[^}]*align-self:\s*center/s,
    );
    expect(app).toMatch(
      /\.turn-rail-marker\.prompt \{[^}]*color:\s*var\(--trouve-accent\)/s,
    );
    expect(app).toMatch(
      /\.turn-rail-marker\.response\.running \{[^}]*color:\s*var\(--trouve-accent\)/s,
    );
    expect(app).toMatch(
      /\.turn-rail-marker\.response\.complete \{[^}]*color:\s*var\(--trouve-ok\)/s,
    );
    expect(thread).toContain('class="composer-option mode-option"');
    expect(thread).toContain('class="composer-option model-option"');
    expect(thread).toContain('class="composer-option permission-option"');
    expect(thread).toContain("message-body turn-body-stream agent-body-stream turn-timeline");
    expect(thread).not.toContain("turn-activity-footer");
    expect(thread).toContain('class="turn-rail-node turn-transient-activity"');
    expect(thread).toContain('className: "turn-transient-spinner"');
    expect(thread).toContain('this.#renderContextUsage(turnContextUsage, "turn-context-usage")');
    expect(thread).toContain('class="turn-node-body user-body-stream"');
    expect(thread).not.toContain("Collapse your message");
    expect(thread).not.toContain("Collapse agent message");
    expect(thread).toContain("agent-activity-timeline");
    expect(thread).toContain('activityRows.length === 1 ? "single-activity" : ""');
    expect(thread).toContain('"ends-with-expanded-tool-group"');
    expect(thread).not.toContain("activity-group-status");
    expect(thread).toContain('class="activity-rail-disclosure"');
    expect(thread).toContain('className: "activity-rail-disclosure-icon"');
    expect(thread).toContain('class="tool-inline-status ${item.status}"');
    const toolCase = thread.slice(
      thread.indexOf('case "tool":'),
      thread.indexOf('case "questions":'),
    );
    const detailPosition = toolCase.indexOf('class="tool-meta tool-detail-meta"');
    const statusPosition = toolCase.indexOf('class="tool-inline-status ${item.status}"');
    const durationPosition = toolCase.indexOf('class="tool-meta tool-duration"');
    expect(detailPosition).toBeGreaterThan(-1);
    expect(detailPosition).toBeLessThan(statusPosition);
    expect(statusPosition).toBeLessThan(durationPosition);
    expect(thread).not.toContain('class="turn-markdown"');
    expect(thread).toContain('class="agent-copy-action"');
    expect(thread).toContain('response ? "Copy assistant response" : "Copy assistant update"');
    expect(markdown).not.toContain(":host(.turn-markdown)");
    expect(thread).toContain('candidate.spawned === true');
    expect(thread).toContain('fontAwesomeIcon("code-branch")');
    expect(thread).toContain('event.key !== "Enter" || event.shiftKey');
    expect(app).toMatch(
      /@media \(max-width: 760px\)[\s\S]*\.thread-tabs \{[^}]*height:\s*42px/,
    );
  });

  it("uses a local Font Awesome running-tool spinner with product timing", () => {
    expect(icons).toContain('@fortawesome/fontawesome-free/css/solid.css');
    expect(icons).toContain('spinner: 0xf110');
    expect(thread).toContain('running: "spinner"');
    expect(thread).toContain('spin: item.status === "running"');
    expect(app).toMatch(
      /\.trouve-icon-spin \{[^}]*animation:\s*trouve-font-awesome-spin 900ms linear infinite/s,
    );
    expect(app).toMatch(
      /\[data-reduce-motion\] \.trouve-icon-spin \{[^}]*animation:\s*none/s,
    );
  });

  it("keeps the session-list status precedence and indicators", () => {
    expect(sessionList).toContain("sessionIndicatorPresentation(session)");
    expect(sessionIndicators).toContain('icon: "triangle-exclamation"');
    expect(sessionIndicators).toContain('icon: "circle-question"');
    expect(sessionIndicators).toContain('icon: "xmark"');
    expect(sessionIndicators).toContain('kind: "unread", icon: "circle"');
    expect(sessionIndicators).toContain('kind: "busy", icon: undefined');
    expect(sessionIndicators).toContain('kind: "none", icon: undefined');
    expect(sessionList).toContain('class="session-pr-badge ${pullRequestBadge.tone}"');
    expect(app).toMatch(
      /\.session-indicator\.approval,[^}]*color:\s*var\(--trouve-warn\)[^}]*font-size:\s*16px/s,
    );
    expect(app).toMatch(
      /\.session-indicator\.error \{[^}]*color:\s*var\(--trouve-err\)[^}]*font-size:\s*18px/s,
    );
    expect(app).toMatch(
      /\.session-indicator\.unread \{[^}]*color:\s*var\(--trouve-accent\)[^}]*font-size:\s*11px/s,
    );
    expect(app).toMatch(
      /\.session-indicator\.busy::before \{[^}]*width:\s*10px[^}]*height:\s*10px[^}]*background:\s*var\(--trouve-accent\)[^}]*animation:\s*trouve-session-busy-pulse 1\.6s linear infinite/s,
    );
  });

  it("ships every current semantic palette from the generated source", () => {
    expect([...themes.matchAll(/\[data-theme="([^"]+)"\]/g)].map((match) => match[1])).toEqual([
      "dark",
      "light",
      "high-contrast-dark",
      "colorblind-dark",
      "colorblind-light",
    ]);
    for (const role of [
      "win-bg",
      "panel-bg",
      "sidebar-bg",
      "text-hi",
      "accent",
      "user-bg",
      "diff-add-bg",
      "diff-del-bg",
      "ok",
      "warn",
      "err",
    ]) {
      expect(themes).toContain(`--trouve-${role}:`);
    }
  });

  it("retains keyboard, contrast, motion, touch, and safe-area rules", () => {
    expect(app).toContain(":focus-visible");
    expect(app).toContain("@media (prefers-reduced-motion: reduce)");
    expect(app).toContain("@media (forced-colors: active)");
    expect(app).toContain("@media (prefers-contrast: more)");
    expect(app).toContain("@media (max-width: 760px)");
    expect(app).toContain("env(safe-area-inset-bottom)");
    expect(app).toContain("env(safe-area-inset-top)");
    expect(app).toContain(".files-inspection:not(.file-tree-collapsed) > .file-view-shell");
    expect(app).toContain(".diff-inspection:not(.diff-tree-collapsed) > .diff-view-shell");
    expect(app).toContain(".files-inspection, .diff-inspection");
    expect(app).toMatch(/\.tool-approval-actions \{[^}]*position:\s*fixed[^}]*minmax\(0, 1fr\)/s);
    expect(pullRequests).toContain("calc(52px + env(safe-area-inset-top))");
    expect(pullRequests).toContain('class="touch-group-order"');
    expect(pullRequests).toContain("Move ${group.title} up");
    expect(pullRequests).toContain("Move ${group.title} down");
    expect(automations).toContain("calc(52px + env(safe-area-inset-top))");
    expect(app).toMatch(/\.mobile-nav button \{[^}]*min-height:\s*44px/);
  });

  it("keeps load-bearing styles compatible with the pinned Servo preview", () => {
    const servoStyles = [tokens, app, newThread, automations, review].join("\n");
    expect(servoStyles).not.toContain(":has(");
    expect(servoStyles).not.toContain("color-mix(");
    expect(app).toContain(".attachment-button:focus-within");
    expect(app).toContain(".attachment-button.disabled");
    expect(automations).toContain(".day-option.selected");
    expect(tokens).toContain("--wa-color-shadow: var(--trouve-scrim)");
  });

  it("keeps permission state visible and full access error-colored", () => {
    expect(shell).toContain("permission-status");
    expect(shell).toContain('activeThread.permission_mode === "yolo"');
    expect(thread).toContain('thread.permission_mode === "yolo"');
    expect(app).toMatch(
      /\.permission-option > span\.permission-yolo \{[^}]*color:\s*var\(--trouve-err\)/s,
    );
    expect(app).toMatch(
      /\.permission-option select\.permission-yolo:not\(:disabled\)[^{]*\{[^}]*border-color:\s*var\(--trouve-err\)[^}]*color:\s*var\(--trouve-err\)/s,
    );
    expect(app).toMatch(
      /\.permission-option select\.permission-yolo:disabled \{[^}]*border-color:\s*var\(--trouve-border\)[^}]*color:\s*var\(--trouve-text-disabled\)[^}]*font-weight:\s*400/s,
    );
    expect(thread).toContain('class="permission-warning"');
    expect(thread).toContain('title="YOLO: changes run without approval"');
    expect(thread).toContain('fontAwesomeIcon("triangle-exclamation")');
    expect(app).toMatch(
      /\.permission-warning \{[^}]*width:\s*22px[^}]*height:\s*30px[^}]*display:\s*grid[^}]*place-items:\s*center/s,
    );
    expect(app).toContain(".composer-option select:hover:not(:disabled)");
    expect(app).not.toContain(".composer-option select:hover {");
  });

  it("reuses submitted attachment-card geometry for pending images and files", () => {
    expect(thread).toContain('class="attachment-list pending-attachments"');
    expect(shell).toContain('class="attachment-list pending-attachments"');
    expect(thread).toContain("pendingAttachmentPreviewUrl(attachment)");
    expect(shell).toContain("pendingAttachmentPreviewUrl(attachment)");
    expect(newThread).toContain("pendingAttachmentPreviewUrl(attachment)");
    expect(attachments).toContain("data:${mime};base64,${attachment.upload.data}");
    for (const source of [thread, shell, newThread]) {
      expect(source).toContain("<trouve-image-preview");
    }
    expect(imagePreview).toMatch(
      /\.image-preview-trigger \{[^}]*width:\s*64px[^}]*height:\s*48px/s,
    );
    expect(imagePreview).toMatch(
      /\.image-preview-trigger img \{[^}]*object-fit:\s*cover/s,
    );
    expect(imagePreview).toMatch(
      /\.image-preview-full \{[^}]*object-fit:\s*contain/s,
    );
    expect(imagePreview).toContain("dialog.showModal()");
    expect(imagePreview).toContain("View full-size image:");
    expect(app).toMatch(
      /\.attachment-icon \{[^}]*width:\s*64px[^}]*height:\s*48px[^}]*display:\s*grid[^}]*place-items:\s*center/s,
    );
    expect(app).toMatch(
      /\.pending-attachments li \{[^}]*grid-template-columns:\s*auto minmax\(0, 1fr\) auto/s,
    );
  });

  it("keeps compact settings labels, meters, copy, and form alignment", () => {
    expect(cliSettings).toContain(">Uninstall</button>");
    expect(cliSettings).not.toContain(">Remove managed</button>");
    expect(managementSettings).toMatch(
      /\.meta \{[^}]*font-size:\s*var\(--trouve-settings-info-font-size, 11px\)/s,
    );
    expect(managementSettings).toContain('class="integration-add-fields"');
    expect(managementSettings).toMatch(
      /\.integration-add-fields \{[^}]*grid-template-columns:\s*repeat\(2, minmax\(0, 1fr\)\) auto[^}]*align-items:\s*end/s,
    );
    expect(providerSettings).toContain("subscriptionUsageTone(percent)");
    expect(providerSettings).toContain('class="health-meter"');
    expect(providerSettings).not.toContain("<progress max=\"100\" .value=");
    expect(providerSettings).not.toContain("${health.status}</span>");
  });

  it("keeps the session-naming choices and reactive explanations", () => {
    for (const label of [
      "Adaptive (Recommended)",
      "Keep Ready",
      "Load When Needed",
      "Rules Only",
      "GPU, CPU, & RAM",
      "GPU Only",
      "CPU & RAM Only",
    ]) {
      expect(managementSettings).toContain(`label: "${label}"`);
    }
    for (const description of [
      "Keeps the naming model ready when this computer has comfortable memory headroom; otherwise loads it only when needed.",
      "Loads the naming model at startup and keeps it in memory for the fastest new-session creation.",
      "Loads the naming model when a session is created, then releases it after a short idle period.",
      "Uses fast built-in heuristics and never loads the optional naming model.",
      "Uses GPU, CPU, and RAM when no local coding model is active; otherwise uses CPU and RAM only.",
      "Lets llama.cpp use available GPU memory and spill remaining work to CPU and system RAM.",
      "Requires every model layer to fit on a detected GPU; naming falls back to rules when it cannot.",
      "Keeps session naming entirely off the GPU and uses CPU plus system RAM.",
    ]) {
      expect(managementSettings).toContain(`"${description}"`);
    }
    expect(managementSettings).toContain("this.#draftLoadBehavior = behaviorSelect.value");
    expect(managementSettings).toContain("this.#draftResourcePolicy = resourceSelect.value");
    expect(managementSettings).toContain("const behavior = this.#draftLoadBehavior ??");
    expect(managementSettings).toContain("const resources = this.#draftResourcePolicy ??");
    expect(managementSettings).toContain("form.requestSubmit()");
    expect(managementSettings).toContain("current?.title_model_resource_policy ?? \"adaptive\"");
  });
});

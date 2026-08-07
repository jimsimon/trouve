import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const read = (path: string): string =>
  readFileSync(new URL(path, import.meta.url), "utf8");

const numberFrom = (source: string, expression: RegExp, label: string): number => {
  const value = expression.exec(source)?.[1];
  if (value === undefined) throw new Error(`missing visual contract: ${label}`);
  return Number(value);
};

describe("Slint/Lit visual contract", () => {
  const slint = read("../../../../crates/trouve-app/ui/app.slint");
  const settingsSlint = read(
    "../../../../crates/trouve-app/ui/settings-window.slint",
  );
  const slintActivitySpinner = read(
    "../../../../crates/trouve-app/ui/assets/activity-spinner.svg",
  );
  const tokens = read("./tokens.css");
  const themes = read("./themes.generated.css");
  const app = read("./app.css");
  const shell = read("../app/trouve-app.ts");
  const sessionList = read("../components/session-list.ts");
  const sessionIndicators = read("../state/session-indicator-model.ts");
  const icons = read("../components/font-awesome-icon.ts");
  const imagePreview = read("../components/image-preview.ts");
  const attachments = read("../services/attachments.ts");
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

  it("keeps the authoritative desktop geometry and native density", () => {
    const left = numberFrom(slint, /left-width:\s*(\d+)px/, "Slint left pane");
    const right = numberFrom(slint, /right-width:\s*(\d+)px/, "Slint right pane");
    const font = numberFrom(slint, /default-font-size:\s*Theme\.fs\((\d+)px\)/, "Slint font");

    expect(numberFrom(tokens, /--trouve-navigation-width:\s*(\d+)px/, "Lit left pane")).toBe(left);
    expect(numberFrom(tokens, /--trouve-inspection-width:\s*(\d+)px/, "Lit right pane")).toBe(right);
    expect(numberFrom(tokens, /--trouve-font-size:\s*(\d+)px/, "Lit font")).toBe(font);
    expect(slint).toContain("preferred-width: 1400px");
    expect(slint).toContain("preferred-height: 900px");
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
      read("../components/code-view.ts"),
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

  it("keeps the Slint desktop navigation hierarchy and density", () => {
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
    expect(shell).toContain('class="workspace-toggle"');
    expect(shell).toContain('class="workspace-new-session"');
    expect(shell).toContain("#toggleWorkspace");
    expect(app).toMatch(/\.primary-links button \{[^}]*height:\s*34px/s);
    expect(app).toMatch(/\.workspace-row \{[^}]*height:\s*34px/s);
    expect(app).toMatch(/\.session-row-wrap \{[^}]*height:\s*52px/s);
    expect(app).toMatch(/\.session-copy strong \{[^}]*font-size:\s*13px/s);
    expect(app).toMatch(/\.session-copy small \{[^}]*font-size:\s*11px/s);
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
      '"providers"',
      '"modes"',
      '"git-worktrees"',
      '"mcp"',
      '"integrations"',
      '"appearance"',
      '"notifications"',
      '"about"',
    ]) {
      expect(settings).toContain(section);
    }
    expect(settings).toContain("Modes & Models");
    expect(settings).toContain("MCP Servers");
    expect(app).toMatch(/\.settings-screen \{[^}]*grid-template-rows:\s*44px minmax\(0, 1fr\)/s);
    expect(app).toMatch(/\.settings-layout \{[^}]*width:\s*810px[^}]*grid-template-columns:\s*170px 640px/s);
    expect(app).toMatch(/\.settings-nav \{[^}]*padding:\s*20px 12px 0 0/s);
    expect(app).toMatch(/\.settings-content \{[^}]*width:\s*640px[^}]*padding:\s*16px/s);
    expect(settings).not.toContain("<h2>Agent activity</h2>");
    expect(settings).not.toContain('class="theme-preview"');
    expect(settings).toMatch(/id="settings-font-family"[\s\S]*?<option value="">System default<\/option>/);
    expect(settings).not.toMatch(/<input\s+id="settings-font-family"/);
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

  it("keeps Slint's primary inspection order and desktop-height contract", () => {
    expect(shell).toMatch(
      /const INSPECTION_PANELS = \[\s*"diff",\s*"files",\s*"pr",\s*"mcp",\s*"terminal",/,
    );
    for (const [panel, icon, label] of [
      ["diff", "code-compare", "Diff"],
      ["files", "file-lines", "Files"],
      ["pr", "code-pull-request", "Pull Requests"],
      ["mcp", "plug", "MCP"],
      ["terminal", "terminal", "Terminal"],
      ["plan", "list-check", "Todos"],
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

  it("keeps the Slint thread, turn-card, and composer geometry", () => {
    expect(app).toMatch(/\.thread-tabs button \{[^}]*width:\s*145px[^}]*height:\s*30px/s);
    expect(app).toMatch(/\.thread-header \{[^}]*padding:\s*10px[^}]*box-shadow:/s);
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
    expect(app).toMatch(/\.turn-rule \{[^}]*margin:\s*8px 0 6px/s);
    expect(app).toMatch(
      /\.agent-activity \{[^}]*margin:\s*4px 0 10px/s,
    );
    expect(app).toContain(".user-message .message-header");
    expect(app).toContain(".assistant-message .message-header");
    expect(app).toContain(".thread-todo-progress");
    expect(app).toMatch(
      /\.turn-body-stream \{[^}]*min-width:\s*0[^}]*grid-template-columns:\s*minmax\(0, 1fr\)/s,
    );
    expect(app).toMatch(
      /\.turn-card \.turn-body-stream \{[^}]*padding:\s*8px 16px 10px/s,
    );
    expect(app).toMatch(
      /\.user-body-stream > \.attachment-list \{[^}]*margin:\s*0 10px/s,
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
      /\.activity-group-timeline \{[^}]*width:\s*calc\(100% \+ 4px\)[^}]*max-width:\s*none[^}]*gap:\s*2px[^}]*margin-inline-start:\s*-4px/s,
    );
    expect(app).toMatch(
      /\.tool-card \{[^}]*min-width:\s*0[^}]*max-width:\s*100%[^}]*overflow:\s*visible/s,
    );
    expect(app).toMatch(
      /\.tool-card summary \{[^}]*position:\s*relative[^}]*min-width:\s*0[^}]*max-width:\s*100%[^}]*overflow:\s*hidden/s,
    );
    expect(app).toMatch(
      /\.tool-card pre \{[^}]*min-width:\s*0[^}]*max-width:\s*100%[^}]*overflow-x:\s*hidden[^}]*overflow-wrap:\s*anywhere[^}]*white-space:\s*pre-wrap/s,
    );
    expect(app).toMatch(
      /\.composer-entry \{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) 76px/s,
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
      /\.agent-activity-timeline\.has-expanded-group::before \{[^}]*display:\s*none/s,
    );
    expect(app).toMatch(
      /\.agent-activity-timeline > \.activity-group > summary \.disclosure-icon \{[^}]*position:\s*absolute[^}]*inset-block-start:\s*50%[^}]*inset-inline-start:\s*-17\.5px[^}]*translateY\(-50%\)/s,
    );
    expect(app).toMatch(
      /\.agent-activity-timeline > \.activity-group::before, \.agent-activity-timeline > \.tool-card::before, \.agent-activity-timeline > \.thinking-card::before, \.agent-activity-timeline > \.thinking-output::before \{[^}]*width:\s*var\(--trouve-chat-timeline-node-size\)[^}]*height:\s*var\(--trouve-chat-timeline-node-size\)[^}]*border-radius:\s*50%[^}]*background:\s*var\(--trouve-text-faint\)/s,
    );
    expect(app).toMatch(
      /\.agent-activity-timeline > \.activity-group::before, \.agent-activity-timeline > \.tool-card::before \{[^}]*display:\s*none/s,
    );
    expect(app).toMatch(
      /\.tool-status \{[^}]*position:\s*absolute[^}]*inset-block-start:\s*50%[^}]*inset-inline-start:\s*-17\.5px[^}]*width:\s*10px[^}]*height:\s*10px[^}]*background:\s*var\(--trouve-win-bg\)[^}]*transform:\s*translateY\(-50%\)/s,
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
      /\.agent-activity-timeline > \.activity-group\.error::before[^}]*background:\s*var\(--trouve-err\)/s,
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
    expect(thread).toContain('class="composer-option mode-option"');
    expect(thread).toContain('class="composer-option model-option"');
    expect(thread).toContain('class="composer-option permission-option"');
    expect(thread).toContain('class="message-body turn-body-stream user-body-stream"');
    expect(thread).toContain('class="message-body turn-body-stream agent-body-stream"');
    expect(thread).toContain("agent-activity-timeline");
    expect(thread).toContain('activityRows.length === 1 ? "single-activity" : ""');
    expect(thread).toContain('class="turn-markdown"');
    expect(thread).toContain('class="agent-copy-action"');
    expect(thread).toContain('"Copy assistant output"');
    expect(markdown).toMatch(
      /:host\(\.turn-markdown\) :where\([^}]*\) \{[^}]*margin-inline:\s*10px/s,
    );
    expect(thread).toContain('candidate.spawned === true');
    expect(thread).toContain('fontAwesomeIcon("code-branch")');
    expect(thread).toContain('event.key !== "Enter" || event.shiftKey');
    expect(app).toMatch(
      /@media \(max-width: 760px\)[\s\S]*\.thread-tabs \{[^}]*height:\s*42px/,
    );
  });

  it("uses a local Font Awesome running-tool spinner with Slint's timing", () => {
    const spinnerPath = /d="([^"]+)"/.exec(slintActivitySpinner)?.[1];
    expect(spinnerPath).toBe("M12 3a9 9 0 1 1-9 9");
    expect(slintActivitySpinner).toContain('stroke-linecap="round"');
    expect(slintActivitySpinner).toContain('stroke-width="3"');
    expect(slint).toContain("360deg * (mod(root.animation-time, 900ms) / 900ms)");
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

  it("matches Slint's session-list status precedence and indicators", () => {
    expect(slint).toContain("attention, error, unread, busy, PR");
    expect(slint).toContain('icon: row.attention-kind == 2 ? "?" : "!"');
    expect(slint).toContain('icon: "×"');
    expect(slint).toContain('icon: "●"');
    expect(slint).toContain("width: 10px");
    expect(slint).toContain("height: 10px");
    expect(slint).toContain("background: Theme.c.accent");
    expect(slint).toContain("mod(root.activity-animation-time, 1.6s) / 1.6s");
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

  it("keeps compact Slint settings labels, meters, copy, and form alignment", () => {
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

  it("keeps Slint's session-naming choices and reactive explanations", () => {
    for (const label of [
      "Adaptive (Recommended)",
      "Keep Ready",
      "Load When Needed",
      "Rules Only",
      "GPU, CPU, & RAM",
      "GPU Only",
      "CPU & RAM Only",
    ]) {
      expect(settingsSlint).toContain(`"${label}"`);
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
      expect(settingsSlint).toContain(`"${description}"`);
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

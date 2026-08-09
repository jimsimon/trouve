import { html, LitElement } from "lit";

import { THEME_NAMES } from "../services/theme-controller.js";
import { fontAwesomeIcon } from "../components/font-awesome-icon.js";

export class TrouveComponentGallery extends LitElement {
  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  protected override firstUpdated(): void {
    const terminal = this.querySelector("trouve-terminal-view");
    terminal?.write("\u001b[32m✓\u001b[0m protocol connected\r\n$ cargo test --workspace\r\n");
  }

  override render() {
    return html`
      <header class="gallery-header">
        <div>
          <p class="gallery-eyebrow">Migration reference</p>
          <h1>Visual parity gallery</h1>
          <p>Trouve-owned semantic colors and product hierarchy.</p>
        </div>
        <a href="/">Open application shell</a>
      </header>
      <main class="gallery-grid">
        ${THEME_NAMES.map((theme) => this.#renderTheme(theme))}
        ${this.#renderHardWidgets()}
      </main>
    `;
  }

  #renderTheme(theme: (typeof THEME_NAMES)[number]) {
    return html`
      <section class="gallery-theme" data-theme=${theme} aria-labelledby="theme-${theme}">
        <header class="gallery-theme-header">
          <div>
            <p class="gallery-eyebrow">Theme</p>
            <h2 id="theme-${theme}">${theme.replaceAll("-", " ")}</h2>
          </div>
          <div class="gallery-actions">
            <wa-button size="s">Secondary</wa-button>
            <wa-button size="s" variant="brand">Primary</wa-button>
          </div>
        </header>
        <div class="gallery-sample-grid">
          <div>
            <p class="gallery-label">Session navigation</p>
            <button class="session-row selected">
              <span class="session-indicator busy" aria-hidden="true"></span>
              <span class="session-copy"><strong>Preserve existing UX</strong></span>
            </button>
            <button class="session-row">
              <span class="session-indicator approval">${fontAwesomeIcon("triangle-exclamation")}</span>
              <span class="session-copy"><strong>Approval needed</strong></span>
            </button>
          </div>
          <div>
            <p class="gallery-label">Conversation hierarchy</p>
            <article class="message user-message"><p>Keep the current look, feel, and layout.</p></article>
            <article class="message assistant-message">
              <header><span class="status-dot running"></span><strong>trouve</strong></header>
              <p>The generated tokens remain authoritative.</p>
              <div class="agent-activity-timeline">
                <details class="tool-card tool-ok" open>
                  <summary>
                    <span class="activity-rail-disclosure ok" aria-hidden="true">
                      ${fontAwesomeIcon("caret-down", {
                        className: "activity-rail-disclosure-icon",
                      })}
                    </span>
                    <strong>theme parity</strong>
                    <small>5 palettes</small>
                    <span class="tool-inline-status ok" aria-hidden="true">
                      ${fontAwesomeIcon("check", { className: "tool-status-icon" })}
                    </span>
                  </summary>
                  <pre>generated CSS matches theme.rs</pre>
                </details>
              </div>
            </article>
          </div>
          <div>
            <p class="gallery-label">Code and state colors</p>
            <pre class="diff-lines"><span class="context">  const shell = "existing";</span><span class="addition">+ const frontend = "lit";</span><span class="gallery-deletion">- const frontend = "legacy";</span></pre>
            <div class="gallery-statuses"><span class="additions">Success</span><span class="deletions">Error</span><span class="gallery-warning">Attention</span></div>
          </div>
        </div>
      </section>
    `;
  }

  #renderHardWidgets() {
    const before = `import { LitElement } from "lit";\n\nexport class App extends LitElement {}`;
    const after = `import { ContextProvider } from "@lit/context";\nimport { LitElement } from "lit";\n\nexport class App extends LitElement {}`;
    return html`
      <section class="gallery-theme gallery-hard-widgets" data-theme="dark" aria-labelledby="hard-widgets">
        <header class="gallery-theme-header">
          <div>
            <p class="gallery-eyebrow">Product widgets</p>
            <h2 id="hard-widgets">Markdown, code, diff, and terminal</h2>
          </div>
        </header>
        <div class="gallery-widget-grid">
          <div class="gallery-widget">
            <p class="gallery-label">Sanitized streaming Markdown</p>
            <trouve-markdown-view
              .content=${"### Migration status\n\n- **Lit** shell connected\n- Session summaries resume by cursor\n\n`@lit/context` keeps scoped services stable."}
            ></trouve-markdown-view>
          </div>
          <div class="gallery-widget gallery-widget-tall">
            <p class="gallery-label">Read-only code</p>
            <trouve-code-view
              language="typescript"
              label="TypeScript fixture"
              .content=${after}
            ></trouve-code-view>
          </div>
          <div class="gallery-widget gallery-widget-wide gallery-widget-tall">
            <p class="gallery-label">Unified and split diff</p>
            <trouve-diff-view
              language="typescript"
              label="src/app.ts"
              .original=${before}
              .modified=${after}
            ></trouve-diff-view>
          </div>
          <div class="gallery-widget gallery-widget-wide gallery-widget-tall">
            <p class="gallery-label">Ephemeral terminal renderer</p>
            <trouve-terminal-view terminal-id="term-gallery" label="Terminal fixture"></trouve-terminal-view>
          </div>
        </div>
      </section>
    `;
  }
}

customElements.define("trouve-component-gallery", TrouveComponentGallery);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-component-gallery": TrouveComponentGallery;
  }
}

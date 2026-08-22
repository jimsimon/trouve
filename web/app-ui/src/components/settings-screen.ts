import { ContextConsumer } from "@lit/context";
import { html, LitElement, nothing } from "lit";

import {
  appServicesContext,
  appStoreContext,
  hostCapabilitiesContext,
} from "../contexts/app-contexts.js";
import {
  browserNotificationCapability,
  testBrowserNotification,
} from "../services/browser-notifications.js";
import {
  isThemePreference,
  THEME_NAMES,
} from "../services/theme-controller.js";
import { APPEARANCE_FONT_SIZES } from "../services/appearance-preferences.js";
import {
  HostClientError,
  type DesktopUpdateState,
} from "../services/host-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import "./cli-settings.js";
import "./local-model-settings.js";
import "./management-settings-panels.js";
import "./persona-settings-panel.js";
import "./provider-settings.js";

const SETTINGS_SECTIONS = [
  "general",
  "chat",
  "providers",
  "personas",
  "mcp",
  "integrations",
  "appearance",
  "notifications",
  "about",
] as const;
type SettingsSection = (typeof SETTINGS_SECTIONS)[number];

const SETTINGS_ALIASES: Readonly<Record<string, SettingsSection>> = {
  clis: "providers",
  modes: "personas",
  "modes-models": "personas",
  "local-models": "providers",
  "provider-api": "providers",
  "personas-models": "personas",
  "git-worktrees": "chat",
  workspaces: "chat",
  "mcp-servers": "mcp",
  capabilities: "about",
};

const settingsSection = (value: string): SettingsSection =>
  SETTINGS_SECTIONS.includes(value as SettingsSection)
    ? value as SettingsSection
    : SETTINGS_ALIASES[value] ?? "general";

const sectionLabel = (section: SettingsSection): string => {
  if (section === "chat") return "Sessions & Chat";
  if (section === "personas") return "Personas & Models";
  if (section === "mcp") return "MCP Servers";
  return `${section[0]?.toUpperCase()}${section.slice(1)}`;
};

const desktopUpdateIsBusy = (state: DesktopUpdateState | undefined): boolean =>
  state !== undefined
  && [
    "checking",
    "downloading",
    "verifying",
    "installing",
    "restarting",
  ].includes(state.phase);

const DESKTOP_UPDATE_BUSY_POLL_MS = 500;
const DESKTOP_UPDATE_IDLE_POLL_MS = 30_000;

export const desktopUpdatePollIntervalMs = (
  state: DesktopUpdateState | undefined,
): number => desktopUpdateIsBusy(state)
  ? DESKTOP_UPDATE_BUSY_POLL_MS
  : DESKTOP_UPDATE_IDLE_POLL_MS;

const desktopUpdatePhaseAnnouncement = (
  state: DesktopUpdateState | undefined,
  loading: boolean,
): string => state === undefined
  ? (loading ? "Loading update status." : "")
  : `Update ${state.phase}.`;

export const desktopUpdateCanRetryInstall = (state: DesktopUpdateState | undefined): boolean =>
  state?.phase === "error"
  && state.availableVersion !== undefined
  && (
    state.message.startsWith("Update installation failed:")
    || state.message.includes("is installed, but trouve could not restart:")
  );

export class TrouveSettingsScreen extends withSignalTracking(LitElement) {
  static override properties = {
    section: { type: String },
  };

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  section = "general";
  #notificationStatus = "";
  #notificationPending = false;
  #fontFamiliesRequested = false;
  #fontFamiliesLoading = false;
  #desktopUpdateState: DesktopUpdateState | undefined;
  #desktopUpdateLoading = false;
  #desktopUpdateActionPending = false;
  #desktopUpdateError = "";
  #desktopUpdatePollTimer: ReturnType<typeof setInterval> | undefined;
  #desktopUpdatePollIntervalMs: number | undefined;
  #desktopUpdateGeneration = 0;
  #desktopUpdateInitialAttempted = false;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #store = new ContextConsumer(this, {
    context: appStoreContext,
    subscribe: true,
  });
  readonly #capabilities = new ContextConsumer(this, {
    context: hostCapabilitiesContext,
    subscribe: true,
  });

  readonly #close = (): void => {
    this.dispatchEvent(new CustomEvent("trouve-close-full-screen", {
      bubbles: true,
      composed: true,
    }));
  };

  override connectedCallback(): void {
    super.connectedCallback();
    globalThis.addEventListener("focus", this.#refreshWebNotificationCapability);
    this.#desktopUpdateInitialAttempted = false;
    queueMicrotask(() => void this.#loadDesktopUpdate(true));
  }

  override disconnectedCallback(): void {
    globalThis.removeEventListener("focus", this.#refreshWebNotificationCapability);
    this.#desktopUpdateGeneration += 1;
    this.#desktopUpdateLoading = false;
    this.#desktopUpdateActionPending = false;
    this.#stopDesktopUpdatePolling();
    super.disconnectedCallback();
  }

  protected override updated(): void {
    if (settingsSection(this.section) === "appearance") {
      void this.#loadFontFamilies();
    }
    if (
      settingsSection(this.section) === "general"
      && this.#desktopUpdateState === undefined
      && !this.#desktopUpdateInitialAttempted
      && !this.#desktopUpdateLoading
    ) {
      void this.#loadDesktopUpdate(true);
    }
  }

  async #loadDesktopUpdate(silent: boolean): Promise<void> {
    const capabilities = this.#capabilities.value;
    const action = this.#services.value?.nativeHost?.getDesktopUpdate;
    if (
      capabilities === undefined
      || !readSignal(capabilities.current).selfUpdate
      || action === undefined
      || this.#desktopUpdateLoading
    ) return;
    const generation = this.#desktopUpdateGeneration;
    this.#desktopUpdateInitialAttempted = true;
    this.#desktopUpdateLoading = true;
    try {
      const state = await action();
      if (generation !== this.#desktopUpdateGeneration) return;
      this.#desktopUpdateState = state;
      this.#desktopUpdateError = "";
      this.#startDesktopUpdatePolling(desktopUpdatePollIntervalMs(state));
    } catch {
      if (
        generation === this.#desktopUpdateGeneration
        && (!silent || this.#desktopUpdateState === undefined)
      ) {
        this.#desktopUpdateError = "Update status could not be loaded.";
      }
      if (generation === this.#desktopUpdateGeneration) {
        this.#startDesktopUpdatePolling(DESKTOP_UPDATE_IDLE_POLL_MS);
      }
    } finally {
      if (generation !== this.#desktopUpdateGeneration) return;
      this.#desktopUpdateLoading = false;
      if (this.isConnected) this.requestUpdate();
    }
  }

  async #runDesktopUpdateAction(): Promise<void> {
    const nativeHost = this.#services.value?.nativeHost;
    const installing = this.#desktopUpdateState?.phase === "available"
      || desktopUpdateCanRetryInstall(this.#desktopUpdateState);
    const action = installing
      ? nativeHost?.installDesktopUpdate
      : nativeHost?.checkDesktopUpdate;
    if (action === undefined || this.#desktopUpdateActionPending) return;
    this.#desktopUpdateGeneration += 1;
    const actionGeneration = this.#desktopUpdateGeneration;
    this.#desktopUpdateActionPending = true;
    this.#desktopUpdateError = "";
    this.#startDesktopUpdatePolling(DESKTOP_UPDATE_BUSY_POLL_MS);
    this.requestUpdate();
    let keepPolling = false;
    let completionGeneration = actionGeneration;
    try {
      const state = await action();
      if (actionGeneration !== this.#desktopUpdateGeneration || !this.isConnected) return;
      this.#desktopUpdateGeneration += 1;
      completionGeneration = this.#desktopUpdateGeneration;
      this.#desktopUpdateState = state;
    } catch (error) {
      if (actionGeneration !== this.#desktopUpdateGeneration || !this.isConnected) return;
      if (error instanceof HostClientError && error.kind === "action-busy") {
        keepPolling = true;
      } else {
        this.#desktopUpdateError = installing
          ? "The update could not be installed. You can try again without leaving the app."
          : "The update check could not be completed.";
      }
    } finally {
      if (!this.isConnected || completionGeneration !== this.#desktopUpdateGeneration) return;
      this.#desktopUpdateActionPending = false;
      if (keepPolling || desktopUpdateIsBusy(this.#desktopUpdateState)) {
        this.#startDesktopUpdatePolling(DESKTOP_UPDATE_BUSY_POLL_MS);
        void this.#loadDesktopUpdate(true);
      } else {
        this.#startDesktopUpdatePolling(DESKTOP_UPDATE_IDLE_POLL_MS);
      }
      this.requestUpdate();
    }
  }

  #startDesktopUpdatePolling(intervalMs: number): void {
    if (!this.isConnected || this.#desktopUpdatePollIntervalMs === intervalMs) return;
    this.#stopDesktopUpdatePolling();
    this.#desktopUpdatePollIntervalMs = intervalMs;
    this.#desktopUpdatePollTimer = globalThis.setInterval(
      () => void this.#loadDesktopUpdate(true),
      intervalMs,
    );
  }

  #stopDesktopUpdatePolling(): void {
    if (this.#desktopUpdatePollTimer === undefined) return;
    globalThis.clearInterval(this.#desktopUpdatePollTimer);
    this.#desktopUpdatePollTimer = undefined;
    this.#desktopUpdatePollIntervalMs = undefined;
  }

  async #loadFontFamilies(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#fontFamiliesRequested) return;
    this.#fontFamiliesRequested = true;
    this.#fontFamiliesLoading = true;
    this.requestUpdate();
    try {
      await services.loadSystemFontFamilies();
    } catch {
      // Desktop bootstrap and browser Local Font Access failures both retain
      // a useful selector with the platform-default option.
    } finally {
      this.#fontFamiliesLoading = false;
      if (this.isConnected) this.requestUpdate();
    }
  }

  readonly #refreshWebNotificationCapability = (): void => {
    const services = this.#services.value;
    const capabilities = this.#capabilities.value;
    if (services === undefined || capabilities === undefined || services.deployment === "desktop") {
      return;
    }
    const current = readSignal(capabilities.current);
    const available = browserNotificationCapability(services.notifications);
    capabilities.updateWebNotifications(available);
    if (current.webNotifications && !available) {
      this.#notificationStatus = "Notification permission is no longer available.";
      this.requestUpdate();
    }
  };

  async #testWebNotification(): Promise<void> {
    const services = this.#services.value;
    const capabilities = this.#capabilities.value;
    if (
      services === undefined ||
      capabilities === undefined ||
      services.deployment === "desktop" ||
      this.#notificationPending
    ) return;
    this.#notificationPending = true;
    this.#notificationStatus = "Testing notification permission…";
    this.requestUpdate();
    try {
      const notificationResult = await testBrowserNotification(services.notifications);
      capabilities.updateWebNotifications(notificationResult.webNotifications);
      this.#notificationStatus = notificationResult.message;
    } catch {
      // The model catches browser API failures. Keep an outer payload-free
      // guard so an unexpected adapter failure cannot escape a click handler.
      this.#notificationStatus = "Notification test could not be completed.";
    } finally {
      this.#notificationPending = false;
      this.requestUpdate();
    }
  }

  async #testNativeNotification(): Promise<void> {
    const services = this.#services.value;
    const capabilities = this.#capabilities.value;
    const nativeNotification = services?.nativeHost?.showNativeNotification;
    if (
      services === undefined
      || capabilities === undefined
      || services.deployment !== "desktop"
      || !readSignal(capabilities.current).nativeNotifications
      || nativeNotification === undefined
      || this.#notificationPending
    ) return;
    this.#notificationPending = true;
    this.#notificationStatus = "Sending a native test notification…";
    this.requestUpdate();
    try {
      await nativeNotification({
        title: "Trouve notification test",
        body: "Native desktop notifications are working.",
        sound: readSignal(services.notificationPreferences).sound,
        sessionId: "settings-notification-test",
        threadId: undefined,
      });
      this.#notificationStatus = "Native test notification sent.";
    } catch {
      this.#notificationStatus = "The native test notification could not be sent.";
    } finally {
      this.#notificationPending = false;
      this.requestUpdate();
    }
  }

  override render() {
    const services = this.#services.value;
    const store = this.#store.value;
    const capabilities = this.#capabilities.value;
    if (services === undefined || store === undefined || capabilities === undefined) {
      return html`<div class="screen-empty" role="status">Loading settings…</div>`;
    }
    const active = settingsSection(this.section);
    const currentCapabilities = readSignal(capabilities.current);
    const preference = readSignal(services.theme.preference);
    const appearance = readSignal(services.appearance);
    const fontFamilies = readSignal(services.systemFontFamilies);
    const selectedFontUnavailable = appearance.fontFamily !== "" &&
      !fontFamilies.includes(appearance.fontFamily);
    const generalPreferences = readSignal(services.generalPreferences);
    const chatPreferences = readSignal(services.chatPreferences);
    const notificationPreferences = readSignal(services.notificationPreferences);
    const updatePhase = this.#desktopUpdateState?.phase;
    const updateBusy = this.#desktopUpdateActionPending
      || updatePhase === "checking"
      || updatePhase === "downloading"
      || updatePhase === "verifying"
      || updatePhase === "installing"
      || updatePhase === "restarting";
    const routeSection = this.section;
    return html`
      <div class="settings-screen">
        <header class="full-screen-header">
          <strong>Settings</strong>
          <button type="button" @click=${this.#close}>${fontAwesomeIcon("xmark")} Close</button>
        </header>
        <div class="settings-frame">
          <div class="settings-layout">
            <nav class="settings-nav" aria-label="Settings sections">
              ${SETTINGS_SECTIONS.map(
                (section) => html`
                  <button
                    type="button"
                    aria-current=${active === section ? "page" : "false"}
                    @click=${() => services.router.navigate({ kind: "settings", section })}
                  >${sectionLabel(section)}</button>
                `,
              )}
            </nav>
            <section class="settings-content" aria-label=${`${sectionLabel(active)} settings`}>
              ${active === "general"
                ? html`
                    <div class="settings-section">
                      <h1 id="settings-title">General</h1>
                      <label class="settings-check compact" for="settings-prevent-sleep">
                        <input
                          id="settings-prevent-sleep"
                          type="checkbox"
                          .checked=${generalPreferences.preventSleepWhileRunning}
                          @change=${(event: Event) => services.setGeneralPreferences({
                            preventSleepWhileRunning:
                              (event.currentTarget as HTMLInputElement).checked,
                          })}
                        />
                        <span>Prevent the computer from sleeping while agents are running</span>
                      </label>
                      <p class="settings-note">The display may still turn off, and manually requested sleep is not blocked.</p>
                      ${currentCapabilities.sleepInhibition
                        ? nothing
                        : html`<p class="settings-note capability-note">${services.deployment === "pwa"
                          ? "Wake lock depends on browser and device support in the installed PWA."
                          : "This preview host does not currently expose sleep inhibition; the preference is retained."}</p>`}
                      ${services.deployment === "desktop"
                        ? html`
                            <div class="settings-subsection">
                              <h2>Updates</h2>
                              <label class="settings-check compact" for="settings-automatic-updates">
                                <input
                                  id="settings-automatic-updates"
                                  type="checkbox"
                                  .checked=${generalPreferences.automaticUpdates}
                                  ?disabled=${!currentCapabilities.selfUpdate}
                                  @change=${(event: Event) => services.setGeneralPreferences({
                                    automaticUpdates:
                                      (event.currentTarget as HTMLInputElement).checked,
                                  })}
                                />
                                <span>Automatically update before opening trouve</span>
                              </label>
                              <p class="settings-note">When enabled, startup checks, downloads, verifies, and installs a release before the main window opens. Updates found while you are using trouve wait for this button or a later restart.</p>
                              ${currentCapabilities.selfUpdate
                                ? html`
                                    <section class="desktop-update-card" aria-label="Desktop update status">
                                      <p class="visually-hidden" role="status" aria-live="polite" aria-atomic="true">
                                        ${desktopUpdatePhaseAnnouncement(
                                          this.#desktopUpdateState,
                                          this.#desktopUpdateLoading,
                                        )}
                                      </p>
                                      ${this.#desktopUpdateState?.phase === "error"
                                        ? html`<p class="visually-hidden" role="alert">${this.#desktopUpdateState.message}</p>`
                                        : nothing}
                                      <div class="desktop-update-heading">
                                        <strong>${this.#desktopUpdateState?.message
                                          ?? (this.#desktopUpdateLoading ? "Loading update status…" : "Ready to check for updates")}</strong>
                                        <span class="desktop-update-version">v${this.#desktopUpdateState?.currentVersion ?? __TROUVE_FRONTEND_VERSION__}</span>
                                      </div>
                                      ${this.#desktopUpdateState?.progressPercent === undefined
                                        ? nothing
                                        : html`<progress
                                            max="100"
                                            .value=${this.#desktopUpdateState.progressPercent}
                                            aria-label="Update progress"
                                          ></progress>`}
                                      ${this.#desktopUpdateError === ""
                                        ? nothing
                                        : html`<p class="settings-note" role="alert">${this.#desktopUpdateError}</p>`}
                                      <div class="settings-actions">
                                        <button
                                          type="button"
                                          ?disabled=${updateBusy || this.#desktopUpdateLoading}
                                          @click=${() => void this.#runDesktopUpdateAction()}
                                        >${updatePhase === "available"
                                            ? `Install v${this.#desktopUpdateState?.availableVersion ?? "update"} and restart`
                                            : updateBusy
                                              ? "Updating…"
                                              : updatePhase === "error"
                                                ? "Try again"
                                                : "Check for updates"}</button>
                                      </div>
                                    </section>
                                  `
                                : html`<p class="settings-note capability-note">Self-update is unavailable in development, web-preview, and package-managed installations. The automatic-update preference is retained for a supported packaged release.</p>`}
                            </div>
                          `
                        : nothing}
                    </div>
                  `
                : active === "providers"
                  ? html`
                      <div class="settings-provider-shell">
                        <header class="settings-provider-header">
                          <h1 id="settings-title">Providers</h1>
                        </header>
                        <nav class="settings-subnav" aria-label="Provider settings">
                          ${([
                            ["providers", "Subscriptions"],
                            ["provider-api", "API"],
                            ["local-models", "Local"],
                          ] as const).map(([section, label]) => html`
                            <button
                              type="button"
                              aria-current=${routeSection === section || (section === "providers" && !["provider-api", "local-models"].includes(routeSection)) ? "page" : "false"}
                              @click=${() => services.router.navigate({ kind: "settings", section })}
                            >${label}</button>
                          `)}
                        </nav>
                        ${routeSection === "local-models"
                            ? html`<trouve-local-model-settings></trouve-local-model-settings>`
                            : html`<trouve-provider-settings
                                provider-category=${routeSection === "provider-api" ? "api" : "subscription"}
                                .showHeading=${false}
                              ></trouve-provider-settings>`}
                      </div>
                    `
                : active === "chat"
                  ? html`
                      <div class="settings-section">
                        <h1 id="settings-title">Sessions &amp; Chat</h1>
                        <div class="settings-subsection">
                          <h2>Chat output</h2>
                          <label class="settings-toggle-row" for="settings-collapse-sequential-tools">
                            <input
                              id="settings-collapse-sequential-tools"
                              type="checkbox"
                              .checked=${chatPreferences.collapseSequentialToolCalls}
                              @change=${(event: Event) => services.setChatPreferences({
                                collapseSequentialToolCalls:
                                  (event.currentTarget as HTMLInputElement).checked,
                              })}
                            />
                            <span class="toggle-state">${chatPreferences.collapseSequentialToolCalls ? "On" : "Off"}</span>
                            <span>Collapse sequential tool calls.</span>
                          </label>
                          <p class="settings-note">When off, every tool call is shown separately; thinking, context compaction, and TODO updates remain visible at the top level.</p>
                          <div class=${`nested-toggles ${chatPreferences.collapseSequentialToolCalls ? "" : "disabled"}`}>
                            <label class="settings-toggle-row" for="settings-collapse-thinking">
                              <input
                                id="settings-collapse-thinking"
                                type="checkbox"
                                .checked=${chatPreferences.collapseThinkingWithTools}
                                ?disabled=${!chatPreferences.collapseSequentialToolCalls}
                                @change=${(event: Event) => services.setChatPreferences({
                                  collapseThinkingWithTools:
                                    (event.currentTarget as HTMLInputElement).checked,
                                })}
                              />
                              <span class="toggle-state">${chatPreferences.collapseThinkingWithTools ? "On" : "Off"}</span>
                              <span>Collapse thinking output with tool calls.</span>
                            </label>
                            <p class="settings-note">When off, thought output stays visible at the top level and separates the collapsible tool-call groups on either side.</p>
                            <label class="settings-toggle-row" for="settings-collapse-compaction">
                              <input
                                id="settings-collapse-compaction"
                                type="checkbox"
                                .checked=${chatPreferences.collapseCompactionWithTools}
                                ?disabled=${!chatPreferences.collapseSequentialToolCalls}
                                @change=${(event: Event) => services.setChatPreferences({
                                  collapseCompactionWithTools:
                                    (event.currentTarget as HTMLInputElement).checked,
                                })}
                              />
                              <span class="toggle-state">${chatPreferences.collapseCompactionWithTools ? "On" : "Off"}</span>
                              <span>Collapse context compaction with tool calls.</span>
                            </label>
                            <p class="settings-note">When off, context compaction remains a visible top-level boundary and separates the collapsible tool-call groups on either side.</p>
                            <label class="settings-toggle-row" for="settings-collapse-todos">
                              <input
                                id="settings-collapse-todos"
                                type="checkbox"
                                .checked=${chatPreferences.collapseTodoUpdatesWithTools}
                                ?disabled=${!chatPreferences.collapseSequentialToolCalls}
                                @change=${(event: Event) => services.setChatPreferences({
                                  collapseTodoUpdatesWithTools:
                                    (event.currentTarget as HTMLInputElement).checked,
                                })}
                              />
                              <span class="toggle-state">${chatPreferences.collapseTodoUpdatesWithTools ? "On" : "Off"}</span>
                              <span>Collapse TODO updates with tool calls.</span>
                            </label>
                            <p class="settings-note">When off, started, completed, cancelled, and skipped TODO updates stay visible on the turn rail and separate collapsible tool-call groups.</p>
                          </div>
                        </div>
                        <trouve-git-worktree-settings></trouve-git-worktree-settings>
                      </div>
                    `
                : active === "personas"
                  ? html`<trouve-persona-settings></trouve-persona-settings>`
                : active === "mcp"
                  ? html`<trouve-mcp-settings></trouve-mcp-settings>`
                : active === "integrations"
                  ? html`<trouve-integrations-settings></trouve-integrations-settings>`
                : active === "appearance"
                  ? html`
                      <div class="settings-section">
                        <h1 id="settings-title">Appearance</h1>
                        <div class="settings-field">
                          <label for="settings-theme">Theme</label>
                          <select
                            id="settings-theme"
                            .value=${preference}
                            @change=${(event: Event) => {
                              const value = (event.currentTarget as HTMLSelectElement).value;
                              if (isThemePreference(value)) services.setThemePreference(value);
                            }}
                          >
                            <option value="system">System</option>
                            ${THEME_NAMES.map(
                              (name) => html`<option value=${name}>${name.replaceAll("-", " ")}</option>`,
                            )}
                          </select>
                          <p>Every theme is contrast-checked as a whole (WCAG AA), so individual colors can't be overridden. The colorblind themes put success and errors on a blue/orange axis instead of green/red.</p>
                        </div>
                        <div class="settings-field-row">
                          <div class="settings-field font-size-field">
                            <label for="settings-font-size">Base font size</label>
                            <select
                              id="settings-font-size"
                              .value=${String(appearance.fontSize)}
                              @change=${(event: Event) => services.setAppearancePreferences({
                                fontSize: Number((event.currentTarget as HTMLSelectElement).value),
                              })}
                            >
                              ${APPEARANCE_FONT_SIZES.map(
                                (size) => html`<option value=${size}>${size}px</option>`,
                              )}
                            </select>
                          </div>
                          <div class="settings-field font-family-field">
                            <label for="settings-font-family">Font</label>
                            <select
                              id="settings-font-family"
                              aria-busy=${this.#fontFamiliesLoading ? "true" : "false"}
                              .value=${appearance.fontFamily}
                              @change=${(event: Event) => services.setAppearancePreferences({
                                fontFamily: (event.currentTarget as HTMLSelectElement).value,
                              })}
                            >
                              <option value="">System default</option>
                              ${selectedFontUnavailable
                                ? html`<option value=${appearance.fontFamily}>${appearance.fontFamily} (not installed)</option>`
                                : nothing}
                              ${fontFamilies.map(
                                (family) => html`<option value=${family}>${family}</option>`,
                              )}
                            </select>
                          </div>
                        </div>
                        <p class="settings-note">The size scales the whole interface; code keeps its monospace font.</p>
                        <label class="settings-toggle-row" for="settings-reduce-motion">
                          <input
                            id="settings-reduce-motion"
                            type="checkbox"
                            .checked=${appearance.reduceMotion}
                            @change=${(event: Event) => services.setAppearancePreferences({
                              reduceMotion: (event.currentTarget as HTMLInputElement).checked,
                            })}
                          />
                          <span class="toggle-state">${appearance.reduceMotion ? "On" : "Off"}</span>
                          <span>Reduce motion — replace spinners and other animation with static indicators.</span>
                        </label>
                      </div>
                    `
                : active === "notifications"
                  ? html`
                      <div class="settings-section">
                        <h1 id="settings-title">Notifications</h1>
                        <p class="settings-note">System notifications for agent activity you'd otherwise miss: they fire when the window is in the background or the event belongs to a session that isn't on screen.</p>
                        <label class="settings-toggle-row" for="settings-notify-enabled">
                          <input
                            id="settings-notify-enabled"
                            type="checkbox"
                            .checked=${notificationPreferences.enabled}
                            @change=${(event: Event) => services.setNotificationPreferences({
                              enabled: (event.currentTarget as HTMLInputElement).checked,
                            })}
                          />
                          <span class="toggle-state">${notificationPreferences.enabled ? "On" : "Off"}</span>
                          <span>Enable desktop notifications.</span>
                        </label>
                        ${notificationPreferences.enabled
                          ? html`<div class="nested-toggles">
                              ${[
                                ["settings-notify-finish", notificationPreferences.onFinish, "Agent finished — a turn completed.", "onFinish"],
                                ["settings-notify-fail", notificationPreferences.onFail, "Turn failed — a turn ended with an error.", "onFail"],
                                ["settings-notify-attention", notificationPreferences.onAttention, "Needs attention — the agent is waiting on an approval or a question.", "onAttention"],
                                ["settings-notify-sound", notificationPreferences.sound, "Play a sound with each notification.", "sound"],
                              ].map(([id, checked, label, key]) => html`
                                <label class="settings-toggle-row" for=${id as string}>
                                  <input
                                    id=${id as string}
                                    type="checkbox"
                                    .checked=${checked as boolean}
                                    @change=${(event: Event) => services.setNotificationPreferences({
                                      [key as string]: (event.currentTarget as HTMLInputElement).checked,
                                    })}
                                  />
                                  <span class="toggle-state">${checked ? "On" : "Off"}</span>
                                  <span>${label}</span>
                                </label>
                              `)}
                            </div>`
                          : nothing}
                        <div class="settings-actions">
                          ${services.deployment === "desktop"
                            ? html`<button type="button" @click=${() => void this.#testNativeNotification()} ?disabled=${!currentCapabilities.nativeNotifications || !notificationPreferences.enabled || this.#notificationPending}>${this.#notificationPending ? "Testing…" : "Send test notification"}</button>`
                            : html`<button type="button" @click=${() => void this.#testWebNotification()} ?disabled=${!currentCapabilities.webNotifications || !notificationPreferences.enabled || this.#notificationPending}>${this.#notificationPending ? "Testing…" : "Send test notification"}</button>`}
                          <span class="settings-note" role="status" aria-live="polite">${this.#notificationStatus}</span>
                        </div>
                        ${services.deployment === "desktop" && !currentCapabilities.nativeNotifications
                          ? html`<p class="settings-note capability-note">Native notifications are unavailable in this preview host.</p>`
                          : services.deployment !== "desktop" && !currentCapabilities.webNotifications
                            ? html`<p class="settings-note capability-note">Web notifications are unavailable or denied in this browser.</p>`
                            : nothing}
                      </div>
                    `
                : html`
                    ${routeSection === "capabilities"
                      ? html`
                          <div class="settings-section">
                            <h1 id="settings-title">Capabilities</h1>
                            <p class="settings-note">Unavailable operations are explicit; the PWA never pretends to have desktop filesystem or process access.</p>
                            <dl class="capability-grid">
                              ${[
                                ["Deployment", currentCapabilities.kind],
                                ["Bridge version", currentCapabilities.bridgeVersion ?? "Not applicable"],
                                ["Persistent preferences", currentCapabilities.persistentPreferences],
                                ["Native directory picker", currentCapabilities.directoryPicker],
                                ["Native file picker", currentCapabilities.filePicker],
                                ["Clipboard images", currentCapabilities.clipboardImage],
                                ["Lifecycle events", currentCapabilities.lifecycleEvents],
                                ["Close confirmation", currentCapabilities.closeConfirmation],
                                ["Open local files", currentCapabilities.openLocalFile],
                                ["Reveal local files", currentCapabilities.revealLocalFile],
                                ["Open HTTPS links", currentCapabilities.openHttpsUrl],
                                ["Native notifications", currentCapabilities.nativeNotifications],
                                ["Web notifications", currentCapabilities.webNotifications],
                                ["Request user attention", currentCapabilities.userAttention],
                                ["Sleep inhibition", currentCapabilities.sleepInhibition],
                                ["Window geometry", currentCapabilities.windowGeometry],
                                ["Visibility state", currentCapabilities.visibility],
                                ["Occlusion state", currentCapabilities.occlusion],
                                ["Installable", currentCapabilities.installable],
                                ["Self-update", currentCapabilities.selfUpdate],
                              ].map(
                                ([label, value]) => html`<div><dt>${label}</dt><dd>${typeof value === "boolean" ? (value ? "Available" : "Unavailable") : value}</dd></div>`,
                              )}
                            </dl>
                          </div>
                        `
                      : html`
                          <div class="settings-section settings-about">
                            <h1 id="settings-title">About</h1>
                            <p class="settings-note">Application details for this Trouve build.</p>
                            <section class="settings-about-card" aria-label="Trouve application details">
                              <img src="/icons/trouve-512.png" alt="" />
                              <div class="settings-about-copy">
                                <strong>trouve</strong>
                                <span>A protocol-first AI coding harness</span>
                              </div>
                              <dl>
                                <div>
                                  <dt>Version</dt>
                                  <dd>v${__TROUVE_FRONTEND_VERSION__}</dd>
                                </div>
                              </dl>
                            </section>
                          </div>
                        `}
                  `}
            </section>
          </div>
        </div>
      </div>
    `;
  }
}

customElements.define("trouve-settings-screen", TrouveSettingsScreen);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-settings-screen": TrouveSettingsScreen;
  }
}

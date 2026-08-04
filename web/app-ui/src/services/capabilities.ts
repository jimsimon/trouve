import { createSignal, type ReadonlySignal } from "../state/reactivity.js";
import {
  browserNotificationCapability,
  createBrowserNotificationAdapter,
  type BrowserNotificationAdapter,
} from "./browser-notifications.js";
import { browserWakeLockCapability } from "./browser-wake-lock.js";

export type HostKind = "desktop" | "pwa" | "browser";

export interface HostCapabilities {
  readonly kind: HostKind;
  readonly bridgeVersion?: number;
  readonly directoryPicker: boolean;
  readonly filePicker: boolean;
  readonly clipboardImage: boolean;
  readonly lifecycleEvents: boolean;
  readonly closeConfirmation: boolean;
  readonly openLocalFile: boolean;
  readonly revealLocalFile: boolean;
  readonly openHttpsUrl: boolean;
  readonly nativeNotifications: boolean;
  readonly webNotifications: boolean;
  readonly userAttention: boolean;
  readonly sleepInhibition: boolean;
  readonly windowGeometry: boolean;
  readonly visibility: boolean;
  readonly occlusion: boolean;
  readonly persistentPreferences: boolean;
  readonly installable: boolean;
}

export const browserCapabilities = (
  kind: HostKind,
  notifications: BrowserNotificationAdapter = createBrowserNotificationAdapter(),
): HostCapabilities =>
  Object.freeze({
    kind,
    directoryPicker: false,
    // Standard <input type=file> attachments remain available independently;
    // this capability means an explicit native host picker bridge.
    filePicker: false,
    clipboardImage: false,
    lifecycleEvents: false,
    closeConfirmation: false,
    openLocalFile: false,
    revealLocalFile: false,
    openHttpsUrl: kind !== "desktop",
    nativeNotifications: false,
    webNotifications:
      kind !== "desktop" && browserNotificationCapability(notifications),
    userAttention: false,
    sleepInhibition: kind === "pwa" && browserWakeLockCapability(),
    windowGeometry: false,
    visibility: kind !== "desktop",
    occlusion: false,
    persistentPreferences: kind !== "desktop",
    installable: kind === "pwa",
  });

export class HostCapabilitiesController {
  readonly #current: ReturnType<typeof createSignal<HostCapabilities>>;
  readonly current: ReadonlySignal<HostCapabilities>;

  constructor(initial: HostCapabilities) {
    this.#current = createSignal(initial);
    this.current = this.#current;
  }

  update(capabilities: HostCapabilities): void {
    this.#current.set(Object.freeze(capabilities));
  }

  updateWebNotifications(available: boolean): void {
    const current = this.#current.get();
    const webNotifications = current.kind === "desktop" ? false : available;
    if (current.webNotifications === webNotifications) return;
    this.update({ ...current, webNotifications });
  }
}

import { afterEach, describe, expect, it, vi } from "vitest";

import type { BrowserNotificationAdapter } from "./browser-notifications.js";
import {
  browserCapabilities,
  HostCapabilitiesController,
} from "./capabilities.js";

const notificationPermission = (
  permission: NotificationPermission,
): BrowserNotificationAdapter => ({
  permission: () => permission,
  requestPermission: vi.fn(async () => permission),
  show: vi.fn(),
});

describe("browser capabilities", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("does not claim desktop-native operations for the PWA", () => {
    const capabilities = browserCapabilities("pwa");
    expect(capabilities.installable).toBe(true);
    expect(capabilities.directoryPicker).toBe(false);
    expect(capabilities.lifecycleEvents).toBe(false);
    expect(capabilities.closeConfirmation).toBe(false);
    expect(capabilities.openLocalFile).toBe(false);
    expect(capabilities.revealLocalFile).toBe(false);
    expect(capabilities.sleepInhibition).toBe(false);
  });

  it("does not advertise web notifications after permission is denied", () => {
    expect(browserCapabilities("pwa", notificationPermission("denied")).webNotifications).toBe(false);
  });

  it("never advertises web notifications for the desktop deployment", () => {
    expect(browserCapabilities("desktop", notificationPermission("granted")).webNotifications).toBe(false);
  });

  it("updates a browser capability after permission changes", () => {
    const controller = new HostCapabilitiesController(
      browserCapabilities("pwa", notificationPermission("granted")),
    );
    controller.updateWebNotifications(false);
    expect(controller.current.get().webNotifications).toBe(false);

    controller.updateWebNotifications(true);
    expect(controller.current.get().webNotifications).toBe(true);
  });

  it("does not enable browser notifications on a desktop controller", () => {
    const controller = new HostCapabilitiesController(
      browserCapabilities("desktop", notificationPermission("granted")),
    );
    controller.updateWebNotifications(true);
    expect(controller.current.get().webNotifications).toBe(false);
  });
});

import { describe, expect, it, vi } from "vitest";

import {
  browserNotificationCapability,
  createBrowserNotificationAdapter,
  testBrowserNotification,
  type BrowserNotificationAdapter,
} from "./browser-notifications.js";

const notifications = (
  overrides: Partial<BrowserNotificationAdapter> = {},
): BrowserNotificationAdapter => ({
  permission: () => "granted",
  requestPermission: vi.fn(async (): Promise<NotificationPermission> => "granted"),
  show: vi.fn(),
  ...overrides,
});

describe("browser notification adapter", () => {
  it("wraps the browser constructor behind the narrow adapter", async () => {
    const shown: Array<{ title: string; body: string | undefined }> = [];
    class FakeNotification {
      static permission: NotificationPermission = "granted";
      static requestPermission = vi.fn(async (): Promise<NotificationPermission> => "granted");

      constructor(title: string, options?: NotificationOptions) {
        shown.push({ title, body: options?.body });
      }
    }
    const adapter = createBrowserNotificationAdapter({ Notification: FakeNotification });

    expect(adapter.permission()).toBe("granted");
    await expect(adapter.requestPermission()).resolves.toBe("granted");
    adapter.show("Test", { body: "Body" });
    expect(shown).toEqual([{ title: "Test", body: "Body" }]);
  });

  it("reports an absent browser API without throwing", () => {
    const adapter = createBrowserNotificationAdapter({});
    expect(adapter.permission()).toBe("unsupported");
    expect(browserNotificationCapability(adapter)).toBe(false);
  });
});

describe("browser notification test workflow", () => {
  it("reports unsupported browsers without requesting permission", async () => {
    const requestPermission = vi.fn(async (): Promise<NotificationPermission> => "granted");
    const show = vi.fn();

    const outcome = await testBrowserNotification(notifications({
      permission: () => "unsupported",
      requestPermission,
      show,
    }));

    expect(outcome).toEqual({
      state: "unsupported",
      message: "Web notifications are unavailable in this browser.",
      webNotifications: false,
    });
    expect(requestPermission).not.toHaveBeenCalled();
    expect(show).not.toHaveBeenCalled();
  });

  it("does not prompt again after permission is denied", async () => {
    const requestPermission = vi.fn(async (): Promise<NotificationPermission> => "granted");

    const outcome = await testBrowserNotification(notifications({
      permission: () => "denied",
      requestPermission,
    }));

    expect(outcome.state).toBe("permission-denied");
    expect(outcome.message).toBe("Notification permission was not granted.");
    expect(outcome.webNotifications).toBe(false);
    expect(requestPermission).not.toHaveBeenCalled();
  });

  it("requests default permission and sends one foreground test", async () => {
    const requestPermission = vi.fn(async (): Promise<NotificationPermission> => "granted");
    const show = vi.fn();

    const outcome = await testBrowserNotification(notifications({
      permission: () => "default",
      requestPermission,
      show,
    }));

    expect(outcome).toEqual({
      state: "sent",
      message: "Test notification sent.",
      webNotifications: true,
    });
    expect(requestPermission).toHaveBeenCalledOnce();
    expect(show).toHaveBeenCalledWith("Test notification", {
      body: "Notifications are available in this PWA/browser preview.",
    });
  });

  it("records a denied permission request as unavailable", async () => {
    const show = vi.fn();
    const outcome = await testBrowserNotification(notifications({
      permission: () => "default",
      requestPermission: vi.fn(async (): Promise<NotificationPermission> => "denied"),
      show,
    }));

    expect(outcome.state).toBe("permission-denied");
    expect(outcome.webNotifications).toBe(false);
    expect(show).not.toHaveBeenCalled();
  });

  it("uses a generic failure when the permission request throws", async () => {
    const outcome = await testBrowserNotification(notifications({
      permission: () => "default",
      requestPermission: vi.fn(async () => {
        throw new Error("private browser policy detail");
      }),
    }));

    expect(outcome).toEqual({
      state: "failed",
      message: "Notification test could not be completed.",
      webNotifications: true,
    });
  });

  it("uses a generic failure when constructing the notification throws", async () => {
    const outcome = await testBrowserNotification(notifications({
      show: () => {
        throw new Error("private platform detail");
      },
    }));

    expect(outcome).toEqual({
      state: "failed",
      message: "Notification test could not be completed.",
      webNotifications: true,
    });
  });

  it("handles permission access exceptions conservatively", async () => {
    const adapter = notifications({
      permission: () => {
        throw new Error("private getter detail");
      },
    });

    expect(browserNotificationCapability(adapter)).toBe(false);
    await expect(testBrowserNotification(adapter)).resolves.toEqual({
      state: "failed",
      message: "Notification test could not be completed.",
      webNotifications: false,
    });
  });

  it("detects permission revoked after the initial capability snapshot", async () => {
    let permission: NotificationPermission = "granted";
    const adapter = notifications({ permission: () => permission });
    expect(browserNotificationCapability(adapter)).toBe(true);

    permission = "denied";

    expect(browserNotificationCapability(adapter)).toBe(false);
    const outcome = await testBrowserNotification(adapter);
    expect(outcome.state).toBe("permission-denied");
    expect(outcome.webNotifications).toBe(false);
  });
});

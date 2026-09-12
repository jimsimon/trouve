export type BrowserNotificationPermission = NotificationPermission | "unsupported";

export interface BrowserNotificationAdapter {
  permission(): BrowserNotificationPermission;
  requestPermission(): Promise<NotificationPermission>;
  show(
    title: string,
    options: NotificationOptions,
    onActivate?: () => void,
  ): void;
}

interface BrowserNotificationInstance {
  onclick?: ((event: Event) => void) | null;
  close?(): void;
}

interface BrowserNotificationConstructor {
  readonly permission: NotificationPermission;
  requestPermission(): Promise<NotificationPermission>;
  new(title: string, options?: NotificationOptions): BrowserNotificationInstance;
}

export interface BrowserNotificationEnvironment {
  readonly Notification?: BrowserNotificationConstructor;
}

export type BrowserNotificationTestState =
  | "sent"
  | "unsupported"
  | "permission-denied"
  | "failed";

export interface BrowserNotificationTestResult {
  readonly state: BrowserNotificationTestState;
  readonly message: string;
  /** Whether the current browser permission still makes the workflow available.
   * This deliberately says nothing about reliable background delivery. */
  readonly webNotifications: boolean;
}

const isNotificationPermission = (value: unknown): value is NotificationPermission =>
  value === "default" || value === "denied" || value === "granted";

const notificationConstructor = (
  environment: BrowserNotificationEnvironment,
): BrowserNotificationConstructor | undefined => {
  const candidate = environment.Notification;
  return typeof candidate === "function" ? candidate : undefined;
};

/** A narrow browser adapter so permission and constructor behavior can be
 * exercised without replacing globals in settings tests. */
export const createBrowserNotificationAdapter = (
  environment: BrowserNotificationEnvironment = globalThis,
): BrowserNotificationAdapter => ({
  permission: () => {
    const api = notificationConstructor(environment);
    if (api === undefined) return "unsupported";
    const permission: unknown = api.permission;
    if (!isNotificationPermission(permission)) {
      throw new Error("invalid browser notification permission");
    }
    return permission;
  },
  requestPermission: async () => {
    const api = notificationConstructor(environment);
    if (api === undefined) throw new Error("browser notifications unavailable");
    const permission: unknown = await api.requestPermission();
    if (!isNotificationPermission(permission)) {
      throw new Error("invalid browser notification permission");
    }
    return permission;
  },
  show: (title, options, onActivate) => {
    const api = notificationConstructor(environment);
    if (api === undefined) throw new Error("browser notifications unavailable");
    const notification = new api(title, options);
    if (onActivate !== undefined) {
      notification.onclick = () => {
        notification.close?.();
        onActivate();
      };
    }
  },
});

/** Conservative synchronous capability snapshot. An exception never becomes
 * an affirmative capability claim. */
export const browserNotificationCapability = (
  notifications: BrowserNotificationAdapter,
): boolean => {
  try {
    const permission = notifications.permission();
    return permission !== "unsupported" && permission !== "denied";
  } catch {
    return false;
  }
};

const result = (
  state: BrowserNotificationTestState,
  message: string,
  webNotifications: boolean,
): BrowserNotificationTestResult => Object.freeze({ state, message, webNotifications });

/** User-initiated foreground smoke test only. Service-worker push delivery and
 * background reliability remain separate publication work. */
export const testBrowserNotification = async (
  notifications: BrowserNotificationAdapter,
): Promise<BrowserNotificationTestResult> => {
  let permission: BrowserNotificationPermission;
  try {
    permission = notifications.permission();
  } catch {
    return result("failed", "Notification test could not be completed.", false);
  }

  if (permission === "unsupported") {
    return result(
      "unsupported",
      "Web notifications are unavailable in this browser.",
      false,
    );
  }
  if (permission === "denied") {
    return result(
      "permission-denied",
      "Notification permission was not granted.",
      false,
    );
  }

  if (permission === "default") {
    try {
      permission = await notifications.requestPermission();
    } catch {
      return result("failed", "Notification test could not be completed.", true);
    }
  }
  if (permission !== "granted") {
    return result(
      "permission-denied",
      "Notification permission was not granted.",
      false,
    );
  }

  try {
    notifications.show("Test notification", {
      body: "Notifications are available in this PWA/browser preview.",
    });
  } catch {
    return result("failed", "Notification test could not be completed.", true);
  }
  return result("sent", "Test notification sent.", true);
};

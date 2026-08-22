import { describe, expect, it, vi } from "vitest";

import {
  chatPreferencesFromHost,
  HostClient,
  HostClientError,
  generalPreferencesFromHost,
  notificationPreferencesFromHost,
  pullRequestGroupOrderFromHost,
  resumePreferencesFromHost,
  withHostChatPreferences,
  withHostGeneralPreferences,
  withHostNotificationPreferences,
  withHostPullRequestGroupOrder,
  withHostResumePreferences,
  withHostWorkspaceOrder,
  workspaceOrderFromHost,
} from "./host-client.js";
import type { HostPreferences } from "./host-client.js";

const validCapabilities = {
  bridge_version: 15,
  clipboard_image: true,
  close_confirmation: true,
  directory_picker: true,
  file_picker: true,
  installable: false,
  kind: "desktop",
  lifecycle_events: true,
  native_notifications: true,
  occlusion: true,
  open_https_url: true,
  open_local_file: true,
  persistent_preferences: true,
  reveal_local_file: true,
  self_update: true,
  sleep_inhibition: true,
  user_attention: true,
  visibility: true,
  web_notifications: false,
  window_geometry: true,
} as const;

const textAttachment = {
  name: "notes.txt",
  mime: "text/plain",
  data: "aGk=",
  size_bytes: 2,
} as const;

const imageAttachment = {
  name: "pasted-1.png",
  mime: "image/png",
  data: "cG5n",
  size_bytes: 3,
} as const;

const preferences: HostPreferences = {
  appearance: {
    font_family: "",
    font_size: 13,
    reduce_motion: false,
    theme: "dark",
  },
  geometry: null,
  inspection_width: 460,
  navigation_width: 260,
};

describe("HostClient", () => {
  it("runtime-validates bootstrap and maps the generated wire shape", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async () =>
      Response.json({
        capabilities: validCapabilities,
        font_families: ["Zed Sans", " Noto Sans ", "Noto Sans"],
        csrf_token: "a".repeat(64),
      }),
    );
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
    await expect(client.bootstrap()).resolves.toMatchObject({
      kind: "desktop",
      bridgeVersion: 15,
      directoryPicker: true,
      lifecycleEvents: true,
      selfUpdate: true,
    });
    expect(client.systemFontFamilies()).toEqual(["Noto Sans", "Zed Sans"]);
    expect(client.mutationHeaders()).toEqual({
      "x-trouve-host-csrf": "a".repeat(64),
    });
  });

  it("accepts legacy bootstrap payloads without self-update", async () => {
    const legacyCapabilities: Record<string, unknown> = {
      ...validCapabilities,
      bridge_version: 13,
    };
    delete legacyCapabilities.self_update;
    const fakeFetch = vi.fn<typeof fetch>(async () =>
      Response.json({
        capabilities: legacyCapabilities,
        csrf_token: "o".repeat(64),
      }),
    );
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);

    await expect(client.bootstrap()).resolves.toMatchObject({
      bridgeVersion: 13,
      selfUpdate: false,
    });
  });

  it("fails closed without including an invalid payload in diagnostics", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async () =>
      Response.json({ prompt: "repository secret", csrf_token: "bad" }),
    );
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
    const error = await client.bootstrap().catch((reason: unknown) => reason);
    expect(error).toBeInstanceOf(HostClientError);
    expect(String(error)).not.toContain("repository secret");
  });

  it("requires bootstrap and sends CSRF on preference writes", async () => {
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      requests.push(request);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({ capabilities: validCapabilities, csrf_token: "b".repeat(64) });
      }
      return Response.json(preferences);
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
    await expect(client.putPreferences(preferences)).rejects.toMatchObject({
      kind: "not-bootstrapped",
    });
    await client.bootstrap();
    await expect(client.putPreferences(preferences)).resolves.toEqual(preferences);
    expect(requests.at(-1)?.headers.get("x-trouve-host-csrf")).toBe("b".repeat(64));
  });

  it("reads update status and authenticates explicit update actions", async () => {
    const requests: Request[] = [];
    const update = {
      available_version: "4.1.0",
      current_version: "4.0.0",
      message: "Version 4.1.0 is ready to install.",
      phase: "available",
      progress_percent: null,
    } as const;
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      requests.push(request);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({ capabilities: validCapabilities, csrf_token: "u".repeat(64) });
      }
      return Response.json(update);
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
    await client.bootstrap();

    await expect(client.getDesktopUpdate()).resolves.toEqual({
      availableVersion: "4.1.0",
      currentVersion: "4.0.0",
      message: "Version 4.1.0 is ready to install.",
      phase: "available",
      progressPercent: undefined,
    });
    await client.checkDesktopUpdate();
    await client.installDesktopUpdate();

    expect(requests.at(-2)?.url).toContain("/update/check");
    expect(requests.at(-1)?.url).toContain("/update/install");
    expect(requests.at(-2)?.headers.get("x-trouve-host-csrf")).toBe("u".repeat(64));
    expect(requests.at(-1)?.headers.get("x-trouve-host-csrf")).toBe("u".repeat(64));
  });

  it("aborts a desktop update request that exceeds its deadline", async () => {
    vi.useFakeTimers();
    try {
      const fakeFetch = vi.fn<typeof fetch>(async (input) => {
        const request = input instanceof Request ? input : new Request(input);
        if (request.url.endsWith("/capabilities")) {
          return Response.json({
            capabilities: validCapabilities,
            csrf_token: "t".repeat(64),
          });
        }
        return await new Promise<Response>((_resolve, reject) => {
          request.signal.addEventListener(
            "abort",
            () => reject(request.signal.reason),
            { once: true },
          );
        });
      });
      const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
      await client.bootstrap();

      const assertion = expect(client.getDesktopUpdate()).rejects.toMatchObject({
        kind: "request-failed",
      });
      await vi.advanceTimersByTimeAsync(30_000);
      await assertion;
    } finally {
      vi.useRealTimers();
    }
  });

  it("bounds a stalled desktop install acknowledgement", async () => {
    vi.useFakeTimers();
    try {
      let installRequest: Request | undefined;
      const fakeFetch = vi.fn<typeof fetch>(async (input) => {
        const request = input instanceof Request ? input : new Request(input);
        if (request.url.endsWith("/capabilities")) {
          return Response.json({
            capabilities: validCapabilities,
            csrf_token: "i".repeat(64),
          });
        }
        installRequest = request;
        return await new Promise<Response>((_resolve, reject) => {
          request.signal.addEventListener(
            "abort",
            () => reject(request.signal.reason),
            { once: true },
          );
        });
      });
      const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
      await client.bootstrap();

      const installation = expect(client.installDesktopUpdate()).rejects.toMatchObject({
        kind: "request-failed",
      });
      await vi.advanceTimersByTimeAsync(30_000);
      expect(installRequest?.signal.aborted).toBe(true);
      await installation;
    } finally {
      vi.useRealTimers();
    }
  });

  it("gates close acknowledgement on the independently versioned bridge", async () => {
    const legacyRequests: Request[] = [];
    const legacyFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      legacyRequests.push(request);
      return Response.json({
        capabilities: { ...validCapabilities, bridge_version: 12 },
        csrf_token: "l".repeat(64),
      });
    });
    const legacy = new HostClient("http://127.0.0.1:43127", legacyFetch);
    await legacy.bootstrap();
    await expect(legacy.acknowledgeClose(7)).resolves.toBeUndefined();
    expect(legacyRequests).toHaveLength(1);

    const currentRequests: Request[] = [];
    const currentFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      currentRequests.push(request);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({
          capabilities: { ...validCapabilities, bridge_version: 13 },
          csrf_token: "n".repeat(64),
        });
      }
      return new Response(null, { status: 204 });
    });
    const current = new HostClient("http://127.0.0.1:43127", currentFetch);
    await current.bootstrap();
    await current.acknowledgeClose(8);

    expect(currentRequests.at(-1)?.url).toContain("/close-acknowledgement");
    await expect(currentRequests.at(-1)?.clone().json()).resolves.toEqual({ request_id: 8 });
    expect(currentRequests.at(-1)?.headers.get("x-trouve-host-csrf")).toBe("n".repeat(64));
  });

  it("serializes active preference writes and coalesces queued values", async () => {
    let releaseFirst!: () => void;
    const firstBlocked = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    let writes = 0;
    const writtenPreferences: HostPreferences[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({ capabilities: validCapabilities, csrf_token: "c".repeat(64) });
      }
      if (request.method === "GET") return Response.json(preferences);
      writes += 1;
      if (writes === 1) await firstBlocked;
      const { preferences: value } = await request.clone().json() as {
        preferences: HostPreferences;
      };
      writtenPreferences.push(value);
      return Response.json(value);
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
    await client.bootstrap();
    await client.getPreferences();

    const first = client.putPreferences(preferences);
    const second = client.putPreferences({
      ...preferences,
      navigation_width: 310,
    });
    const thirdValue = {
      ...preferences,
      navigation_width: 320,
    };
    const third = client.putPreferences(thirdValue);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(writes).toBe(1);
    releaseFirst();
    const saved = await Promise.all([first, second, third]);
    expect(saved[0]).toEqual(preferences);
    expect(saved[1]).toMatchObject(thirdValue);
    expect(saved[2]).toMatchObject(thirdValue);
    expect(writes).toBe(2);
    expect(writtenPreferences.map(({ navigation_width }) => navigation_width)).toEqual([
      preferences.navigation_width,
      320,
    ]);
  });

  it("rebases queued preference edits onto the active merged response", async () => {
    let releaseFirst!: () => void;
    const firstBlocked = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    let writes = 0;
    const written: HostPreferences[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({ capabilities: validCapabilities, csrf_token: "r".repeat(64) });
      }
      if (request.method === "GET") return Response.json(preferences);
      writes += 1;
      const { preferences: value } = await request.clone().json() as {
        preferences: HostPreferences;
      };
      written.push(value);
      if (writes === 1) {
        await firstBlocked;
        return Response.json({
          ...value,
          appearance: { ...value.appearance, font_size: 16 },
          resume: {
            ...value.resume,
            session_threads: { "se-external": "th-external" },
          },
        });
      }
      return Response.json(value);
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
    await client.bootstrap();
    await client.getPreferences();

    const first = client.putPreferences(preferences);
    const queued = client.putPreferences({
      ...preferences,
      appearance: { ...preferences.appearance, theme: "light" },
      resume: {
        ...preferences.resume,
        session_threads: { "se-local": "th-local" },
      },
      navigation_width: 320,
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    releaseFirst();
    await Promise.all([first, queued]);

    expect(written).toHaveLength(2);
    expect(written[1]).toMatchObject({
      appearance: { theme: "light", font_size: 16 },
      resume: {
        session_threads: {
          "se-external": "th-external",
          "se-local": "th-local",
        },
      },
      navigation_width: 320,
    });
  });

  it("refreshes and rebases queued intent after an ambiguous write failure", async () => {
    let releaseFirst!: () => void;
    const firstBlocked = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const written: HostPreferences[] = [];
    let putCount = 0;
    let getCount = 0;
    const latest: HostPreferences = {
      ...preferences,
      appearance: { ...preferences.appearance, font_size: 17 },
    };
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({ capabilities: validCapabilities, csrf_token: "f".repeat(64) });
      }
      if (request.method === "GET") {
        getCount += 1;
        return Response.json(getCount === 1 ? preferences : latest);
      }
      putCount += 1;
      const { preferences: value } = await request.clone().json() as {
        preferences: HostPreferences;
      };
      written.push(value);
      if (putCount === 1) {
        await firstBlocked;
        return new Response(null, { status: 502 });
      }
      return Response.json(value);
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
    await client.bootstrap();
    await client.getPreferences();

    const first = client.putPreferences(preferences);
    const queued = client.putPreferences({ ...preferences, navigation_width: 333 });
    releaseFirst();

    await expect(first).rejects.toMatchObject({ kind: "request-failed" });
    await expect(queued).resolves.toMatchObject({
      appearance: { font_size: 17 },
      navigation_width: 333,
    });
    expect(written[1]).toMatchObject({
      appearance: { font_size: 17 },
      navigation_width: 333,
    });
  });

  it("opens only validated HTTPS URLs through the CSRF-protected host action", async () => {
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      requests.push(request);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({ capabilities: validCapabilities, csrf_token: "d".repeat(64) });
      }
      return new Response(null, { status: 204 });
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);

    await expect(client.openHttpsUrl("https://example.com/docs")).rejects.toMatchObject({
      kind: "not-bootstrapped",
    });
    await client.bootstrap();
    await expect(
      client.openHttpsUrl("https://example.com/docs?q=1#start"),
    ).resolves.toBeUndefined();

    const request = requests.at(-1);
    expect(request).toBeDefined();
    expect(request?.method).toBe("POST");
    expect(request?.headers.get("x-trouve-host-csrf")).toBe("d".repeat(64));
    await expect(request!.clone().json()).resolves.toEqual({
      url: "https://example.com/docs?q=1#start",
    });

    const requestCount = requests.length;
    for (const unsafeUrl of [
      "http://example.com",
      "https://user:secret@example.com",
      "file:///tmp/secret",
      "https://example.com/\nsecret",
    ]) {
      await expect(client.openHttpsUrl(unsafeUrl)).rejects.toMatchObject({
        kind: "invalid-request",
      });
    }
    expect(requests).toHaveLength(requestCount);
  });

  it("picks a directory through the CSRF-protected host action and treats cancel normally", async () => {
    const requests: Request[] = [];
    const responses = [
      Response.json({ path: "/srv/repos/trouve" }),
      Response.json({ path: null }),
    ];
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      requests.push(request);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({ capabilities: validCapabilities, csrf_token: "f".repeat(64) });
      }
      return responses.shift() ?? new Response(null, { status: 500 });
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);

    await expect(client.pickDirectory()).rejects.toMatchObject({
      kind: "not-bootstrapped",
    });
    await client.bootstrap();
    await expect(client.pickDirectory()).resolves.toBe("/srv/repos/trouve");
    await expect(client.pickDirectory()).resolves.toBeUndefined();

    const pickerRequests = requests.filter((request) =>
      request.url.endsWith("/pick-directory"),
    );
    expect(pickerRequests).toHaveLength(2);
    expect(pickerRequests[0]?.method).toBe("POST");
    expect(pickerRequests[0]?.headers.get("x-trouve-host-csrf")).toBe(
      "f".repeat(64),
    );
  });

  it("fails closed for unavailable, busy, and malformed directory picker responses", async () => {
    const unavailableFetch = vi.fn<typeof fetch>(async () =>
      Response.json({
        capabilities: { ...validCapabilities, directory_picker: false },
        csrf_token: "g".repeat(64),
      }),
    );
    const unavailable = new HostClient(
      "http://127.0.0.1:43127",
      unavailableFetch,
    );
    await unavailable.bootstrap();
    await expect(unavailable.pickDirectory()).rejects.toMatchObject({
      kind: "capability-unavailable",
    });
    expect(unavailableFetch).toHaveBeenCalledTimes(1);

    const oldBridgeFetch = vi.fn<typeof fetch>(async () =>
      Response.json({
        capabilities: { ...validCapabilities, bridge_version: 2 },
        csrf_token: "i".repeat(64),
      }),
    );
    const oldBridge = new HostClient(
      "http://127.0.0.1:43127",
      oldBridgeFetch,
    );
    await expect(oldBridge.bootstrap()).resolves.toMatchObject({
      bridgeVersion: 2,
      directoryPicker: false,
    });
    await expect(oldBridge.pickDirectory()).rejects.toMatchObject({
      kind: "capability-unavailable",
    });
    expect(oldBridgeFetch).toHaveBeenCalledTimes(1);

    for (const [response, kind] of [
      [new Response(null, { status: 409 }), "action-busy"],
      [Response.json({ path: 42 }), "invalid-response"],
      [Response.json({ path: "/srv/repos/secret\npath" }), "invalid-response"],
    ] as const) {
      let bootstrapped = false;
      const fakeFetch = vi.fn<typeof fetch>(async () => {
        if (!bootstrapped) {
          bootstrapped = true;
          return Response.json({
            capabilities: validCapabilities,
            csrf_token: "h".repeat(64),
          });
        }
        return response;
      });
      const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
      await client.bootstrap();
      const error = await client.pickDirectory().catch((reason: unknown) => reason);
      expect(error).toMatchObject({ kind });
      expect(String(error)).not.toContain("secret");
    }
  });

  it("returns bounded native file-picker payloads with CSRF and normal cancellation", async () => {
    const requests: Request[] = [];
    const responses = [
      Response.json({ attachments: [textAttachment] }),
      Response.json({ attachments: [] }),
    ];
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      requests.push(request);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({
          capabilities: validCapabilities,
          csrf_token: "j".repeat(64),
        });
      }
      return responses.shift() ?? new Response(null, { status: 500 });
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);

    await expect(client.pickFiles()).rejects.toMatchObject({
      kind: "not-bootstrapped",
    });
    await client.bootstrap();
    await expect(client.pickFiles()).resolves.toEqual([
      {
        upload: {
          name: "notes.txt",
          mime: "text/plain",
          data: "aGk=",
        },
        size: 2,
      },
    ]);
    await expect(client.pickFiles()).resolves.toEqual([]);
    const actions = requests.filter((request) => request.url.endsWith("/pick-files"));
    expect(actions).toHaveLength(2);
    expect(actions[0]?.method).toBe("POST");
    expect(actions[0]?.headers.get("x-trouve-host-csrf")).toBe("j".repeat(64));
  });

  it("reads only validated native clipboard images and treats text/no-image as normal", async () => {
    const requests: Request[] = [];
    const responses = [
      Response.json({ attachment: imageAttachment }),
      Response.json({ attachment: null }),
    ];
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      requests.push(request);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({
          capabilities: validCapabilities,
          csrf_token: "k".repeat(64),
        });
      }
      return responses.shift() ?? new Response(null, { status: 500 });
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);

    await expect(client.readClipboardImage()).rejects.toMatchObject({
      kind: "not-bootstrapped",
    });
    await client.bootstrap();
    await expect(client.readClipboardImage()).resolves.toEqual({
      upload: {
        name: "pasted-1.png",
        mime: "image/png",
        data: "cG5n",
      },
      size: 3,
    });
    await expect(client.readClipboardImage()).resolves.toBeUndefined();
    const actions = requests.filter((request) =>
      request.url.endsWith("/read-clipboard-image"),
    );
    expect(actions).toHaveLength(2);
    expect(actions[0]?.headers.get("x-trouve-host-csrf")).toBe("k".repeat(64));
  });

  it("fails closed for stale bridges, busy actions, and malformed native attachments", async () => {
    const staleFetch = vi.fn<typeof fetch>(async () =>
      Response.json({
        capabilities: { ...validCapabilities, bridge_version: 3 },
        csrf_token: "l".repeat(64),
      }),
    );
    const stale = new HostClient("http://127.0.0.1:43127", staleFetch);
    await expect(stale.bootstrap()).resolves.toMatchObject({
      bridgeVersion: 3,
      directoryPicker: true,
      filePicker: false,
      clipboardImage: false,
      lifecycleEvents: false,
      closeConfirmation: false,
      nativeNotifications: false,
    });
    await expect(stale.pickFiles()).rejects.toMatchObject({
      kind: "capability-unavailable",
    });
    await expect(stale.readClipboardImage()).rejects.toMatchObject({
      kind: "capability-unavailable",
    });
    expect(staleFetch).toHaveBeenCalledTimes(1);

    for (const [path, response, action, kind] of [
      [
        "/pick-files",
        new Response(null, { status: 409 }),
        (client: HostClient) => client.pickFiles(),
        "action-busy",
      ],
      [
        "/pick-files",
        Response.json({
          attachments: [{ ...textAttachment, name: "secret/path.txt" }],
        }),
        (client: HostClient) => client.pickFiles(),
        "invalid-response",
      ],
      [
        "/pick-files",
        Response.json({
          attachments: [{ ...textAttachment, data: "not base64", size_bytes: 10 }],
        }),
        (client: HostClient) => client.pickFiles(),
        "invalid-response",
      ],
      [
        "/read-clipboard-image",
        Response.json({ attachment: textAttachment }),
        (client: HostClient) => client.readClipboardImage(),
        "invalid-response",
      ],
    ] as const) {
      let bootstrapped = false;
      const fakeFetch = vi.fn<typeof fetch>(async (input) => {
        const request = input instanceof Request ? input : new Request(input);
        if (!bootstrapped) {
          bootstrapped = true;
          return Response.json({
            capabilities: validCapabilities,
            csrf_token: "m".repeat(64),
          });
        }
        expect(request.url.endsWith(path)).toBe(true);
        return response;
      });
      const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
      await client.bootstrap();
      const error = await action(client).catch((reason: unknown) => reason);
      expect(error).toMatchObject({ kind });
      expect(String(error)).not.toContain("secret");
    }
  });

  it("polls validated lifecycle state and applies typed native actions with CSRF", async () => {
    const requests: Request[] = [];
    let notificationId = "";
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      requests.push(request);
      const url = new URL(request.url);
      if (url.pathname.endsWith("/capabilities")) {
        return Response.json({
          capabilities: validCapabilities,
          csrf_token: "n".repeat(64),
        });
      }
      if (url.pathname.endsWith("/native-notification")) {
        const body = await request.clone().json() as { notification_id: string };
        notificationId = body.notification_id;
        return new Response(null, { status: 204 });
      }
      if (url.pathname.endsWith("/lifecycle")) {
        return Response.json({
          cursor: 3,
          state: {
            focused: false,
            visible: true,
            occluded: false,
            pending_close: {
              request_id: 17,
              waiting_for_idle: false,
            },
          },
          events: [
            {
              cursor: 3,
              event: {
                type: "notification_activated",
                notification_id: notificationId,
                session_id: "se-1",
                thread_id: "th-1",
              },
            },
          ],
        });
      }
      return new Response(null, { status: 204 });
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
    const activate = vi.fn();
    await client.bootstrap();
    await client.showNativeNotification({
      title: "Approval needed",
      body: "Port the frontend",
      sound: false,
      sessionId: "se-1",
      threadId: "th-1",
    }, activate);
    await expect(client.pollLifecycle(0, 0)).resolves.toMatchObject({
      cursor: 3,
      state: {
        focused: false,
        pendingClose: { requestId: 17, waitingForIdle: false },
      },
    });
    expect(activate).toHaveBeenCalledOnce();

    await client.resolveClose(17, "quit_when_idle");
    await client.setSleepInhibition(true);
    await client.requestUserAttention();
    await client.actOnSessionFile("se-1", "src/main.rs", "reveal");

    const mutations = requests.filter((request) => request.method === "POST");
    expect(mutations).toHaveLength(5);
    expect(
      mutations.every(
        (request) => request.headers.get("x-trouve-host-csrf") === "n".repeat(64),
      ),
    ).toBe(true);
    await expect(
      mutations.find((request) => request.url.includes("/local-file-action"))!
        .clone()
        .json(),
    ).resolves.toEqual({
      session_id: "se-1",
      relative_path: "src/main.rs",
      action: "reveal",
    });
  });

  it("fails closed for malformed lifecycle, notification, and local-file requests", async () => {
    let calls = 0;
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      calls += 1;
      const request = input instanceof Request ? input : new Request(input);
      if (request.url.endsWith("/capabilities")) {
        return Response.json({
          capabilities: validCapabilities,
          csrf_token: "o".repeat(64),
        });
      }
      return Response.json({
        cursor: Number.MAX_SAFE_INTEGER + 1,
        state: {
          focused: true,
          visible: true,
          occluded: false,
          pending_close: null,
        },
        events: [],
      });
    });
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
    await client.bootstrap();
    await expect(client.pollLifecycle(0, 0)).rejects.toMatchObject({
      kind: "invalid-response",
    });
    const afterLifecycle = calls;
    await expect(client.showNativeNotification({
      title: "bad\0title",
      body: "",
      sound: false,
      sessionId: "se-1",
      threadId: undefined,
    })).rejects.toMatchObject({ kind: "invalid-request" });
    for (const path of ["../secret", "/tmp/secret", "src//main.rs", "src\\main.rs"]) {
      await expect(
        client.actOnSessionFile("se-1", path, "open"),
      ).rejects.toMatchObject({ kind: "invalid-request" });
    }
    expect(calls).toBe(afterLifecycle);
  });

  it("maps and updates all host-backed presentation preferences", () => {
    expect(generalPreferencesFromHost(preferences)).toEqual({
      preventSleepWhileRunning: true,
      automaticUpdates: true,
    });
    expect(chatPreferencesFromHost(preferences)).toEqual({
      collapseSequentialToolCalls: true,
      collapseThinkingWithTools: false,
      collapseCompactionWithTools: false,
      collapseTodoUpdatesWithTools: false,
    });
    expect(chatPreferencesFromHost(preferences, {
      collapseSequentialToolCalls: false,
      collapseThinkingWithTools: true,
      collapseCompactionWithTools: true,
      collapseTodoUpdatesWithTools: true,
    })).toEqual({
      collapseSequentialToolCalls: false,
      collapseThinkingWithTools: true,
      collapseCompactionWithTools: true,
      collapseTodoUpdatesWithTools: true,
    });
    expect(chatPreferencesFromHost({
      ...preferences,
      chat: {
        collapse_sequential_tool_calls: false,
        collapse_thinking_with_tools: false,
        collapse_compaction_with_tools: false,
        collapse_todo_updates_with_tools: false,
      },
    }, {
      collapseSequentialToolCalls: true,
      collapseThinkingWithTools: true,
      collapseCompactionWithTools: true,
      collapseTodoUpdatesWithTools: true,
    })).toEqual({
      collapseSequentialToolCalls: false,
      collapseThinkingWithTools: false,
      collapseCompactionWithTools: false,
      collapseTodoUpdatesWithTools: false,
    });
    expect(notificationPreferencesFromHost(preferences)).toEqual({
      enabled: true,
      onFinish: true,
      onFail: true,
      onAttention: true,
      sound: false,
    });
    let next = withHostGeneralPreferences(preferences, {
      preventSleepWhileRunning: false,
      automaticUpdates: false,
    });
    next = withHostChatPreferences(next, {
      collapseSequentialToolCalls: true,
      collapseThinkingWithTools: true,
      collapseCompactionWithTools: true,
      collapseTodoUpdatesWithTools: true,
    });
    next = withHostNotificationPreferences(next, {
      enabled: true,
      onFinish: false,
      onFail: true,
      onAttention: false,
      sound: true,
    });
    next = withHostWorkspaceOrder(next, ["ws-2", "ws-1", "ws-2", "bad id"]);
    next = withHostPullRequestGroupOrder(next, [
      "ready-to-merge",
      "drafts",
      "ready-to-merge",
      "Invalid Group",
    ]);
    next = withHostResumePreferences(next, {
      selectedSessionId: "se-1",
      sessionThreads: { "se-1": "th-1" },
      threadScroll: { "th-1": { itemId: "assistant:42", offset: 18.5 } },
      closedThreadTabs: ["th-2"],
      pinnedThreadTabs: ["th-1"],
    });
    expect(next.general?.prevent_sleep_while_running).toBe(false);
    expect(next.general?.automatic_updates).toBe(false);
    expect(next.chat?.collapse_sequential_tool_calls).toBe(true);
    expect(next.chat?.collapse_thinking_with_tools).toBe(true);
    expect(next.chat?.collapse_compaction_with_tools).toBe(true);
    expect(next.chat?.collapse_todo_updates_with_tools).toBe(true);
    expect(next.notifications).toMatchObject({
      on_finish: false,
      on_attention: false,
      sound: true,
    });
    expect(workspaceOrderFromHost(next)).toEqual(["ws-2", "ws-1"]);
    expect(pullRequestGroupOrderFromHost(next)).toEqual([
      "ready-to-merge",
      "drafts",
    ]);
    expect(resumePreferencesFromHost(next)).toEqual({
      selectedSessionId: "se-1",
      sessionThreads: { "se-1": "th-1" },
      threadScroll: { "th-1": { itemId: "assistant:42", offset: 18.5 } },
      closedThreadTabs: ["th-2"],
      pinnedThreadTabs: ["th-1"],
    });
    expect(next.resume?.closed_thread_tabs).toEqual(["th-2"]);
    expect(next.resume?.pinned_thread_tabs).toEqual(["th-1"]);
  });

  it("refuses an external URL action the host did not advertise", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async () =>
      Response.json({
        capabilities: { ...validCapabilities, open_https_url: false },
        csrf_token: "e".repeat(64),
      }),
    );
    const client = new HostClient("http://127.0.0.1:43127", fakeFetch);
    await client.bootstrap();
    await expect(client.openHttpsUrl("https://example.com")).rejects.toMatchObject({
      kind: "capability-unavailable",
    });
    expect(fakeFetch).toHaveBeenCalledTimes(1);
  });
});

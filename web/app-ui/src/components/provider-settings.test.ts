import { describe, expect, it, vi } from "vitest";

import type {
  ProtocolKnownProvider,
  ProtocolLoginStatus,
  ProtocolProviderInfo,
} from "../services/protocol-client.js";
import {
  ProviderLoginPoller,
  automaticRoutingProviders,
  movedProviderOrder,
  normalizedProviderOrder,
  providerSubmission,
  validatedHttpsUrl,
  type LoginPollScheduler,
} from "./provider-settings.js";

class ManualScheduler implements LoginPollScheduler {
  #nextId = 1;
  readonly tasks = new Map<number, () => void>();

  readonly set = (callback: () => void): number => {
    const id = this.#nextId++;
    this.tasks.set(id, callback);
    return id;
  };

  readonly clear = (handle: unknown): void => {
    this.tasks.delete(handle as number);
  };

  runNext(): void {
    const entry = this.tasks.entries().next().value as [number, () => void] | undefined;
    if (entry === undefined) throw new Error("no scheduled login poll");
    this.tasks.delete(entry[0]);
    entry[1]();
  }
}

const flush = async (): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
};

const preset: ProtocolKnownProvider = {
  id: "acme",
  display_name: "Acme AI",
  kind: "openai-compat",
  base_url: "https://api.acme.test/v1",
  api_key_env: "ACME_API_KEY",
  auth: "api-key",
  category: "api",
  experimental: false,
  config_fields: [
    { id: "TENANT", label: "Tenant", required: true, secret: false },
    { id: "TOKEN", label: "Tenant token", required: true, secret: true },
  ],
  headers: { "x-tenant": "${TENANT}", authorization: "Bearer ${TOKEN}" },
  query_params: { api_version: "2026-01-01" },
};

describe("provider settings security boundaries", () => {
  it("moves providers within a normalized complete preference order", () => {
    expect(movedProviderOrder(["codex", "stale"], ["openai", "codex", "cursor"], "cursor", -1))
      .toEqual(["codex", "cursor", "openai"]);
    expect(movedProviderOrder(["codex", "openai"], ["openai", "codex"], "codex", -1))
      .toEqual(["codex", "openai"]);
  });

  it("normalizes stale, duplicate, and omitted provider ids", () => {
    expect(normalizedProviderOrder(
      ["codex", "stale", "codex"],
      ["openai", "codex", "cursor"],
    )).toEqual(["codex", "openai", "cursor"]);
  });

  it("moves hosted providers without changing local-provider slots", () => {
    expect(movedProviderOrder(
      ["local", "codex", "loopback", "cursor"],
      ["local", "codex", "loopback", "cursor"],
      "cursor",
      -1,
      ["codex", "cursor"],
    )).toEqual(["local", "cursor", "loopback", "codex"]);
  });

  it("keeps managed local models and localhost APIs out of hosted priority", () => {
    const provider = (id: string, category: string): ProtocolProviderInfo => ({
      id,
      kind: "openai-compat",
      has_credentials: true,
      auth: "api-key",
      category,
    });
    expect(automaticRoutingProviders([
      provider("codex", "subscription"),
      provider("openai", "api"),
      provider("local", "local"),
      provider("ollama", "local"),
    ]).map(({ id }) => id)).toEqual(["codex", "openai"]);
  });

  it("accepts only HTTPS authorization URLs", () => {
    expect(validatedHttpsUrl("https://auth.example.test/device?flow=1")).toBe(
      "https://auth.example.test/device?flow=1",
    );
    expect(validatedHttpsUrl("http://auth.example.test/device")).toBeUndefined();
    expect(validatedHttpsUrl("javascript:alert(1)")).toBeUndefined();
    expect(validatedHttpsUrl("/relative-login")).toBeUndefined();
    expect(validatedHttpsUrl("https://user:secret@auth.example.test/device")).toBeUndefined();
    expect(validatedHttpsUrl("https://auth.example.test/device\nunsafe")).toBeUndefined();
  });

  it("separates write-only preset fields and omits blank credentials", () => {
    const withoutCredentials = providerSubmission(preset, {
      id: "ignored",
      kind: "ignored",
      baseUrl: "",
      apiKey: "",
      fields: { TENANT: "  team-one  ", TOKEN: "" },
    });
    expect(withoutCredentials).toEqual({
      id: "acme",
      request: {
        kind: "openai-compat",
        base_url: "https://api.acme.test/v1",
        settings: { TENANT: "team-one" },
        secret_values: {},
        headers: preset.headers,
        query_params: preset.query_params,
      },
    });
    expect(withoutCredentials.request).not.toHaveProperty("api_key");

    const withCredentials = providerSubmission(preset, {
      id: "ignored",
      kind: "ignored",
      baseUrl: "",
      apiKey: "api-key-with-significant-spacing ",
      fields: { TENANT: "team-one", TOKEN: " write-only-token " },
    });
    expect(withCredentials.request.api_key).toBe("api-key-with-significant-spacing ");
    expect(withCredentials.request.secret_values).toEqual({ TOKEN: " write-only-token " });
    expect(withCredentials.request.settings).toEqual({ TENANT: "team-one" });
  });
});

describe("ProviderLoginPoller", () => {
  it("does not overlap requests and stops after a terminal status", async () => {
    const scheduler = new ManualScheduler();
    let resolveFirst: ((status: ProtocolLoginStatus) => void) | undefined;
    const load = vi.fn(() => new Promise<ProtocolLoginStatus>((resolve) => {
      resolveFirst = resolve;
    }));
    const observed: string[] = [];
    const poller = new ProviderLoginPoller(load, scheduler, 1, 4);

    poller.start("acme", (status) => observed.push(status.status), vi.fn());
    expect(scheduler.tasks.size).toBe(1);
    scheduler.runNext();
    expect(load).toHaveBeenCalledTimes(1);
    expect(scheduler.tasks.size).toBe(0);

    resolveFirst?.({ status: "pending" });
    await flush();
    expect(observed).toEqual(["pending"]);
    expect(scheduler.tasks.size).toBe(1);

    load.mockResolvedValueOnce({ status: "success" });
    scheduler.runNext();
    await flush();
    expect(observed).toEqual(["pending", "success"]);
    expect(poller.active).toBe(false);
    expect(scheduler.tasks.size).toBe(0);
  });

  it("is bounded and clears scheduled work when stopped", async () => {
    const scheduler = new ManualScheduler();
    const exhausted = vi.fn();
    const load = vi.fn(async (): Promise<ProtocolLoginStatus> => ({ status: "pending" }));
    const poller = new ProviderLoginPoller(load, scheduler, 1, 2);

    poller.start("acme", vi.fn(), exhausted);
    scheduler.runNext();
    await flush();
    expect(scheduler.tasks.size).toBe(1);
    scheduler.runNext();
    await flush();
    expect(load).toHaveBeenCalledTimes(2);
    expect(exhausted).toHaveBeenCalledOnce();
    expect(poller.active).toBe(false);
    expect(scheduler.tasks.size).toBe(0);

    poller.start("acme", vi.fn(), exhausted);
    expect(scheduler.tasks.size).toBe(1);
    poller.stop();
    expect(scheduler.tasks.size).toBe(0);
    expect(poller.active).toBe(false);
  });

  it("ignores a status response that arrives after disposal", async () => {
    const scheduler = new ManualScheduler();
    let resolveStatus: ((status: ProtocolLoginStatus) => void) | undefined;
    const load = () => new Promise<ProtocolLoginStatus>((resolve) => { resolveStatus = resolve; });
    const onStatus = vi.fn();
    const poller = new ProviderLoginPoller(load, scheduler, 1, 3);

    poller.start("acme", onStatus, vi.fn());
    scheduler.runNext();
    poller.stop();
    resolveStatus?.({ status: "success" });
    await flush();
    expect(onStatus).not.toHaveBeenCalled();
    expect(scheduler.tasks.size).toBe(0);
  });
});

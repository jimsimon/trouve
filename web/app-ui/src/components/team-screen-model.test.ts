import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import type { ProtocolTeam } from "../services/protocol-client.js";
import { latestTeamSnapshot, TeamRefreshCoordinator } from "./team-screen-model.js";

const teamAt = (snapshotCursor: number): ProtocolTeam => ({
  session_id: "se_team",
  snapshot_cursor: snapshotCursor,
  goal: "Ship it",
  status: "active",
  orchestrator_member_id: "tm_orchestrator",
  members: [],
  messages: [],
  messages_truncated: false,
  max_turns: 8,
  turns_used: 0,
  created_at: "2026-08-22T12:00:00Z",
});

describe("team screen snapshots", () => {
  it("discards a refresh response older than the rendered team", () => {
    const current = teamAt(12);
    expect(latestTeamSnapshot(current, teamAt(9))).toBe(current);
    expect(latestTeamSnapshot(current, teamAt(13)).snapshot_cursor).toBe(13);
  });

  it("coalesces an event burst into at most one follow-up refresh", async () => {
    const coordinator = new TeamRefreshCoordinator();
    const completions: Array<() => void> = [];
    let calls = 0;
    const refresh = async (): Promise<void> => {
      calls += 1;
      await new Promise<void>((resolve) => completions.push(resolve));
    };

    coordinator.request(refresh);
    coordinator.request(refresh);
    await Promise.resolve();
    expect(calls).toBe(1);

    coordinator.request(refresh);
    coordinator.request(refresh);
    completions.shift()?.();
    await Promise.resolve();
    await Promise.resolve();
    expect(calls).toBe(2);

    completions.shift()?.();
    await Promise.resolve();
  });

  it("allows a reconnected lifecycle to refresh while the old request settles", async () => {
    const coordinator = new TeamRefreshCoordinator();
    let finishOld: (() => void) | undefined;
    let calls = 0;
    coordinator.request(async () => {
      calls += 1;
      await new Promise<void>((resolve) => {
        finishOld = resolve;
      });
    });
    await Promise.resolve();

    coordinator.reset();
    coordinator.request(async () => {
      calls += 1;
    });
    await Promise.resolve();
    expect(calls).toBe(2);
    finishOld?.();
    await Promise.resolve();
  });

  it("keeps the message composer explicitly named", () => {
    const source = readFileSync(new URL("./team-screen.ts", import.meta.url), "utf8");
    expect(source).toContain('aria-label="Team message"');
  });

  it("keeps failed initial loads recoverable", () => {
    const source = readFileSync(new URL("./team-screen.ts", import.meta.url), "utf8");
    expect(source).toContain("Retrying automatically.");
    expect(source).toContain("#scheduleLoadRetry()");
  });

  it("reopens its stream and preserves cursor ordering after lifecycle changes", () => {
    const source = readFileSync(new URL("./team-screen.ts", import.meta.url), "utf8");
    expect(source).toContain("override connectedCallback(): void");
    expect(source).toContain("this.#observedServices = undefined;");
    expect(source).toContain("latestTeamSnapshot(this.#team, team)");
  });
});

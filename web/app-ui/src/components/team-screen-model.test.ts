import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import type { ProtocolTeam } from "../services/protocol-client.js";
import { latestTeamSnapshot } from "./team-screen-model.js";

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

  it("keeps the message composer explicitly named", () => {
    const source = readFileSync(new URL("./team-screen.ts", import.meta.url), "utf8");
    expect(source).toContain('aria-label="Team message"');
  });

  it("keeps failed initial loads recoverable", () => {
    const source = readFileSync(new URL("./team-screen.ts", import.meta.url), "utf8");
    expect(source).toContain("Retrying automatically.");
    expect(source).toContain("#scheduleLoadRetry()");
  });
});

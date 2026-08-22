import type { ProtocolTeam } from "../services/protocol-client.js";

/** Keep a completed refresh from rolling the UI behind a newer snapshot. */
export const latestTeamSnapshot = (
  current: ProtocolTeam | undefined,
  incoming: ProtocolTeam,
): ProtocolTeam =>
  current !== undefined
    && (current.snapshot_cursor ?? 0) > (incoming.snapshot_cursor ?? 0)
    ? current
    : incoming;

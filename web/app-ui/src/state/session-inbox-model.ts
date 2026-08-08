export type InboxSessionAttention = "none" | "approval" | "question" | "both";
export type InboxSessionOutcome = "idle" | "running" | "succeeded" | "failed";

/** The projection fields that determine inbox placement and accessible status. */
export interface InboxSessionOrderFields {
  readonly id: string;
  readonly workspaceId: string;
  readonly archived: boolean;
  readonly active: boolean;
  readonly attention: InboxSessionAttention;
  readonly outcome: InboxSessionOutcome;
  /** Omitted for generic protocol-domain callers; false means a terminal
   * outcome has already been presented by this frontend. */
  readonly unread?: boolean;
  readonly updatedAt: string;
}

export interface WorkspaceSessionGroups<T extends InboxSessionOrderFields> {
  readonly active: readonly T[];
  readonly archived: readonly T[];
  readonly archivedExpanded: boolean;
}

const compareText = (left: string, right: string): number =>
  left < right ? -1 : left > right ? 1 : 0;

const outcomePriority = (session: InboxSessionOrderFields): number => {
  if (session.active || session.outcome === "running") return 1;
  if (session.outcome === "failed" && session.unread !== false) return 0;
  if (session.outcome === "idle") return 2;
  return 3;
};

/**
 * Inbox order mirrors the attention-first product model. Archived sessions are
 * a separate, recency-ordered history rather than competitors for attention.
 */
export const compareInboxSessions = (
  left: InboxSessionOrderFields,
  right: InboxSessionOrderFields,
): number => {
  if (left.archived !== right.archived) return left.archived ? 1 : -1;
  if (!left.archived) {
    const leftNeedsAttention = left.attention !== "none";
    const rightNeedsAttention = right.attention !== "none";
    if (leftNeedsAttention !== rightNeedsAttention) {
      return leftNeedsAttention ? -1 : 1;
    }
    const outcomeDifference = outcomePriority(left) - outcomePriority(right);
    if (outcomeDifference !== 0) return outcomeDifference;
  }
  const leftEpoch = Date.parse(left.updatedAt);
  const rightEpoch = Date.parse(right.updatedAt);
  const recencyDifference = Number.isFinite(leftEpoch) && Number.isFinite(rightEpoch)
    ? rightEpoch - leftEpoch
    : compareText(right.updatedAt, left.updatedAt);
  return recencyDifference || compareText(left.id, right.id);
};

export const sortInboxSessions = <T extends InboxSessionOrderFields>(
  sessions: readonly T[],
): readonly T[] => [...sessions].sort(compareInboxSessions);

/** Select the next visible session after returning to the inbox. */
export const inboxRecoverySession = <T extends InboxSessionOrderFields>(
  sessions: readonly T[],
): T | undefined => sortInboxSessions(sessions).find((session) => !session.archived);

export const groupWorkspaceSessions = <T extends InboxSessionOrderFields>(
  sessions: readonly T[],
  options: {
    /** Empty selects sessions from every workspace. */
    readonly workspaceId: string;
    readonly selectedSessionId: string | undefined;
    readonly archivedExpanded: boolean;
  },
): WorkspaceSessionGroups<T> => {
  const workspaceSessions = sessions.filter(
    (session) => options.workspaceId === "" || session.workspaceId === options.workspaceId,
  );
  const active = sortInboxSessions(
    workspaceSessions.filter((session) => !session.archived),
  );
  const archived = sortInboxSessions(
    workspaceSessions.filter((session) => session.archived),
  );
  const selectedArchived = archived.some(
    (session) => session.id === options.selectedSessionId,
  );
  return {
    active,
    archived,
    archivedExpanded:
      archived.length > 0 && (options.archivedExpanded || selectedArchived),
  };
};

export const sessionStatusText = (session: InboxSessionOrderFields): string => {
  if (session.attention === "approval") return "Needs attention: approval required";
  if (session.attention === "question") return "Needs attention: question awaiting answer";
  if (session.attention === "both") return "Needs attention: approval and question";
  if (session.active || session.outcome === "running") return "Running";
  if (session.outcome === "failed" && session.unread !== false) return "Failed";
  if (session.outcome === "succeeded" && session.unread !== false) return "Completed";
  return "Idle";
};

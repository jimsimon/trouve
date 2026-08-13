export interface ThreadTitleSource {
  readonly id: string;
  readonly mode: string;
  readonly model: string;
  readonly title?: string | null;
  readonly spawned?: boolean;
}

const shortModelName = (model: string): string => {
  const segments = model.split("/").filter((segment) => segment !== "");
  return segments.at(-1) ?? model;
};

const cleanTitle = (title: string | null | undefined): string | undefined => {
  const cleaned = title?.trim();
  return cleaned === undefined || cleaned === "" ? undefined : cleaned;
};

/** Resolves the durable navigation title while retaining useful fallbacks for
 * threads created by protocol versions that predate titles. */
export const threadNavigationTitle = (input: {
  readonly thread: ThreadTitleSource;
  readonly sessionTitle: string;
  readonly initialThreadId: string | undefined;
  readonly modeDisplayName?: string | undefined;
}): string => {
  const { thread } = input;
  const storedTitle = cleanTitle(thread.title);
  const mode = cleanTitle(input.modeDisplayName) ?? thread.mode;
  const metadataFallback = `${mode} · ${shortModelName(thread.model)}`;
  const hasSubagentPrefix = storedTitle?.startsWith("Subagent:") === true;

  if (thread.spawned === true || hasSubagentPrefix) {
    return hasSubagentPrefix
      ? storedTitle
      : `Subagent: ${storedTitle ?? metadataFallback}`;
  }
  if (thread.id === input.initialThreadId) {
    return cleanTitle(input.sessionTitle) ?? storedTitle ?? metadataFallback;
  }
  return storedTitle ?? metadataFallback;
};

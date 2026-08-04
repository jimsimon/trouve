export interface ChatFileTarget {
  readonly path: string;
  /** One-based inclusive line numbers; zero means no requested range. */
  readonly from: number;
  readonly to: number;
}

const MAX_FILE_TARGET_LENGTH = 4_096;

/** Match the retained Slint behavior for model-authored workspace file links
 * while keeping URL-like targets out of the typed file workflow. */
export const parseChatFileTarget = (value: string): ChatFileTarget | undefined => {
  if (value.startsWith("file://") && !value.startsWith("file:///")) return undefined;
  let target = value.startsWith("file://") ? value.slice("file://".length) : value;
  if (
    target === "" ||
    target.length > MAX_FILE_TARGET_LENGTH ||
    target.startsWith("#") ||
    target.startsWith("mailto:") ||
    target.startsWith("//") ||
    target.startsWith("/\\") ||
    target.startsWith("\\\\") ||
    /^\/(?:%2f|%5c)/iu.test(target) ||
    target.includes("://") ||
    /[\u0000-\u001f\u007f]/u.test(target)
  ) return undefined;

  let from = 0;
  let to = 0;
  const colon = target.lastIndexOf(":");
  if (colon > 0 && /^\d+$/u.test(target.slice(colon + 1))) {
    const line = Number(target.slice(colon + 1));
    if (Number.isSafeInteger(line) && line > 0) {
      target = target.slice(0, colon);
      from = line;
      to = line;
    }
  } else {
    const fragment = /#L(\d+)(?:-L(\d+))?$/u.exec(target);
    if (fragment !== null) {
      const first = Number(fragment[1]);
      const last = Number(fragment[2] ?? fragment[1]);
      if (
        Number.isSafeInteger(first) &&
        Number.isSafeInteger(last) &&
        first > 0 &&
        last >= first
      ) {
        target = target.slice(0, fragment.index);
        from = first;
        to = last;
      }
    }
  }

  const looksLikePath = target.startsWith("/") ||
    target.startsWith("./") ||
    target.startsWith("../") ||
    target.includes("/") ||
    target.includes("\\") ||
    /(?:^|\/)[^/]+\.[^/]+$/u.test(target);
  return looksLikePath && target !== "" ? Object.freeze({ path: target, from, to }) : undefined;
};

export const isApplicationRouteTarget = (target: string): boolean =>
  target === "/" ||
  /^\/(?:inbox|reviews|automations)(?:[/?#]|$)/u.test(target) ||
  /^\/(?:settings|workspaces)(?:[/?#]|$)/u.test(target);

const normalizedSeparators = (value: string): string => value.replaceAll("\\", "/");

/** Resolve an agent-reported path only within the selected session worktree.
 * The server still performs authoritative containment checks. */
export const sessionRelativeFilePath = (
  reportedPath: string,
  worktreePath: string,
): string | undefined => {
  const reported = normalizedSeparators(reportedPath).replace(/^\.\//u, "");
  const worktree = normalizedSeparators(worktreePath).replace(/\/+$/u, "");
  let relative = reported;
  const absolute = reported.startsWith("/") || /^[A-Za-z]:\//u.test(reported);
  if (absolute) {
    if (worktree === "" || (reported !== worktree && !reported.startsWith(`${worktree}/`))) {
      return undefined;
    }
    relative = reported.slice(worktree.length).replace(/^\/+/, "");
  }
  const segments = relative.split("/");
  if (
    relative === "" ||
    relative.startsWith("/") ||
    segments.some((segment) => segment === "" || segment === "." || segment === "..")
  ) return undefined;
  return relative;
};

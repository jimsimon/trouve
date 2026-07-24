const JOB_STATUS_CLASSES = new Set(["queued", "running", "succeeded", "failed", "stale"]);

/** Return a canonical browser URL only for non-executable HTTP(S) links. */
export function safeExternalUrl(value: unknown): string {
  if (typeof value !== "string") return "";
  try {
    const url = new URL(value);
    if (url.protocol !== "https:" && url.protocol !== "http:") return "";
    if (url.username || url.password) return "";
    return url.toString();
  } catch {
    return "";
  }
}

/** Normalize protocol data before using it in CSS classes or form state. */
export function normalizedReviewMode(value: unknown): "off" | "manual" | "automatic" {
  return value === "manual" || value === "automatic" ? value : "off";
}

/** Keep server-provided job state from introducing arbitrary CSS classes. */
export function jobStatusClass(value: unknown): string {
  return typeof value === "string" && JOB_STATUS_CLASSES.has(value) ? value : "unknown";
}

import type { components as ProtocolComponents } from "../generated/protocol.js";

type ProtocolPrDetail = ProtocolComponents["schemas"]["PrDetail"];
type ProtocolPrFileDiff = ProtocolComponents["schemas"]["PrFileDiff"];
type JsonRecord = Readonly<Record<string, unknown>>;
type Guard = (value: unknown) => boolean;

const object = (value: unknown): JsonRecord | undefined =>
  typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as JsonRecord
    : undefined;
const text = (value: unknown): boolean => typeof value === "string";
const count = (value: unknown): boolean =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
const list = (value: unknown, guard: Guard): boolean =>
  Array.isArray(value) && value.every(guard);
const optionalList = (item: JsonRecord, key: string, guard: Guard): boolean =>
  item[key] === undefined || list(item[key], guard);
const strings = (item: JsonRecord, keys: readonly string[]): boolean =>
  keys.every((key) => text(item[key]));
const counts = (item: JsonRecord, keys: readonly string[]): boolean =>
  keys.every((key) => count(item[key]));
const optionalObject = (item: JsonRecord, key: string, guard: Guard): boolean =>
  item[key] === undefined || item[key] === null || guard(item[key]);

const actor: Guard = (value) => {
  const item = object(value);
  return item !== undefined && strings(item, ["id", "login", "kind"]);
};
const reaction: Guard = (value) => {
  const item = object(value);
  return item !== undefined && text(item["content"]) && count(item["count"]);
};
const check: Guard = (value) => {
  const item = object(value);
  return item !== undefined
    && strings(item, ["name", "status"])
    && ["details_url", "started_at", "completed_at"].every((key) =>
      item[key] === undefined || item[key] === null || text(item[key])
    );
};
const compactReview: Guard = (value) => {
  const item = object(value);
  return item !== undefined && strings(item, ["reviewer", "state"]);
};
const prInfo: Guard = (value) => {
  const item = object(value);
  return item !== undefined
    && count(item["number"])
    && strings(item, ["url", "title", "state", "base", "head"])
    && typeof item["draft"] === "boolean"
    && list(item["checks"], check)
    && list(item["reviews"], compactReview);
};
const label: Guard = (value) => {
  const item = object(value);
  return item !== undefined && strings(item, ["id", "name"]);
};
const milestone: Guard = (value) => {
  const item = object(value);
  return item !== undefined
    && strings(item, ["id", "title"])
    && count(item["number"]);
};
const comment: Guard = (value) => {
  const item = object(value);
  return item !== undefined
    && strings(item, ["id", "body", "url", "created_at", "updated_at"])
    && optionalObject(item, "author", actor)
    && optionalList(item, "reactions", reaction);
};
const review: Guard = (value) => {
  const item = object(value);
  return item !== undefined
    && strings(item, ["id", "state", "url"])
    && optionalObject(item, "author", actor);
};
const reviewThread: Guard = (value) => {
  const item = object(value);
  return item !== undefined
    && strings(item, ["id", "path"])
    && optionalList(item, "comments", comment);
};
const commit: Guard = (value) => {
  const item = object(value);
  return item !== undefined
    && strings(item, [
      "oid",
      "abbreviated_oid",
      "message_headline",
      "committed_at",
      "url",
    ])
    && optionalObject(item, "author", actor);
};
const file: Guard = (value) => {
  const item = object(value);
  return item !== undefined
    && strings(item, ["path", "change_type"])
    && counts(item, ["additions", "deletions"]);
};
const queue: Guard = (value) => {
  const item = object(value);
  const entry = object(item?.["entry"]);
  return item !== undefined && (
    item["entry"] === undefined
    || item["entry"] === null
    || (entry !== undefined
      && strings(entry, ["id", "state", "enqueued_at"])
      && count(entry["position"]))
  );
};
const autoMerge: Guard = (value) => {
  const item = object(value);
  return item !== undefined && strings(item, ["method", "enabled_at"]);
};
const stack: Guard = (value) => {
  const item = object(value);
  return item !== undefined
    && strings(item, ["id", "base"])
    && counts(item, ["number", "size"])
    && optionalList(item, "entries", (entry) => {
      const row = object(entry);
      return row !== undefined
        && strings(row, ["title", "url", "state", "base", "head"])
        && counts(row, ["position", "number"]);
    });
};

/** Compact boundary guard for the lazy PR workspace. A standalone AJV build
 * of this deeply nested response adds roughly 150 KB of generated source;
 * this checks required discriminating/rendered structure while generated
 * OpenAPI types remain the compile-time source of truth. */
export const validatePrDetail = (value: unknown): value is ProtocolPrDetail => {
  const detail = object(value);
  return detail !== undefined
    && prInfo(detail["info"])
    && strings(detail, ["id", "viewer", "created_at", "updated_at"])
    && counts(detail, ["additions", "deletions", "changed_files", "commit_count"])
    && object(detail["capabilities"]) !== undefined
    && queue(detail["merge_queue"])
    && optionalList(detail, "reactions", reaction)
    && optionalList(detail, "labels", label)
    && optionalList(detail, "available_labels", label)
    && optionalList(detail, "assignees", actor)
    && optionalList(detail, "assignable_users", actor)
    && optionalObject(detail, "milestone", milestone)
    && optionalList(detail, "available_milestones", milestone)
    && optionalList(detail, "review_requests", actor)
    && optionalList(detail, "reviews", review)
    && optionalList(detail, "comments", comment)
    && optionalList(detail, "review_threads", reviewThread)
    && optionalList(detail, "commits", commit)
    && optionalList(detail, "files", file)
    && optionalObject(detail, "auto_merge", autoMerge)
    && optionalObject(detail, "stack", stack);
};

/** The selected-file response is deliberately small and lazy, but it still
 * crosses the same untrusted protocol boundary as the full PR workspace. */
export const validatePrFileDiff = (value: unknown): value is ProtocolPrFileDiff => {
  const diff = object(value);
  const optionalText = (key: string): boolean =>
    diff?.[key] === undefined || diff[key] === null || text(diff[key]);
  const optionalCount = (key: string): boolean =>
    diff?.[key] === undefined || diff[key] === null || count(diff[key]);
  return diff !== undefined
    && strings(diff, ["path", "change_type"])
    && optionalText("original")
    && optionalText("modified")
    && optionalText("notice")
    && optionalCount("original_bytes")
    && optionalCount("modified_bytes")
    && (diff["binary"] === undefined || typeof diff["binary"] === "boolean")
    && (diff["truncated"] === undefined || typeof diff["truncated"] === "boolean");
};

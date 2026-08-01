import type { RoutingMode } from "./types";

export function routingReasonLabel(source: string, mode: RoutingMode): string {
  switch (source) {
    case "core":
      return "Manual selection";
    case "baseline":
      return mode === "additive"
        ? "Additive baseline"
        : mode === "automatic"
          ? "Automatic baseline"
          : "Routing baseline";
    case "deterministic":
      return "Diff signal";
    case "semantic":
      return "Semantic triage";
    case "included":
      return "Additive core";
    case "thorough":
      return "Legacy thorough mode";
    default:
      return source;
  }
}

import { describe, expect, it } from "vitest";

import { jobStatusClass, normalizedReviewMode, safeExternalUrl } from "./security.js";

describe("API-derived markup safety", () => {
  it("safeExternalUrl accepts and canonicalizes HTTP links", () => {
    expect(safeExternalUrl("https://github.com/trouve-ai/trouve/pull/1")).toBe(
      "https://github.com/trouve-ai/trouve/pull/1",
    );
    expect(safeExternalUrl("http://localhost:8080/path")).toBe("http://localhost:8080/path");
  });

  it("safeExternalUrl rejects executable, relative, and credential-bearing URLs", () => {
    for (const value of [
      "javascript:alert(document.domain)",
      "data:text/html,<script>alert(1)</script>",
      "file:///etc/passwd",
      "//attacker.example/path",
      "https://user:secret@example.com/path",
      "not a URL",
      "",
      null,
    ]) {
      expect(safeExternalUrl(value), String(value)).toBe("");
    }
  });

  it("API-derived class values are allowlisted", () => {
    expect(normalizedReviewMode("manual")).toBe("manual");
    expect(normalizedReviewMode("automatic")).toBe("automatic");
    expect(normalizedReviewMode('off" onmouseover="alert(1)')).toBe("off");
    expect(jobStatusClass("pending")).toBe("pending");
    expect(jobStatusClass("running")).toBe("running");
    expect(jobStatusClass('failed" onmouseover="alert(1)')).toBe("unknown");
  });
});

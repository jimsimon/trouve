import assert from "node:assert/strict";
import test from "node:test";

import { jobStatusClass, normalizedReviewMode, safeExternalUrl } from "./security.ts";

test("safeExternalUrl accepts and canonicalizes HTTP links", () => {
  assert.equal(safeExternalUrl("https://github.com/trouve-ai/trouve/pull/1"), "https://github.com/trouve-ai/trouve/pull/1");
  assert.equal(safeExternalUrl("http://localhost:8080/path"), "http://localhost:8080/path");
});

test("safeExternalUrl rejects executable, relative, and credential-bearing URLs", () => {
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
    assert.equal(safeExternalUrl(value), "", String(value));
  }
});

test("API-derived class values are allowlisted", () => {
  assert.equal(normalizedReviewMode("manual"), "manual");
  assert.equal(normalizedReviewMode("automatic"), "automatic");
  assert.equal(normalizedReviewMode('off\" onmouseover=\"alert(1)'), "off");
  assert.equal(jobStatusClass("running"), "running");
  assert.equal(jobStatusClass('failed\" onmouseover=\"alert(1)'), "unknown");
});

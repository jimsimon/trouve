import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const sourceRoot = fileURLToPath(new URL(".", import.meta.url));

const source = readdirSync(sourceRoot)
  .filter((name) => name.endsWith(".ts") && !name.endsWith(".test.ts"))
  .map((name) => readFileSync(`${sourceRoot}${name}`, "utf8"))
  .join("\n");

const jobDetail = readFileSync(new URL("./job-detail.ts", import.meta.url), "utf8");

describe("automatic refresh", () => {
  it("review data and CLI status do not rely on manual refresh controls", () => {
    expect(source).not.toMatch(/Reconcile now/u);
    expect(source).not.toMatch(/>\s*Refresh\s*</u);
    expect(source).toMatch(/DASHBOARD_FALLBACK_REFRESH_MS/u);
    expect(source).toMatch(/CLI_IDLE_REFRESH_MS/u);
    expect(source).toMatch(/Retrying automatically\./u);
  });

  it("persona retry labels describe the actual terminal state", () => {
    expect(jobDetail).toContain("`Retry full review after ${group.name} ${group.status}`");
    expect(jobDetail).toMatch(/persona \? "Retry all" : "Retry"/u);
  });
});

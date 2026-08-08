import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("trouve-todo-plan-panel semantic contract", () => {
  it("renders the compact progress label and ordered status rows", () => {
    const source = readFileSync(
      new URL("./todo-plan-panel.ts", import.meta.url),
      "utf8",
    );

    expect(source).toContain("${plan.progressLabel}");
    expect(source).toContain("<ol class=\"todo-plan-list\">");
    expect(source).toContain("data-todo-id=${todo.id}");
    expect(source).toContain('aria-current=${todo.current ? "step" : nothing}');
    expect(source).toContain('<span class="visually-hidden">Status: ${todo.statusLabel}</span>');
  });
});

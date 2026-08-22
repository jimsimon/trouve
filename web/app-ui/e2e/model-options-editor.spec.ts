import { expect, test } from "@playwright/test";

test("model-option choices preserve selected state and scalar value types", async ({ page }) => {
  await page.goto("/gallery.html");
  await page.evaluate(async () => {
    const modulePath = "/src/components/model-options-editor.ts";
    await import(modulePath);
    const editor = document.createElement("trouve-model-options-editor") as HTMLElement & {
      controls: readonly unknown[];
      updateComplete: Promise<boolean>;
    };
    editor.id = "model-options-editor-fixture";
    editor.controls = [
      {
        kind: "choice",
        key: "context_window",
        label: "Context window",
        description: "Maximum input context.",
        overridden: true,
        choices: [
          { label: "300K", value: 300_000 },
          { label: "1M", value: 1_000_000 },
        ],
        selectedIndex: 1,
      },
      {
        kind: "choice",
        key: "fast",
        label: "Fast mode",
        description: "Prefer low latency.",
        overridden: true,
        choices: [
          { label: "Off", value: false },
          { label: "On", value: true },
        ],
        selectedIndex: 0,
      },
      {
        kind: "text",
        key: "thinking_budget_tokens",
        label: "Thinking budget",
        description: "Token budget for reasoning.",
        overridden: true,
        scalarType: "integer",
        text: "8",
        hint: "between 4 and 16",
        minimum: 4,
        maximum: 16,
      },
    ];
    const changes: unknown[] = [];
    editor.addEventListener("trouve-model-option-changed", (event) => {
      changes.push((event as CustomEvent).detail);
    });
    (window as Window & { modelOptionChanges?: unknown[] }).modelOptionChanges = changes;
    document.body.append(editor);
    await editor.updateComplete;
  });

  const editor = page.locator("#model-options-editor-fixture");
  const context = editor.getByLabel("Context window");
  const fast = editor.getByLabel("Fast mode");
  const budget = editor.getByLabel("Thinking budget");
  await expect(context).toHaveValue("1000000");
  await expect(fast).toHaveValue("false");
  await expect(budget).toHaveAttribute("aria-describedby", /model-option-description-/);

  await context.selectOption({ label: "300K" });
  await fast.selectOption({ label: "On" });
  await budget.fill("3.5");
  await budget.press("Tab");
  await expect(budget).toHaveValue("8");

  await expect.poll(() => page.evaluate(() =>
    (window as Window & { modelOptionChanges?: unknown[] }).modelOptionChanges,
  )).toEqual([
    { key: "context_window", value: 300_000 },
    { key: "fast", value: true },
  ]);

  await context.selectOption({ label: "Model default · 1M" });
  await expect.poll(() => page.evaluate(() =>
    (window as Window & { modelOptionChanges?: unknown[] }).modelOptionChanges,
  )).toEqual([
    { key: "context_window", value: 300_000 },
    { key: "fast", value: true },
    { key: "context_window", value: undefined },
  ]);
});

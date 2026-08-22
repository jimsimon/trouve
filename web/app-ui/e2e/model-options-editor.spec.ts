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
      {
        kind: "choice",
        key: "tone",
        label: "Tone",
        description: "Optional response tone.",
        overridden: true,
        choices: [
          { label: "Empty", value: "" },
          { label: "Concise", value: "concise" },
        ],
        selectedIndex: 1,
      },
      {
        kind: "text",
        key: "instructions",
        label: "Instructions",
        description: "Additional model instructions.",
        overridden: true,
        scalarType: "string",
        text: "Existing",
        hint: "Optional instructions",
      },
    ];
    const changes: unknown[] = [];
    editor.addEventListener("trouve-model-option-changed", (event) => {
      const detail = (event as CustomEvent<{ key: string; value: unknown }>).detail;
      changes.push(detail);
      if (detail.key === "instructions" && detail.value === undefined) {
        editor.controls = editor.controls.map((control) => {
          const option = control as Record<string, unknown>;
          return option["key"] === detail.key
            ? { ...option, overridden: false, text: "" }
            : control;
        });
      }
    });
    (window as Window & { modelOptionChanges?: unknown[] }).modelOptionChanges = changes;
    document.body.append(editor);
    await editor.updateComplete;
  });

  const editor = page.locator("#model-options-editor-fixture");
  const context = editor.getByLabel("Context window");
  const fast = editor.getByLabel("Fast mode");
  const budget = editor.getByRole("spinbutton", { name: "Thinking budget", exact: true });
  const tone = editor.getByLabel("Tone");
  const instructions = editor.getByRole("textbox", { name: "Instructions", exact: true });
  await expect(context).toHaveValue("1000000");
  await expect(fast).toHaveValue("false");
  await expect(budget).toHaveAttribute("aria-describedby", /model-option-description-/);

  await context.selectOption({ label: "300K" });
  await fast.selectOption({ label: "On" });
  await budget.fill("3.5");
  await budget.press("Tab");
  await expect(budget).toHaveValue("8");
  await tone.selectOption({ label: "Empty" });
  await instructions.fill("");
  await instructions.press("Tab");
  await instructions.fill("   ");
  await instructions.press("Tab");

  await expect.poll(() => page.evaluate(() =>
    (window as Window & { modelOptionChanges?: unknown[] }).modelOptionChanges,
  )).toEqual([
    { key: "context_window", value: 300_000 },
    { key: "fast", value: true },
    { key: "tone", value: "" },
    { key: "instructions", value: "" },
    { key: "instructions", value: "   " },
  ]);

  await context.selectOption({ label: "Model default · 1M" });
  const resetInstructions = editor.getByLabel("Use model default for Instructions");
  await resetInstructions.click();
  await expect(instructions).toBeFocused();
  await expect.poll(() => page.evaluate(() =>
    (window as Window & { modelOptionChanges?: unknown[] }).modelOptionChanges,
  )).toEqual([
    { key: "context_window", value: 300_000 },
    { key: "fast", value: true },
    { key: "tone", value: "" },
    { key: "instructions", value: "" },
    { key: "instructions", value: "   " },
    { key: "context_window", value: undefined },
    { key: "instructions", value: undefined },
  ]);
});

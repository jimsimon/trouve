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
          { label: "300K", value: { value: 300_000, source: "300000" } },
          { label: "1M", value: { value: 1_000_000, source: "1000000" } },
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
      {
        kind: "text",
        key: "temperature",
        label: "Temperature",
        description: "",
        overridden: true,
        scalarType: "number",
        text: "0.5",
        hint: "value",
      },
      {
        kind: "boolean",
        key: "stream",
        label: "Streaming",
        description: "",
        overridden: false,
        selected: undefined,
      },
    ];
    const changes: unknown[] = [];
    const validationMessages: string[] = [];
    const reportValidity = HTMLInputElement.prototype.reportValidity;
    HTMLInputElement.prototype.reportValidity = function () {
      validationMessages.push(this.validationMessage);
      return reportValidity.call(this);
    };
    editor.addEventListener("trouve-model-option-changed", (event) => {
      const detail = (event as CustomEvent<{ key: string; value: unknown }>).detail;
      changes.push(detail);
      const detailValue = typeof detail.value === "object" && detail.value !== null
        && "value" in detail.value
        ? (detail.value as { value: unknown }).value
        : detail.value;
      if (detail.key === "temperature" && detailValue === 1e20) {
        editor.controls = editor.controls.map((control) => {
          const option = control as Record<string, unknown>;
          return option["key"] === detail.key
            ? { ...option, text: String(detailValue) }
            : control;
        });
      }
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
    (window as Window & { modelOptionValidationMessages?: string[] })
      .modelOptionValidationMessages = validationMessages;
    document.body.append(editor);
    await editor.updateComplete;
  });

  const editor = page.locator("#model-options-editor-fixture");
  const context = editor.getByLabel("Context window");
  const fast = editor.getByLabel("Fast mode");
  const budget = editor.getByRole("spinbutton", { name: "Thinking budget", exact: true });
  const tone = editor.getByLabel("Tone");
  const instructions = editor.getByRole("textbox", { name: "Instructions", exact: true });
  const temperature = editor.getByRole("spinbutton", { name: "Temperature", exact: true });
  const streaming = editor.getByLabel("Streaming");
  await expect(context).toHaveValue("1000000");
  await expect(fast).toHaveValue("false");
  await expect(budget).toHaveAttribute("aria-describedby", /model-option-description-/);
  await expect(streaming.locator("option").first()).toHaveText("Model default");

  for (const value of ["1.0", "1e3", "-0", "1e20"]) {
    await temperature.fill(value);
    await temperature.press("Enter");
  }
  await expect.poll(() => page.evaluate(() => {
    const changes = (window as Window & { modelOptionChanges?: unknown[] })
      .modelOptionChanges as { key: string; value: unknown }[];
    return changes.map((detail) => ({
      ...detail,
      value: typeof detail.value === "object" && detail.value !== null
          && "value" in detail.value
        ? {
            ...detail.value,
            value: Object.is((detail.value as { value: unknown }).value, -0)
              ? "-0"
              : (detail.value as { value: unknown }).value,
          }
        : detail.value,
    }));
  })).toEqual([
    { key: "temperature", value: { value: 1, source: "1.0" } },
    { key: "temperature", value: { value: 1_000, source: "1e3" } },
    { key: "temperature", value: { value: "-0", source: "-0" } },
    { key: "temperature", value: { value: 1e20, source: "1e20" } },
  ]);
  await expect(temperature).toHaveValue("100000000000000000000");
  await page.evaluate(() => {
    (window as Window & { modelOptionChanges?: unknown[] }).modelOptionChanges?.splice(0);
  });

  await context.selectOption({ label: "300K" });
  await fast.selectOption({ label: "On" });
  await budget.fill("3.5");
  await budget.press("Tab");
  await expect(budget).toHaveValue("3.5");
  await budget.fill("9007199254740993");
  await budget.dispatchEvent("change");
  await expect(budget).toHaveValue("9007199254740993");
  await budget.fill("12");
  await budget.press("Enter");
  await temperature.fill("0.1234567890123456789");
  await temperature.dispatchEvent("change");
  await expect(temperature).toHaveValue("0.1234567890123456789");
  await temperature.fill("9007199254740993");
  await temperature.press("Enter");
  await expect(temperature).toHaveValue("9007199254740993");
  await temperature.fill("1e-324");
  await temperature.press("Enter");
  await expect(temperature).toHaveValue("1e-324");
  await temperature.fill("3e-324");
  await temperature.press("Enter");
  await expect(temperature).toHaveValue("3e-324");
  await temperature.fill("0.25");
  await temperature.press("Enter");
  await expect.poll(() => page.evaluate(() =>
    (window as Window & { modelOptionValidationMessages?: string[] })
      .modelOptionValidationMessages,
  )).toEqual([
    "Enter a valid integer between 4 and 16.",
    "Enter a valid integer between 4 and 16.",
    "Enter a valid number value.",
    "Enter a valid number value.",
    "Enter a valid number value.",
    "Enter a valid number value.",
  ]);
  await tone.selectOption({ label: "Empty" });
  await instructions.fill("");
  await instructions.press("Tab");
  await instructions.fill("   ");
  await instructions.press("Tab");

  await expect.poll(() => page.evaluate(() =>
    (window as Window & { modelOptionChanges?: unknown[] }).modelOptionChanges,
  )).toEqual([
    { key: "context_window", value: { value: 300_000, source: "300000" } },
    { key: "fast", value: true },
    { key: "thinking_budget_tokens", value: { value: 12, source: "12" } },
    { key: "temperature", value: { value: 0.25, source: "0.25" } },
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
    { key: "context_window", value: { value: 300_000, source: "300000" } },
    { key: "fast", value: true },
    { key: "thinking_budget_tokens", value: { value: 12, source: "12" } },
    { key: "temperature", value: { value: 0.25, source: "0.25" } },
    { key: "tone", value: "" },
    { key: "instructions", value: "" },
    { key: "instructions", value: "   " },
    { key: "context_window", value: undefined },
    { key: "instructions", value: undefined },
  ]);
});

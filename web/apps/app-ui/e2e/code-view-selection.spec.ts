import { expect, test } from "@playwright/test";

const THEMES = [
  "dark",
  "light",
  "high-contrast-dark",
  "colorblind-dark",
  "colorblind-light",
] as const;

test("Files-view selections use readable semantic colors in every theme", async ({ page }) => {
  await page.goto("/gallery.html");
  const codeView = page.locator("trouve-code-view").first();
  await expect(codeView.locator(".cm-content")).toBeVisible();
  await codeView.evaluate((element) => {
    (element as HTMLElement & { revealRange(from: number, to?: number): void })
      .revealRange(0, 64);
  });

  const selection = codeView.locator(".cm-selectionBackground").first();
  const selectedText = codeView.locator(".cm-trouve-selectedText").first();
  const selectedToken = codeView.locator(".cm-trouve-selectedText span").first();
  await expect(selection).toBeVisible();
  await expect(selectedText).toBeVisible();
  await expect(selectedToken).toBeVisible();

  for (const theme of THEMES) {
    await codeView.evaluate((element, selectedTheme) => {
      element.closest(".gallery-theme")?.setAttribute("data-theme", selectedTheme);
    }, theme);
    const expected = await codeView.evaluate((element) => {
      const probe = document.createElement("span");
      probe.style.cssText = [
        "position: absolute",
        "background: var(--trouve-selection-bg)",
        "color: var(--trouve-selection-fg)",
      ].join(";");
      element.shadowRoot!.append(probe);
      const style = getComputedStyle(probe);
      const colors = { background: style.backgroundColor, foreground: style.color };
      probe.remove();
      return colors;
    });

    await expect(selection, `${theme} selection background`).toHaveCSS(
      "background-color",
      expected.background,
    );
    await expect(selectedText, `${theme} selection foreground`).toHaveCSS(
      "color",
      expected.foreground,
    );
    await expect(selectedToken, `${theme} syntax token foreground`).toHaveCSS(
      "color",
      expected.foreground,
    );
  }
});

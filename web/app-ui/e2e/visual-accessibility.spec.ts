import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Locator, type Page } from "@playwright/test";

const themes = [
  "dark",
  "light",
  "high-contrast-dark",
  "colorblind-dark",
  "colorblind-light",
] as const;

const openGallery = async (page: Page): Promise<void> => {
  await page.goto("/gallery.html");
  await page.locator("trouve-component-gallery").waitFor();
  await page.locator(".gallery-theme").first().waitFor();
  await page.locator("trouve-terminal-view .xterm").waitFor();
  await page.addStyleTag({
    content: `
      *, *::before, *::after {
        animation-duration: 0s !important;
        transition-duration: 0s !important;
        caret-color: transparent !important;
      }
      .xterm-cursor-layer, .cm-cursorLayer, .cm-selectionLayer {
        visibility: hidden !important;
      }
    `,
  });
};

const expectLocatorScreenshot = async (locator: Locator, name: string): Promise<void> => {
  await locator.scrollIntoViewIfNeeded();
  const screenshot = await locator.screenshot({ animations: "disabled" });
  expect(screenshot).toMatchSnapshot(name, { maxDiffPixelRatio: 0.01 });
};

test("gallery has no serious or critical automated accessibility findings", async ({ page }) => {
  await openGallery(page);

  const result = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
    .analyze();
  const blocking = result.violations.filter(
    ({ impact }) => impact === "serious" || impact === "critical",
  );

  expect(blocking).toEqual([]);
});

test("the lazy content worker stays alive without browser errors", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });

  await openGallery(page);
  await expect.poll(() => page.workers().length).toBeGreaterThan(0);
  expect(errors).toEqual([]);
});

test("gallery remains keyboard operable and its content is selectable", async ({ page }) => {
  await openGallery(page);

  const shellLink = page.getByRole("link", { name: "Open application shell" });
  for (let index = 0; index < 2 && !(await shellLink.evaluate((link) => link === document.activeElement)); index += 1) {
    await page.keyboard.press("Tab");
  }
  await expect(shellLink).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "Secondary" }).first()).toBeFocused();

  const message = page.getByText("Keep the current look, feel, and layout.").first();
  await message.evaluate((element) => {
    const selection = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(element);
    selection?.removeAllRanges();
    selection?.addRange(range);
  });
  expect(await page.evaluate(() => window.getSelection()?.toString())).toContain(
    "Keep the current look, feel, and layout.",
  );
});

test("theme surfaces match their reviewed visual references", async ({ browserName, page }, testInfo) => {
  test.skip(browserName !== "chromium", "pixel references are recorded on the Chromium baseline");
  await openGallery(page);

  for (const theme of themes) {
    await expectLocatorScreenshot(
      page.locator(`.gallery-theme:not(.gallery-hard-widgets)[data-theme="${theme}"]`),
      `gallery-${theme}.png`,
    );
  }

  await expectLocatorScreenshot(page.locator(".gallery-hard-widgets"), "gallery-hard-widgets.png");
  await expect(page).toHaveScreenshot(`gallery-full-${testInfo.project.name}.png`, {
    fullPage: true,
  });
});

test("forced colors, reduced motion, and enlarged text retain usable structure", async ({ browserName, page }) => {
  test.skip(browserName !== "chromium", "forced-colors emulation is a Chromium qualification case");
  await page.emulateMedia({ forcedColors: "active", reducedMotion: "reduce" });
  await openGallery(page);
  await page.addStyleTag({ content: "html { font-size: 200% !important; }" });

  const result = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
    .analyze();
  const blocking = result.violations.filter(
    ({ impact }) => impact === "serious" || impact === "critical",
  );
  expect(blocking).toEqual([]);

  await expect(page.locator(".gallery-theme").first()).toHaveScreenshot(
    "gallery-forced-colors-200-percent.png",
  );
});

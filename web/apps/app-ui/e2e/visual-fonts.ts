import type { Page } from "@playwright/test";

/**
 * Use a cross-runner font only for pixel references.
 *
 * The product intentionally follows the platform UI font. Linux developer
 * hosts and GitHub runners resolve `system-ui` differently, however, which
 * turns ordinary glyph-metric changes into unrelated screenshot failures.
 */
export const stabilizeVisualFonts = async (page: Page): Promise<void> => {
  await page.addStyleTag({
    content: `
      :root {
        --trouve-font-sans: "Liberation Sans", sans-serif !important;
        --trouve-font-mono: "Liberation Mono", monospace !important;
      }
    `,
  });
  await page.evaluate(async () => await document.fonts.ready);
};

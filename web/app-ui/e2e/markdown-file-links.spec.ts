import { expect, test } from "@playwright/test";

test("sanitized workspace links emit typed file-open actions", async ({ page }) => {
  await page.goto("/gallery.html");
  await page.locator("trouve-component-gallery").waitFor();

  await page.evaluate(async () => {
    const view = document.createElement("trouve-markdown-view");
    view.id = "file-link-fixture";
    view.content = "[Open source](web/app-ui/src/components/markdown-view.ts#L102-L124)";
    document.body.append(view);
    await view.updateComplete;
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    document.addEventListener("trouve-open-file", (event) => {
      (globalThis as typeof globalThis & { openedFile?: unknown }).openedFile =
        (event as CustomEvent).detail;
    }, { once: true });
  });

  const link = page.locator("#file-link-fixture").getByRole("link", { name: "Open source" });
  await expect(link).toHaveAttribute("href", "#");
  await link.click();

  await expect.poll(() => page.evaluate(() =>
    (globalThis as typeof globalThis & { openedFile?: unknown }).openedFile
  )).toEqual({
    path: "web/app-ui/src/components/markdown-view.ts",
    from: 102,
    to: 124,
  });
});

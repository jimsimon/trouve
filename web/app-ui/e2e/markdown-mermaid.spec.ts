import { expect, test } from "@playwright/test";

test("mermaid fences render as inline diagrams", async ({ page }) => {
  await page.goto("/gallery.html");
  await page.locator("trouve-component-gallery").waitFor();

  await page.evaluate(async () => {
    const view = document.createElement("trouve-markdown-view");
    view.id = "mermaid-fixture";
    view.content = "Before\n\n```mermaid\ngraph TD\n  core[trouve-core] --> api[trouve-plugin-api]\n```\n\nAfter";
    document.body.append(view);
    await view.updateComplete;
  });

  const view = page.locator("#mermaid-fixture");
  const diagram = view.locator("trouve-mermaid-diagram");
  await expect(diagram).toHaveCount(1);
  await expect(diagram.locator("svg")).toBeVisible();
  await expect(diagram.locator("svg")).toContainText("trouve-plugin-api");
  await expect(diagram.locator(".diagram")).toHaveAttribute("aria-describedby", "diagram-source");
  await expect(diagram.locator("#diagram-source")).toContainText(
    "core[trouve-core] --> api[trouve-plugin-api]",
  );
  await expect(view.locator("code.language-mermaid")).toHaveCount(0);
  await expect(view).toContainText("Before");
  await expect(view).toContainText("After");
});

test("unparseable mermaid fences keep their source as a code block", async ({ page }) => {
  await page.goto("/gallery.html");
  await page.locator("trouve-component-gallery").waitFor();

  await page.evaluate(async () => {
    const view = document.createElement("trouve-markdown-view");
    view.id = "mermaid-invalid-fixture";
    view.content = "```mermaid\nthis is not a diagram\n```";
    document.body.append(view);
    await view.updateComplete;
  });

  const diagram = page.locator("#mermaid-invalid-fixture trouve-mermaid-diagram");
  await expect(diagram.locator("pre")).toHaveAttribute("aria-busy", "false");
  await expect(diagram.locator("svg")).toHaveCount(0);
  await expect(diagram).toContainText("this is not a diagram");
});

test("a pending mermaid render finishes after the element reconnects", async ({ page }) => {
  let releaseRenderer = () => {};
  const rendererRelease = new Promise<void>((resolve) => {
    releaseRenderer = resolve;
  });
  let markRendererRequested = () => {};
  const rendererRequested = new Promise<void>((resolve) => {
    markRendererRequested = resolve;
  });
  await page.route("**/*beautiful-mermaid*", async (route) => {
    markRendererRequested();
    await rendererRelease;
    await route.continue();
  });
  await page.goto("/gallery.html");
  await page.locator("trouve-component-gallery").waitFor();

  await page.evaluate(async () => {
    const diagram = document.createElement("trouve-mermaid-diagram");
    diagram.id = "mermaid-reconnect-fixture";
    diagram.source = "graph TD\n  pending --> reconnected";
    document.body.append(diagram);
    await diagram.updateComplete;
  });
  await rendererRequested;
  await page.evaluate(() => {
    const diagram = document.querySelector("#mermaid-reconnect-fixture");
    if (!(diagram instanceof HTMLElement)) throw new Error("missing diagram fixture");
    diagram.remove();
    document.body.append(diagram);
  });
  releaseRenderer();

  const diagram = page.locator("#mermaid-reconnect-fixture");
  await expect(diagram.locator("svg")).toBeVisible();
  await expect(diagram.locator("svg")).toContainText("reconnected");
});

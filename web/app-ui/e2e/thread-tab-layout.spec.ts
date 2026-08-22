import { expect, test } from "@playwright/test";

test("thread working set stays on one row with persistent actions", async ({ page }) => {
  await page.goto("/gallery.html");
  await page.locator("trouve-component-gallery").waitFor();

  await page.evaluate(() => {
    const fixture = document.createElement("header");
    fixture.id = "thread-working-set-fixture";
    fixture.className = "thread-header thread-tab-header";
    fixture.style.width = "520px";
    const tabs = document.createElement("div");
    tabs.className = "thread-tabs";
    tabs.setAttribute("role", "tablist");
    tabs.setAttribute("aria-label", "Threads");
    for (let index = 0; index < 2; index += 1) {
      const item = document.createElement("span");
      item.className = "thread-tab-item";
      item.setAttribute("role", "presentation");
      const tab = document.createElement("button");
      tab.className = "thread-tab-main";
      tab.type = "button";
      tab.setAttribute("role", "tab");
      const label = document.createElement("span");
      label.className = "thread-tab-label";
      const title = document.createElement("span");
      title.className = "thread-tab-title";
      title.textContent = `Thread ${index + 1}`;
      label.append(title);
      tab.append(label);
      const close = document.createElement("button");
      close.className = "thread-tab-close";
      close.type = "button";
      close.setAttribute("aria-label", `Close thread ${index + 1}`);
      close.textContent = "×";
      item.append(tab, close);
      tabs.append(item);
    }
    const add = document.createElement("button");
    add.className = "new-thread-tab";
    add.type = "button";
    add.setAttribute("aria-label", "New thread");
    add.textContent = "+";
    const switcher = document.createElement("div");
    switcher.className = "thread-switcher";
    const toggle = document.createElement("button");
    toggle.className = "thread-switcher-toggle";
    toggle.type = "button";
    toggle.setAttribute("aria-label", "Threads (7)");
    const toggleLabel = document.createElement("span");
    toggleLabel.textContent = "Threads";
    const total = document.createElement("span");
    total.className = "thread-switcher-total";
    total.textContent = "7";
    toggle.append(toggleLabel, total);
    switcher.append(toggle);
    const find = document.createElement("button");
    find.className = "chat-find-toggle";
    find.type = "button";
    find.setAttribute("aria-label", "Find in chat");
    find.textContent = "⌕";
    fixture.append(tabs, add, switcher, find);
    document.body.append(fixture);
  });

  const header = page.locator("#thread-working-set-fixture");
  const tabs = header.locator(".thread-tabs");
  const geometry = await header.evaluate((element) => {
    const bounds = element.getBoundingClientRect();
    const tabs = element.querySelector<HTMLElement>(".thread-tabs");
    const items = [...element.querySelectorAll<HTMLElement>(".thread-tab-item")];
    const add = element.querySelector<HTMLElement>(".new-thread-tab");
    const switcher = element.querySelector<HTMLElement>(".thread-switcher-toggle");
    const find = element.querySelector<HTMLElement>(".chat-find-toggle");
    if (tabs === null || add === null || switcher === null || find === null) {
      throw new Error("fixture incomplete");
    }
    const top = Math.round(items[0]?.getBoundingClientRect().top ?? -1);
    return {
      tabHeight: tabs.clientHeight,
      horizontalOverflow: tabs.scrollWidth - tabs.clientWidth,
      allOneRow: items.every((item) => Math.round(item.getBoundingClientRect().top) === top),
      addVisible: add.getBoundingClientRect().right <= bounds.right,
      switcherVisible: switcher.getBoundingClientRect().right <= bounds.right,
      findVisible: find.getBoundingClientRect().right <= bounds.right,
      findRightmost: find.getBoundingClientRect().left >= switcher.getBoundingClientRect().right,
      actionsAligned: [switcher, find].every((action) =>
        Math.abs(add.getBoundingClientRect().top - action.getBoundingClientRect().top) <= 1),
      overflow: getComputedStyle(tabs).overflow,
    };
  });

  expect(geometry.tabHeight).toBe(
    (page.viewportSize()?.width ?? 1440) <= 760 ? 42 : 30,
  );
  expect(geometry.horizontalOverflow).toBeLessThanOrEqual(1);
  expect(geometry.allOneRow).toBe(true);
  expect(geometry.addVisible).toBe(true);
  expect(geometry.switcherVisible).toBe(true);
  expect(geometry.findVisible).toBe(true);
  expect(geometry.findRightmost).toBe(true);
  expect(geometry.actionsAligned).toBe(true);
  expect(geometry.overflow).toBe("hidden");
  await expect(tabs).toBeVisible();
  await expect(page.getByRole("button", { name: "Threads (7)" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Find in chat" })).toBeVisible();
});

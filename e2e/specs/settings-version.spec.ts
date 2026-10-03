import { clickHeaderAction, expect, onAFrozenClock, openProject, seedToken, test } from "./support";
import type { Page } from "@playwright/test";

/** Observe production footer work even when its formatted age does not change. */
async function expectFooterUpdate(page: Page, milliseconds: number, identity: string): Promise<void> {
  const updated = page.locator("#footer-updated");
  const updates = await updated.evaluateHandle(node => {
    const observation = { count: 0 };
    const observer = new MutationObserver(() => observation.count++);
    observer.observe(node, { childList: true, characterData: true, subtree: true });
    return { observation, observer };
  });
  try {
    await page.clock.runFor(milliseconds);
    expect(await updates.evaluate(state => state.observation.count)).toBeGreaterThan(0);
    await expect(page.locator("#footer-version")).toHaveText(identity);
  } finally {
    await updates.evaluate(state => state.observer.disconnect());
    await updates.dispose();
  }
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("a footer timer update can retain the same age label and version", async ({ page }) => {
  await page.clock.install();
  await onAFrozenClock(page, async () => {
    await page.goto("/");
    await expect(page.locator(".repo-card-name", { hasText: "Alpha Project" })).toBeVisible();
    const identity = await page.locator("#footer-version").innerText();
    await expect(page.locator("#footer-updated")).toHaveText("Updated just now");
    await expectFooterUpdate(page, 1000, identity);
    await expect(page.locator("#footer-updated")).toHaveText("Updated just now");
  });
});

test("Settings renders the running Storyhook version in its About section", async ({
  page,
}) => {
  await page.goto("/");
  await page.locator("#settings-btn").click();

  const settings = page.locator("#settings-view");
  await expect(settings).toBeVisible();
  await expect(settings.getByRole("heading", { level: 2 })).toHaveText([
    "Notices",
    "Automatic model and effort",
    "Projects",
    "About",
    "Dispatch log",
  ]);

  const about = settings.locator("section[aria-labelledby='settings-about-title']");
  await expect(about.getByRole("heading", { level: 2, name: "About" })).toBeVisible();
  await expect(about.locator("dt")).toHaveText("Version");

  const version = about.locator("dd#settings-version");
  await expect(version).toHaveText(/^Storyhook v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)? \(\d+\)$/);
  await expect(version).toHaveCSS("font-family", /monospace/);
  await expect(version).toHaveText(await page.locator("#footer-version").innerText());
});

for (const width of [1280, 390]) {
  test(`footer version survives navigation and timer updates at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 800 });
    await page.clock.install();
    await page.goto("/");
    await expect(page.locator(".repo-card-name", { hasText: "Alpha Project" })).toBeVisible();

    const version = page.locator("#footer-version");
    const updated = page.locator("#footer-updated");
    await expect(version).toHaveText(/^Storyhook v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)? \(\d+\)$/);
    const identity = await version.innerText();

    const checkFooter = async () => {
      await expect(version).toHaveText(identity);
      await expect(version).toBeInViewport({ ratio: 1 });
      await expect(updated).toBeInViewport({ ratio: 1 });
      await expect(version).toHaveCSS("font-size", "11px");
      const versionBox = await version.boundingBox();
      const updatedBox = await updated.boundingBox();
      expect(versionBox).not.toBeNull();
      expect(updatedBox).not.toBeNull();
      expect(versionBox!.y).toBeGreaterThanOrEqual(updatedBox!.y + updatedBox!.height);
      await onAFrozenClock(page, async () => {
        await expectFooterUpdate(page, 3000, identity);
        await expect(updated).toHaveText(/^Updated \d+s ago$/);
        await expect(version).toHaveText(identity);
      });
    };

    await checkFooter();
    await openProject(page, "Alpha Project");
    await checkFooter();
    await clickHeaderAction(page, "settings-btn");
    await expect(page.locator("#settings-view")).toBeVisible();
    await expect(page.locator("#settings-version")).toHaveText(identity);
    await checkFooter();
    await clickHeaderAction(page, "home-btn");
    await expect(page.locator("#home-view")).toBeVisible();
    await checkFooter();
  });
}

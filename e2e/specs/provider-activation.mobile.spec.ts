import { test, expect, openProject, seedToken } from "./support";

// The mobile projects supply touch input; tablet dimensions cover the iPad
// report while retaining both configured browser engines.
test.use({ viewport: { width: 1024, height: 1366 } });

for (const entry of ["drawer", "menu"]) {
  test(`tablet Dispatch focuses its heading after ${entry} tap`, async ({ page }) => {
    await seedToken(page);
    await page.goto("/");
    await openProject(page, "Alpha Project");
    const card = page.locator(".card", { hasText: "Wire up the auth flow" });
    if (entry === "drawer") {
      await card.tap();
      await page.locator("#dispatch-btn").tap();
    } else {
      await card.locator(".card-actions-btn").tap();
      await page.getByRole("menuitem", { name: "Dispatch", exact: true }).tap();
    }
    await expect(page.locator("#dispatch-modal-header")).toBeFocused();
    await expect(page.locator("#dispatch-agent")).not.toBeFocused();
    await expect(page.locator("#dispatch-modal-submit")).toBeEnabled();
    await expect(page.locator("#dispatch-modal-header")).toBeFocused();
    await page.locator("#dispatch-modal-cancel").tap();
    await expect(page.locator("#dispatch-modal")).not.toHaveClass(/open/);
  });
}

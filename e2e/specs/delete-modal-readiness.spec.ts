import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  createStory,
  openDeleteModal,
  openProject,
  seedToken,
} from "./support";

cleanUpCreatedStories("Alpha Project");

for (const confirmation of ["confirm", ""]) {
  test(`opening a delete modal waits for its server plan (${confirmation || "empty field"})`, async ({ page }) => {
    await seedToken(page);
    await page.goto("/");
    await openProject(page, "Alpha Project");
    const title = `SH-588 — delayed deletion plan ${confirmation} ${Date.now()}`;
    await createStory(page, title);
    const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
    const id = (await card.getAttribute("data-id"))!;
    let planReleased = false;

    await page.route("**/api/repos/*/story/*", async (route) => {
      const request = route.request();
      if (request.method() === "DELETE" && request.postDataJSON()?.force === false) {
        // Controlled endpoint latency exposes a helper that mistakes the
        // loading summary's story ID for a completed confirmation plan.
        await new Promise((resolve) => setTimeout(resolve, 250));
        planReleased = true;
      }
      await route.continue();
    });

    await openDeleteModal(page, card, confirmation);
    expect(planReleased, "the modal helper returned before its plan was released").toBe(true);
    await expect(page.locator("#delete-modal-submit")).toBeEnabled();
    await expect(page.locator("#delete-confirmation")).toHaveValue(confirmation ? id : "");

    if (!confirmation) await page.locator("#delete-confirmation").fill(id);
    await page.locator("#delete-confirmation").press("Enter");
    await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
    await expect(card).not.toBeVisible();
  });
}

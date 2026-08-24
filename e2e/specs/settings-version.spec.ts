import { expect, seedToken, test } from "./support";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
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
    "Projects",
    "About",
    "Dispatch log",
  ]);

  const about = settings.locator("section[aria-labelledby='settings-about-title']");
  await expect(about.getByRole("heading", { level: 2, name: "About" })).toBeVisible();
  await expect(about.locator("dt")).toHaveText("Version");

  const version = about.locator("dd#settings-version");
  await expect(version).toHaveText(/^Storyhook v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/);
  await expect(version).toHaveCSS("font-family", /monospace/);
});

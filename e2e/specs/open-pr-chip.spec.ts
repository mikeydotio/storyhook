import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  createStory,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/** SH-586: open linked pull requests are visible and directly actionable on board cards. */

cleanUpCreatedStories("Alpha Project");

const DASHBOARD_TOKEN = requiredEnv("DASHBOARD_TOKEN");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("an open PR chip opens its exact link in a new tab without opening the drawer", async ({
  page,
  request,
}) => {
  const title = "SH-586 open PR chip";
  const id = await createStory(page, title);
  const slug = await projectSlug(request, "Alpha Project");
  const card = page.locator(`.card[data-id="${id}"]`);
  const chip = card.locator(".open-pr-chip");
  const prUrl = "https://github.com/acme/widgets/pull/586";

  await expect(chip).toHaveCount(0);
  const linked = await request.post(
    `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(id)}/link-pr`,
    {
      headers: {
        "Content-Type": "application/json",
        "X-Storyhook": "1",
        "X-Storyhook-Token": DASHBOARD_TOKEN,
      },
      data: { url: prUrl },
    },
  );
  expect(linked.ok(), await linked.text()).toBe(true);

  await expect(chip).toHaveText("PR #586 ↗");
  await expect(chip).toHaveAttribute("href", prUrl);
  await expect(chip).toHaveAttribute("target", "_blank");
  await expect(chip).toHaveAttribute("rel", "noopener noreferrer");
  await expect(chip).toHaveAttribute("title", "Open acme/widgets#586 in a new tab");
  await expect(card).toHaveAccessibleName(/open pull request acme\/widgets#586/i);

  await page.context().route(prUrl, async (route) => {
    await route.fulfill({ status: 200, contentType: "text/html", body: "PR 586" });
  });
  const [popup] = await Promise.all([page.waitForEvent("popup"), chip.click()]);
  await popup.waitForLoadState();
  expect(popup.url()).toBe(prUrl);
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await popup.close();
});

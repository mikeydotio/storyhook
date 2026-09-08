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
  const labelled = await request.post(
    `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(id)}/labels`,
    {
      headers: {
        "Content-Type": "application/json",
        "X-Storyhook": "1",
        "X-Storyhook-Token": DASHBOARD_TOKEN,
      },
      data: { add: ["frontend"] },
    },
  );
  expect(labelled.ok(), await labelled.text()).toBe(true);
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

  // SH-597: the painted PR pill is as compact as a card label, while its
  // transparent anchor keeps the pointer target promised by --tap-min.
  const labelVisual = card.locator(".card-labels .chip", { hasText: "frontend" });
  const prVisual = chip.locator(".open-pr-chip-visual");
  await expect(labelVisual).toBeVisible();
  await expect(prVisual).toBeVisible();
  const visualMetrics = async (selector: typeof labelVisual) =>
    selector.evaluate((node) => {
      const style = getComputedStyle(node);
      return {
        height: node.getBoundingClientRect().height,
        paddingTop: style.paddingTop,
        paddingRight: style.paddingRight,
        paddingBottom: style.paddingBottom,
        paddingLeft: style.paddingLeft,
        fontSize: style.fontSize,
        borderRadius: style.borderRadius,
      };
    });
  expect(await visualMetrics(prVisual)).toEqual(await visualMetrics(labelVisual));
  const targetMetrics = await chip.evaluate((node) => {
    const style = getComputedStyle(node);
    const tapMin = getComputedStyle(document.documentElement)
      .getPropertyValue("--tap-min")
      .trim();
    return { height: style.height, minHeight: style.minHeight, tapMin };
  });
  expect(targetMetrics).toEqual({
    height: targetMetrics.tapMin,
    minHeight: targetMetrics.tapMin,
    tapMin: targetMetrics.tapMin,
  });

  await page.context().route(prUrl, async (route) => {
    await route.fulfill({ status: 200, contentType: "text/html", body: "PR 586" });
  });
  const [popup] = await Promise.all([page.waitForEvent("popup"), chip.click()]);
  await popup.waitForLoadState();
  expect(popup.url()).toBe(prUrl);
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await popup.close();
});

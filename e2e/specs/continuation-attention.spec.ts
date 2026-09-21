import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  createStory,
  openProject,
  projectSlug,
  seedToken,
} from "./support";

cleanUpCreatedStories("Alpha Project");

test("an unresolved continuation is visible on the card and in its detail", async ({ page, request }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const id = await createStory(page, "SH-744 delayed continuation fixture");
  const slug = await projectSlug(request, "Alpha Project");
  const alert = {
    request_id: "request-sh744",
    status: "needs-attention",
    detail: "Receiving review timed out; live session preserved",
    next_step: `Run story continuation status ${id} --json and review the exact request.`,
  };
  let unresolved = true;
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    async (route) => {
      const response = await route.fetch();
      const data = await response.json();
      const view = data.stories.find((candidate: { story: { id: string } }) => candidate.story.id === id);
      if (!view) throw new Error("created fixture story is missing from project data");
      if (unresolved) view.continuation_alerts = [alert];
      else delete view.continuation_alerts;
      await route.fulfill({ response, json: data });
    },
  );
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/story/${id}`,
    async (route) => {
      const response = await route.fetch();
      const view = await response.json();
      if (unresolved) view.continuation_alerts = [alert];
      else delete view.continuation_alerts;
      await route.fulfill({ response, json: view });
    },
  );

  await page.reload();
  const card = page.locator(`.card[data-id="${id}"]`);
  await expect(card.locator(".continuation-alert-chip")).toContainText("Continuation needs attention");
  await expect(card).toHaveAttribute("aria-label", /Continuation needs attention/);
  await card.click();
  const banner = page.locator("#drawer-body .continuation-alert-banner");
  await expect(banner).toContainText("request-sh744");
  await expect(banner).toContainText(alert.detail);
  await expect(banner).toContainText(alert.next_step);

  unresolved = false;
  await page.reload();
  await expect(page.locator(`.card[data-id="${id}"] .continuation-alert-chip`)).toHaveCount(0);
});

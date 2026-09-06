import { test, expect } from "./support";
import { openProject, projectSlug, seedToken } from "./support";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

test("a durable verifier halt remains visible until exact acknowledgement", async ({
  page,
  request,
}) => {
  const slug = await projectSlug(request, "Alpha Project");
  let active = true;
  const incidentId = "fixture:verification:42";

  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    async (route) => {
      const response = await route.fetch();
      const data = await response.json();
      const template = data.stories?.[0];
      if (!template) throw new Error("verification incident fixture has no story to clone");
      const stalled = JSON.parse(JSON.stringify(template));
      stalled.story.id = "SH-9573";
      stalled.story.title = "SH-573 halted verification fixture";
      stalled.story.state = "verifying";
      stalled.story.superstate = "OPEN";
      stalled.display_state = null;
      stalled.is_ready = false;
      stalled.is_blocked = false;
      stalled.verification = {
        status: "stalled",
        attempts: 3,
        first_failed_at: "2026-09-05T21:10:44Z",
        last_failed_at: "2026-09-05T21:11:44Z",
        detail: "not inside a git worktree",
        halted: true,
      };
      data.stories.push(stalled);
      data.verification_incident = active
        ? {
            incident_id: incidentId,
            project: slug,
            story_id: "SH-9573",
            generation: 42,
            disposition: "permanent",
            halted: true,
            attempts: 3,
            detail: "not inside a git worktree",
            first_failed_at: "2026-09-05T21:10:44Z",
            last_failed_at: "2026-09-05T21:11:44Z",
          }
        : null;
      await route.fulfill({ response, json: data });
    },
  );
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/verification/ack`,
    async (route) => {
      expect(route.request().postDataJSON()).toEqual({ incident_id: incidentId });
      active = false;
      await route.fulfill({ status: 200, json: { acknowledged: incidentId } });
    },
  );

  await openProject(page, "Alpha Project");
  const banner = page.locator(".verification-halted-banner");
  await expect(banner).toContainText("Central verification halted");
  await expect(banner).toContainText("not inside a git worktree");
  await page.reload();
  await expect(banner).toBeVisible();

  const card = page.locator('.column[data-state="verifying"] .card', {
    hasText: "SH-573 halted verification fixture",
  });
  await expect(card.locator(".verification-chip-stalled")).toContainText(
    "Verification halted · attempt 3",
  );
  await banner.getByRole("button", { name: "Acknowledge and retry" }).click();
  await expect(banner).toHaveCount(0);
});

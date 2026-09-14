import { test, expect, openProject, projectSlug, seedToken } from "./support";

test("a recovered retry shows running work and ordinary queue ownership", async ({ page, request }) => {
  await seedToken(page);
  await page.goto("/");
  const slug = await projectSlug(request, "Alpha Project");
  await page.route(
    url => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    async route => {
      const response = await route.fetch();
      const data = await response.json();
      const template = data.stories?.[0];
      if (!template) throw new Error("retry fixture needs a production story view");
      const cards = [
        { id: "SH-94714", title: "SH-714 recovered gate", verification: {
          status: "running", elapsed_seconds: 65,
          current_step: { label: "release gate", elapsed_seconds: 30 },
        } },
        { id: "SH-94715", title: "SH-714 ordinary waiting", verification: {
          status: "queued", wait_seconds: 65, position: 1,
        } },
      ];
      for (const fixture of cards) {
        const card = JSON.parse(JSON.stringify(template));
        Object.assign(card.story, { id: fixture.id, title: fixture.title, state: "verifying", superstate: "OPEN" });
        Object.assign(card, { display_state: null, is_ready: false, is_blocked: false, verification: fixture.verification });
        data.stories.push(card);
      }
      data.verification_incident = {
        incident_id: "fixture:714", project: slug, story_id: cards[0].id,
        generation: 714, disposition: "retryable", halted: false, attempts: 1,
        detail: "Earlier PR head convergence failure",
        first_failed_at: "2026-09-13T08:55:35Z", last_failed_at: "2026-09-13T08:55:35Z",
      };
      data.verification_control = { state: "running" };
      Object.assign(data.verifier, {
        incident: data.verification_incident, incident_is_current: false,
        held_stories: [], warning: null,
        active: { attempt_id: "retry-714", story_id: cards[0].id, generation: 714 },
        recovery: { acknowledgement: null, request: null },
      });
      await route.fulfill({ response, json: data });
    },
  );
  await openProject(page, "Alpha Project");
  const column = page.locator('.column[data-state="verifying"]');
  const running = column.locator(".card", { hasText: "SH-714 recovered gate" });
  const queued = column.locator(".card", { hasText: "SH-714 ordinary waiting" });
  await expect(running.locator(".verification-chip-running")).toBeVisible();
  await expect(running).toContainText("release gate");
  await expect(queued.locator(".verification-chip-queued")).toBeVisible();
  await expect(queued).not.toContainText(/retrying|halted|convergence/i);
  await expect(page.locator(".verification-halted-banner")).toHaveCount(0);
  await expect(column.getByRole("button", { name: "Stop verifier", exact: true })).toBeEnabled();
  await page.reload();
  await expect(running.locator(".verification-chip-running")).toBeVisible();
  await expect(queued).not.toContainText(/retrying|halted|convergence/i);
});

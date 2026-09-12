import { test, expect } from "./support";
import { openProject, projectSlug, seedToken } from "./support";

/**
 * SH-679's mobile half: the stacked list's "Updated" detail row is the
 * viewer's local date. Same pinned instant and the same two DST-free zones as
 * `local-time.spec.ts`; runs under both mobile engines by its suffix.
 */

const FIXTURE = "SH-679 mobile local time fixture";
const FIXTURE_ID = "SH-90681";
/** Late evening UTC on the 1st: already the 2nd in Tokyo, still the 1st in Honolulu. */
const AT = "2026-03-01T23:30:00Z";

const ZONES = [
  { zone: "Asia/Tokyo", date: "2026-03-02" },
  { zone: "Pacific/Honolulu", date: "2026-03-01" },
] as const;

for (const expected of ZONES) {
  test.describe(`viewed from ${expected.zone}`, () => {
    test.use({ timezoneId: expected.zone });

    test.beforeEach(async ({ page, request }) => {
      await seedToken(page);
      await page.setViewportSize({ width: 390, height: 844 });
      await page.goto("/");
      const slug = await projectSlug(request, "Alpha Project");
      await page.route(
        (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
        async (route) => {
          const response = await route.fetch();
          const data = await response.json();
          const template = data.stories?.[0];
          if (!template) throw new Error("mobile local-time fixture has no story to clone");
          const fixture = JSON.parse(JSON.stringify(template));
          fixture.story.id = FIXTURE_ID;
          fixture.story.title = FIXTURE;
          fixture.story.state = "todo";
          fixture.story.superstate = "OPEN";
          fixture.story.updated_at = AT;
          fixture.display_state = null;
          fixture.is_ready = true;
          fixture.is_blocked = false;
          data.stories.push(fixture);
          await route.fulfill({ response, json: data });
        },
      );
      await openProject(page, "Alpha Project");
    });

    test("the mobile Updated detail is the local date, carrying the stored instant", async ({
      page,
    }) => {
      await page.locator('#view-toggle button[data-view="list"]').click();
      await expect(page.locator("#list-view")).toBeVisible();
      await expect(page.locator("#mobile-list")).toBeVisible();

      const item = page.locator(`#mobile-list li[data-id="${FIXTURE_ID}"]`);
      await item.getByRole("button", { name: `Details for ${FIXTURE_ID}` }).click();
      const updated = item.locator("dl.mobile-story-details dd time");
      await expect(updated).toHaveText(expected.date);
      await expect(updated).toHaveAttribute("datetime", AT);
      await expect(updated).toHaveAttribute("title", new RegExp(`^${AT} UTC`));
    });
  });
}

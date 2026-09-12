import { test, expect } from "./support";
import { openProject, projectSlug, seedToken } from "./support";

/**
 * SH-679: every absolute time the dashboard shows is the viewer's local
 * wall-clock, not the server's UTC string with its `Z` chopped off.
 *
 * The fixture is response-only: it clones the first Alpha Project story into
 * two stories whose `updated_at`, comment and commit times are pinned to
 * instants that fall on different calendar days east and west of Greenwich,
 * and raises a halted-verifier incident with a pinned `first_failed_at`.
 * Each `describe` runs the same page under a DST-free zone on either side of
 * the date line, so a correct conversion shows a different date in each and
 * a UTC leak shows the same date in both. The ordering the list applies to
 * these rows keys on the stored string, never the displayed one, so it must
 * not change with the zone.
 */

const FIXTURE = "SH-679 local time fixture";
const LATER = "SH-679 local time fixture, later";
const FIXTURE_ID = "SH-90679";
const LATER_ID = "SH-90680";
/** Late evening UTC on the 1st: already the 2nd in Tokyo, still the 1st in Honolulu. */
const AT = "2026-03-01T23:30:00Z";
/** Forty minutes later: a new UTC day, the same local day as AT in both zones. */
const LATER_AT = "2026-03-02T00:10:00Z";

const ZONES = [
  { zone: "Asia/Tokyo", date: "2026-03-02", minute: "2026-03-02 08:30", second: "2026-03-02 08:30:00" },
  { zone: "Pacific/Honolulu", date: "2026-03-01", minute: "2026-03-01 13:30", second: "2026-03-01 13:30:00" },
] as const;

async function injectPinnedTimes(
  page: import("@playwright/test").Page,
  slug: string,
): Promise<void> {
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    async (route) => {
      const response = await route.fetch();
      const data = await response.json();
      const template = data.stories?.[0];
      if (!template) throw new Error("local-time fixture has no story to clone");

      const fixture = JSON.parse(JSON.stringify(template));
      fixture.story.id = FIXTURE_ID;
      fixture.story.title = FIXTURE;
      fixture.story.state = "todo";
      fixture.story.superstate = "OPEN";
      fixture.story.updated_at = AT;
      fixture.story.comments = [{ at: AT, text: "SH-679 pinned comment" }];
      fixture.story.referenced_by_commits = [
        { at: AT, sha: "0123456789abcdef0123456789abcdef01234567", subject: "feat: pinned commit" },
      ];
      fixture.display_state = null;
      fixture.is_ready = true;
      fixture.is_blocked = false;

      const later = JSON.parse(JSON.stringify(fixture));
      later.story.id = LATER_ID;
      later.story.title = LATER;
      later.story.updated_at = LATER_AT;
      later.story.comments = [];
      later.story.referenced_by_commits = [];

      data.stories.push(fixture, later);
      data.verification_control = { state: "running" };
      data.verification_incident = {
        incident_id: "fixture:verification:679",
        project: slug,
        story_id: FIXTURE_ID,
        generation: 679,
        disposition: "permanent",
        halted: true,
        attempts: 1,
        detail: "pinned incident for SH-679",
        first_failed_at: AT,
        last_failed_at: AT,
      };
      await route.fulfill({ response, json: data });
    },
  );
}

for (const expected of ZONES) {
  test.describe(`viewed from ${expected.zone}`, () => {
    test.use({ timezoneId: expected.zone });

    test.beforeEach(async ({ page, request }) => {
      await seedToken(page);
      await page.goto("/");
      const slug = await projectSlug(request, "Alpha Project");
      await injectPinnedTimes(page, slug);
      await openProject(page, "Alpha Project");
    });

    test("the list's Updated column is the local date, carrying the stored instant", async ({
      page,
    }) => {
      await page.locator('#view-toggle button[data-view="list"]').click();
      await expect(page.locator("#list-view")).toBeVisible();

      const cell = page.locator(`tr[data-id="${FIXTURE_ID}"] td.col-date time`);
      await expect(cell).toHaveText(expected.date);
      await expect(cell).toHaveAttribute("datetime", AT);
      await expect(cell).toHaveAttribute("title", new RegExp(`^${AT} UTC`));
      await expect(cell).toHaveAttribute("title", new RegExp(expected.zone.replace("/", "\\/")));
    });

    test("sorting by Updated still orders by the stored instant", async ({ page }) => {
      await page.locator('#view-toggle button[data-view="list"]').click();
      await expect(page.locator("#list-view")).toBeVisible();

      const header = page.locator('thead th[data-col="updated"]');
      await header.click();
      await expect(page.locator("#sort-updated")).toHaveText("▲");
      const ascending = await page.locator("#list-body tr[data-id]").evaluateAll((rows) =>
        rows.map((row) => row.getAttribute("data-id")),
      );
      expect(ascending.indexOf(FIXTURE_ID)).toBeLessThan(ascending.indexOf(LATER_ID));

      await header.click();
      await expect(page.locator("#sort-updated")).toHaveText("▼");
      const descending = await page.locator("#list-body tr[data-id]").evaluateAll((rows) =>
        rows.map((row) => row.getAttribute("data-id")),
      );
      expect(descending.indexOf(LATER_ID)).toBeLessThan(descending.indexOf(FIXTURE_ID));
    });

    test("comment and commit rows in the drawer show the local minute", async ({ page }) => {
      await page.locator(`.card[data-id="${FIXTURE_ID}"]`).click();
      await expect(page.locator("#drawer")).toHaveClass(/open/);

      const comment = page.locator("#drawer-body .comments .comment-meta time");
      await expect(comment).toHaveText(expected.minute);
      await expect(comment).toHaveAttribute("datetime", AT);

      // Referenced By is collapsed by default; open it to read the commit row.
      const toggle = page.locator("#drawer-body button", { hasText: "Referenced By" });
      if ((await toggle.getAttribute("aria-expanded")) !== "true") await toggle.click();
      const commit = page.locator("#drawer-body .referenced-by-row .referenced-by-meta time");
      await expect(commit).toHaveText(expected.minute);
      await expect(commit).toHaveAttribute("datetime", AT);
    });

    test("the halted-verifier banner shows the first failure to the local second", async ({
      page,
    }) => {
      const banner = page.locator(".verification-halted-banner");
      await expect(banner).toContainText("Central verification halted");
      const firstFailure = banner.locator(".engine-banner-meta time");
      await expect(firstFailure).toHaveText(expected.second);
      await expect(firstFailure).toHaveAttribute("datetime", AT);
    });
  });
}

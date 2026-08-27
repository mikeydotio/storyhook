import { expect, test } from "./support";
import { openProject, seedToken } from "./support";
import type { Locator, Page } from "@playwright/test";

/**
 * SH-444: the topbar's Home/Settings/Drafts icons, plus the board's column
 * sort control and the Settings-statuses back link, used to be single
 * Unicode characters (`⌂`/`⚙`/`✎`/`⇅`/`←`) rendered through whatever
 * fallback font the platform picked for a codepoint `--sans` doesn't cover
 * -- one of them (`⚙` GEAR) is additionally an *unqualified* emoji, so even
 * its text-vs-colour presentation was undetermined per platform. All five
 * are now inline `<svg class="icon">` shapes, matching the pattern the
 * search box's icon already used (`.search-wrap svg`).
 *
 * `tests/dashboard_icon_glyphs.rs` is the wiring fence -- it proves the
 * source no longer contains an unqualified pictographic character and that
 * every `.btn-icon` span holds a shape, not text. It cannot prove the shape
 * actually paints anything, which is what these tests are for: a real
 * browser layout, on both desktop engines, confirming `svg.icon` renders
 * with a non-zero box in the exact control it replaced.
 *
 * `.card-actions-btn`/`.row-actions-btn` are `display: none` outside
 * `pointer: coarse` (SH-235) and are covered by `icon-shapes.mobile.spec.ts`
 * instead, on the projects where they're actually visible.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

async function expectIconShape(locator: Locator): Promise<void> {
  await expect(locator).toBeVisible();
  const box = await locator.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.width).toBeGreaterThan(0);
  expect(box!.height).toBeGreaterThan(0);
}

async function expectDisclosureShape(
  locator: Locator,
  direction: "right" | "down",
): Promise<void> {
  await expectIconShape(locator);
  await expect(locator).toHaveAttribute("aria-hidden", "true");
  await expect(locator).toHaveAttribute("data-direction", direction);
  await expect(locator).toHaveAttribute("stroke", "currentColor");
  const box = await locator.boundingBox();
  expect(box!.width).toBeCloseTo(14, 1);
  expect(box!.height).toBeCloseTo(14, 1);
}

function buttonIcon(page: Page, id: string): Locator {
  return page.locator(`#${id} svg.icon`);
}

test("Home, Settings and Drafts render an svg icon, not a character", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");

  for (const id of ["home-btn", "settings-btn", "drafts-btn"]) {
    await expectIconShape(buttonIcon(page, id));
  }

  // The accessible name comes from `.btn-text` (`.btn-icon` is
  // `aria-hidden`), never the icon -- unaffected by the glyph-to-shape swap.
  // `#drafts-btn-text` carries the live count on top of the static label, so
  // it's matched loosely rather than exactly.
  await expect(page.locator("#home-btn .btn-text")).toHaveText("Home");
  await expect(page.locator("#settings-btn .btn-text")).toHaveText(
    "Settings",
  );
  await expect(page.locator("#drafts-btn-text")).toContainText("Drafts");
});

test("a column's sort button renders an svg icon and keeps its own accessible name", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  const sortBtn = page.locator('.column[data-state="todo"] .column-sort-btn');
  await expectIconShape(sortBtn.locator("svg.icon"));
  // `aria-label` is set per render from the active sort (`renderBoard`),
  // independent of the icon -- still a real name after the glyph is gone.
  await expect(sortBtn).toHaveAttribute("aria-label", /^Sort: /);
});

test("the statuses editor's back link renders an svg icon alongside its text", async ({
  page,
}) => {
  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  await page
    .locator(".settings-table tbody tr", { hasText: "Alpha Project" })
    .getByRole("button", { name: "Statuses" })
    .click();
  await expect(page.locator(".settings-head h2")).toHaveText(
    "Statuses · Alpha Project",
  );

  // `.back-link` by class, not by accessible name: `#projsel-btn` can show
  // the identical "All projects" text (no project selected), which would
  // make a name-based role query strict-mode ambiguous.
  const backLink = page.locator(".back-link");
  await expectIconShape(backLink.locator("svg.icon"));
  await expect(backLink).toHaveText(/All projects/);
});

test("every hidden-content control draws a consistently sized disclosure chevron", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");

  await expectDisclosureShape(
    page.locator("#projsel-btn .disclosure-icon"),
    "down",
  );

  const filterToggle = page.locator("#filter-toggle-btn");
  const filterIcon = page.locator("#filter-toggle-chevron");
  await expectDisclosureShape(filterIcon, "right");
  await expect(filterToggle).toHaveAccessibleName("Filters");
  expect(
    await filterIcon.evaluate(
      (icon) =>
        getComputedStyle(icon).stroke ===
        getComputedStyle(icon.parentElement!).color,
    ),
  ).toBe(true);

  await filterToggle.click();
  await expectDisclosureShape(filterIcon, "down");
  for (const icon of await page.locator(".fdd-btn .fdd-caret").all()) {
    await expectDisclosureShape(icon, "down");
  }

  const card = page.locator(".card", { hasText: "Wire up the auth flow" });
  await card.click();
  const sectionIcons = page.locator(".section-toggle .disclosure-icon");
  expect(await sectionIcons.count()).toBeGreaterThan(0);
  for (const icon of await sectionIcons.all()) {
    const direction = (await icon.getAttribute("data-direction")) as
      | "right"
      | "down";
    await expectDisclosureShape(icon, direction);
  }
  await page.locator("#drawer-close").click();

  await card.click({ button: "right" });
  const submenuIcons = page.locator(".ctxmenu-arrow");
  expect(await submenuIcons.count()).toBeGreaterThan(0);
  for (const icon of await submenuIcons.all()) {
    await expectDisclosureShape(icon, "right");
  }
});

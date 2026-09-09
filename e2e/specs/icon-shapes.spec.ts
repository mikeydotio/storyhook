import { expect, test } from "./support";
import { openProject, seedToken } from "./support";
import type { Locator, Page } from "@playwright/test";

/**
 * SH-620: dashboard controls use a small, explicit emoji vocabulary whose
 * meaning matches the action. Emoji are decorative; visible text or the
 * owning control's aria-label remains the accessible name. These tests run
 * in Chromium and WebKit and prove the source-level vocabulary actually
 * renders in the controls that consume it.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

async function expectEmoji(
  locator: Locator,
  kind: string,
  glyph: string,
): Promise<void> {
  await expect(locator).toBeVisible();
  await expect(locator).toHaveAttribute("data-emoji", kind);
  await expect(locator).toHaveAttribute("aria-hidden", "true");
  await expect(locator).toHaveText(glyph);
  const box = await locator.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.width).toBeGreaterThan(0);
  expect(box!.height).toBeGreaterThan(0);
}

function buttonEmoji(page: Page, id: string): Locator {
  return page.locator(`#${id} .emoji-icon`);
}

test("topbar and search controls use emoji that name their purpose", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");

  await expectEmoji(buttonEmoji(page, "home-btn"), "home", "🏠");
  await expectEmoji(buttonEmoji(page, "settings-btn"), "settings", "⚙️");
  await expectEmoji(buttonEmoji(page, "drafts-btn"), "drafts", "📝");
  await expectEmoji(page.locator("#search-wrap .emoji-icon"), "search", "🔍");

  await expect(page.locator("#home-btn")).toHaveAccessibleName("Home");
  await expect(page.locator("#settings-btn")).toHaveAccessibleName("Settings");
  await expect(page.locator("#drafts-btn")).toHaveAccessibleName(/Drafts/);
  await expect(page.locator("#search-input")).toHaveAccessibleName(
    "Search stories, IDs, and labels",
  );
});

test("sort, back, and close controls use action-specific emoji and keep names", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  const sort = page.locator('.column[data-state="todo"] .column-sort-btn');
  await expectEmoji(sort.locator(".emoji-icon"), "sort", "↕️");
  await expect(sort).toHaveAttribute("aria-label", /^Sort: /);

  const card = page.locator(".card", { hasText: "Wire up the auth flow" });
  await card.click();
  await expectEmoji(buttonEmoji(page, "drawer-close"), "close", "✖️");
  await expect(page.locator("#drawer-close")).toHaveAccessibleName(
    "Close story details",
  );
  await page.locator("#drawer-close").click();

  await page.locator("#settings-btn").click();
  await page
    .locator(".settings-table tbody tr", { hasText: "Alpha Project" })
    .getByRole("button", { name: "Statuses" })
    .click();
  const back = page.locator(".back-link");
  await expectEmoji(back.locator(".emoji-icon"), "back", "⬅️");
  await expect(back).toHaveAccessibleName("All projects");
});

test("disclosure emoji communicate each control's function and state", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");

  const projectDropdown = page.locator("#projsel-btn .emoji-icon");
  await expectEmoji(projectDropdown, "dropdown", "🔽");
  await expect(projectDropdown).toHaveAttribute("data-direction", "down");

  const filterToggle = page.locator("#filter-toggle-btn");
  const filterEmoji = page.locator("#filter-toggle-chevron");
  await expectEmoji(filterEmoji, "filters", "🎛️");
  await expect(filterEmoji).toHaveAttribute("data-direction", "right");
  await expect(filterToggle).toHaveAccessibleName("Filters");
  await filterToggle.click();
  await expect(filterEmoji).toHaveAttribute("data-direction", "down");

  for (const dropdown of await page.locator(".fdd-btn .emoji-icon").all()) {
    await expectEmoji(dropdown, "dropdown", "🔽");
    await expect(dropdown).toHaveAttribute("data-direction", "down");
  }

  const card = page.locator(".card", { hasText: "Wire up the auth flow" });
  await card.click();
  const sectionEmoji = page.locator(".section-toggle .emoji-icon");
  expect(await sectionEmoji.count()).toBeGreaterThan(0);
  for (const emoji of await sectionEmoji.all()) {
    const expanded = await emoji.getAttribute("data-direction");
    await expectEmoji(
      emoji,
      expanded === "down" ? "collapse" : "expand",
      expanded === "down" ? "➖" : "➕",
    );
  }
  await page.locator("#drawer-close").click();

  await card.click({ button: "right" });
  const submenuEmoji = page.locator(".ctxmenu-arrow");
  expect(await submenuEmoji.count()).toBeGreaterThan(0);
  for (const emoji of await submenuEmoji.all()) {
    await expectEmoji(emoji, "submenu", "➡️");
    await expect(emoji).toHaveAttribute("data-direction", "right");
  }
});

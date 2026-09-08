import { expect, test } from "./support";
import {
  backdropOf,
  cleanUpCreatedStories,
  contrastRatio,
  createStory,
  MIN_CONTRAST,
  openProject,
  openStatusesEditor,
  parseColor,
  resolvedTokenColor,
  seedToken,
  THEMES,
} from "./support";
import type { Locator, Page } from "@playwright/test";

/**
 * SH-601: six always-visible control families used `--fg-faint`, which in
 * the light palette paints at 2.60:1 on raised surfaces and 2.24:1 on sunken
 * ones. Measure the browser's resolved colours against the composited real
 * backdrop in every theme resolution, rather than proving only that CSS text
 * contains a preferred token name.
 */

cleanUpCreatedStories("Alpha Project");

const TEXT_CONTRAST = 4.5;

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

async function appearance(locator: Locator): Promise<{
  color: string;
  backgrounds: string[];
}> {
  await expect(locator).toBeVisible();
  return locator.evaluate((node) => {
    const backgrounds: string[] = [];
    for (let current: Element | null = node; current; current = current.parentElement) {
      backgrounds.push(getComputedStyle(current).backgroundColor);
    }
    return { color: getComputedStyle(node).color, backgrounds };
  });
}

async function expectContrast(
  locator: Locator,
  minimum: number,
  context: string,
): Promise<void> {
  const resolved = await appearance(locator);
  const ratio = contrastRatio(
    parseColor(resolved.color),
    backdropOf(resolved.backgrounds),
  );
  expect(
    ratio,
    `${context}: contrast was ${ratio.toFixed(2)}:1`,
  ).toBeGreaterThanOrEqual(minimum);
}

async function waitForSunkenColumn(page: Page): Promise<void> {
  const expected = await resolvedTokenColor(page, "--bg-sunken");
  await expect
    .poll(() =>
      page.locator(".column").first().evaluate((column) =>
        getComputedStyle(column).backgroundColor,
      ),
    )
    .toBe(expected);
}

test("board header controls meet contrast thresholds in every theme and state", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  const archive = page.locator('.column[data-state="done"] .column-archive-btn');
  const sort = page.locator('.column[data-state="todo"] .column-sort-btn');

  for (const theme of THEMES) {
    await theme.apply(page);
    await waitForSunkenColumn(page);
    await page.locator("#home-btn").hover();

    await expectContrast(archive, TEXT_CONTRAST, `${theme.name}: Archive at rest`);
    await expectContrast(sort, MIN_CONTRAST, `${theme.name}: column sort icon`);

    await archive.hover();
    await expectContrast(archive, TEXT_CONTRAST, `${theme.name}: Archive on hover`);
  }
});

test("drawer controls meet contrast thresholds in every theme", async ({ page }) => {
  await openProject(page, "Alpha Project");
  const targetTitle = "SH-601 contrast relation target";
  const title = "SH-601 drawer control contrast";
  const targetId = await createStory(page, targetTitle);
  await createStory(page, title);

  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await card.getByText(title, { exact: true }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const labelInput = page.locator('input[data-field="label-add"]');
  await labelInput.fill("contrast-check");
  await labelInput.press("Enter");
  const labelRemove = page.locator(".label-chip button");
  await expect(labelRemove).toHaveCount(1);

  const relationships = page
    .locator("#drawer-body > div")
    .filter({ has: page.locator(".section-toggle", { hasText: "Relationships" }) });
  await relationships.locator('input[data-field="relationship-id"]').fill(targetId);
  await relationships.locator("button.btn", { hasText: "Add" }).click();
  const relationRemove = relationships.locator(".rel-remove");
  await expect(relationRemove).toHaveCount(1);

  for (const theme of THEMES) {
    await theme.apply(page);
    await expectContrast(
      page.locator(".section-toggle").first(),
      TEXT_CONTRAST,
      `${theme.name}: section toggle`,
    );
    await expectContrast(
      labelRemove,
      TEXT_CONTRAST,
      `${theme.name}: label removal character`,
    );
    await expectContrast(
      relationRemove,
      TEXT_CONTRAST,
      `${theme.name}: relationship removal character`,
    );
  }
});

test("enabled status reorder controls meet text contrast in every theme", async ({
  page,
}) => {
  await openStatusesEditor(page, "Alpha Project");
  const enabledReorder = page.locator(".status-reorder button:not(:disabled)");
  await expect(enabledReorder.first()).toBeVisible();

  for (const theme of THEMES) {
    await theme.apply(page);
    for (let index = 0; index < await enabledReorder.count(); index += 1) {
      await expectContrast(
        enabledReorder.nth(index),
        TEXT_CONTRAST,
        `${theme.name}: enabled status reorder ${index + 1}`,
      );
    }
  }
});

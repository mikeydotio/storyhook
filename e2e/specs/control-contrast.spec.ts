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
  const backdrop = backdropOf(resolved.backgrounds);
  const ratio = contrastRatio(parseColor(resolved.color), backdrop);
  expect(
    ratio,
    `${context}: ${resolved.color} over ${JSON.stringify(backdrop)} was ${ratio.toFixed(2)}:1`,
  ).toBeGreaterThanOrEqual(minimum);
}

async function expectBoundaryContrast(
  locator: Locator,
  context: string,
): Promise<void> {
  await expect(locator).toBeVisible();
  const resolved = await locator.evaluate((node) => {
    const backgrounds: string[] = [];
    for (let current = node.parentElement; current; current = current.parentElement) {
      backgrounds.push(getComputedStyle(current).backgroundColor);
    }
    return {
      border: getComputedStyle(node).borderTopColor,
      backgrounds,
    };
  });
  const backdrop = backdropOf(resolved.backgrounds);
  const ratio = contrastRatio(parseColor(resolved.border), backdrop);
  expect(
    ratio,
    `${context}: ${resolved.border} over ${JSON.stringify(backdrop)} was ${ratio.toFixed(2)}:1`,
  ).toBeGreaterThanOrEqual(MIN_CONTRAST);
}

async function waitForBoundaryToken(
  page: Page,
  locator: Locator,
  token: string,
): Promise<void> {
  const expected = await resolvedTokenColor(page, token);
  await expect
    .poll(() => locator.evaluate((node) => getComputedStyle(node).borderTopColor))
    .toBe(expected);
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

test("shared control boundaries retain contrast on hover", async ({ page }) => {
  const projectSelector = page.locator("#projsel-btn");
  for (const theme of THEMES) {
    await theme.apply(page);
    await waitForBoundaryToken(page, projectSelector, "--control-boundary");
    await expectBoundaryContrast(projectSelector, `${theme.name}: project selector at rest`);
    await projectSelector.hover();
    await waitForBoundaryToken(page, projectSelector, "--fg-functional");
    await expectBoundaryContrast(projectSelector, `${theme.name}: project selector on hover`);
    await page.locator("#home-btn").hover();
  }

  await openProject(page, "Alpha Project");
  const filterToggle = page.locator("#filter-toggle-btn");
  await filterToggle.click();
  await expect(page.locator("#filter-panel")).toBeVisible();
  const filterDropdown = page.locator(".fdd-btn").first();

  for (const theme of THEMES) {
    await theme.apply(page);
    for (const [control, name] of [
      [filterToggle, "filter toggle"],
      [filterDropdown, "filter dropdown"],
    ] as const) {
      await page.locator(".card").first().hover();
      await waitForBoundaryToken(page, control, "--control-boundary");
      await expectBoundaryContrast(control, `${theme.name}: ${name} at rest`);
      await control.hover();
      await waitForBoundaryToken(page, control, "--fg-functional");
      await expectBoundaryContrast(control, `${theme.name}: ${name} on hover`);
    }
  }
});

test("shared hierarchy copy and control boundaries meet contrast in every theme", async ({
  page,
}) => {
  const homeCopy = [
    [page.locator(".home-stat span").first(), "Home summary label"],
    [page.locator(".repo-card-path").first(), "Home project path"],
    [page.locator(".repo-card-stats").first(), "Home project stats"],
  ] as const;
  for (const theme of THEMES) {
    await theme.apply(page);
    for (const [locator, label] of homeCopy) {
      await expectContrast(locator, TEXT_CONTRAST, `${theme.name}: ${label}`);
    }
  }

  await openProject(page, "Alpha Project");
  for (const theme of THEMES) {
    await theme.apply(page);
    await waitForSunkenColumn(page);
    await expectContrast(
      page.locator(".card-id").first(),
      TEXT_CONTRAST,
      `${theme.name}: board story id`,
    );
    await expectContrast(
      page.locator("#filter-count"),
      TEXT_CONTRAST,
      `${theme.name}: result count`,
    );
    await expectContrast(
      page.locator(".column-empty").first(),
      TEXT_CONTRAST,
      `${theme.name}: board empty state`,
    );
  }

  await page.locator(".card").first().click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  for (const theme of THEMES) {
    await theme.apply(page);
    await expectContrast(
      page.locator(".field label").first(),
      TEXT_CONTRAST,
      `${theme.name}: drawer field label`,
    );
    await expectContrast(
      page.locator(".section-toggle").first(),
      TEXT_CONTRAST,
      `${theme.name}: drawer section label`,
    );
    await expectBoundaryContrast(
      page.locator('.field select, .field input[type="text"]').first(),
      `${theme.name}: drawer control`,
    );
  }
  await page.locator("#drawer-close").click();

  await page.locator('#view-toggle [data-view="list"]').click();
  for (const theme of THEMES) {
    await theme.apply(page);
    await expectContrast(
      page.locator("thead th").first(),
      TEXT_CONTRAST,
      `${theme.name}: list column label`,
    );
    await expectContrast(
      page.locator(".col-order").first(),
      TEXT_CONTRAST,
      `${theme.name}: list order metadata`,
    );
    await expectContrast(
      page.locator(".state-pill").first(),
      TEXT_CONTRAST,
      `${theme.name}: list state metadata`,
    );
  }

  await page.locator("#new-story-btn").click();
  const modalLabelGap = await page.locator("#create-modal .field").first().evaluate((field) => {
    const label = field.querySelector("label")!.getBoundingClientRect();
    const control = field.querySelector("input, select, textarea")!.getBoundingClientRect();
    return control.top - label.bottom;
  });
  expect(modalLabelGap, "create form label/value gap").toBeCloseTo(4, 1);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-error")).toHaveText("Title is required.");
  for (const theme of THEMES) {
    await theme.apply(page);
    await expectContrast(
      page.locator("#create-modal .field label").first(),
      TEXT_CONTRAST,
      `${theme.name}: create field label`,
    );
    await expectContrast(
      page.locator("#create-error"),
      TEXT_CONTRAST,
      `${theme.name}: create error`,
    );
    await expectBoundaryContrast(
      page.locator("#create-title"),
      `${theme.name}: create title control`,
    );
  }
  await page.locator("#create-discard").click();

  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  for (const theme of THEMES) {
    await theme.apply(page);
    await expectContrast(
      page.locator(".settings-hint").first(),
      TEXT_CONTRAST,
      `${theme.name}: Settings hint`,
    );
    await expectContrast(
      page.locator(".settings-form label").first(),
      TEXT_CONTRAST,
      `${theme.name}: Settings field label`,
    );
    await expectBoundaryContrast(
      page.locator(".settings-form input").first(),
      `${theme.name}: Settings control`,
    );
  }
});

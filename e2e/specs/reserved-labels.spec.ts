import { test, expect } from "./support";
import type { Locator, Page } from "@playwright/test";
import {
  THEMES,
  awaitNoOverlay,
  backdropOf,
  cleanUpCreatedStories,
  contrastRatio,
  openProject,
  parseColor,
  seedToken,
} from "./support";

/**
 * Exercises SH-454: the two reserved label names (`no-auto`, `human-only`)
 * render orange everywhere the dashboard draws a label, because decision D12
 * of the Full Auto epic says they mean "a person is required here".
 *
 * `tests/dashboard_reserved_labels.rs` pins the wiring statically — that the
 * browser's name list matches `domain::RESERVED_LABELS`, that `--warn-soft`
 * is declared in every palette block, and that exactly one place builds the
 * class. None of that proves a pixel. This is the layer that opens a real
 * browser and asks whether the tint actually *resolved*, which matters more
 * than it sounds: an undefined custom property is not an error, it paints as
 * nothing, so a chip whose token is missing in one theme silently falls back
 * to looking like every other label. That failure has no console message and
 * no visual cue that anything is wrong — only a measurement finds it.
 *
 * Four render sites, all four covered here: the board card, the drawer, List
 * view, and the create modal's own label combobox.
 *
 * This spec creates and deletes its own story rather than labelling a seeded
 * one: Alpha's exact two-story shape is asserted byte-for-byte by
 * filter-persistence.spec.ts and column-visibility.spec.ts, per
 * run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

/** The reserved names, and one ordinary label as the control.
 *
 * Spelled here rather than imported: this spec is the outside check on the
 * page's own list, so reading that list to test it would only prove the page
 * agrees with itself. The Rust fence is what keeps these honest against
 * `domain::RESERVED_LABELS`. */
const RESERVED = ["human-only", "no-auto"];
const ORDINARY = "web";

/** The lowest contrast this project's own chips meet today: `--fg-muted` on
 * `--bg-sunken` measures 4.16:1 in light and 6.77:1 in dark, and the story
 * asks the reserved chip to meet the same bar. The reserved pair is specified
 * well past it — 4.54:1 light, 5.12:1 dark — so this asserts the WCAG AA
 * threshold for normal text instead, which is the stronger of the two claims
 * and the one worth defending. */
const MIN_CONTRAST = 4.5;

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

/** Opens the create modal, types every label into the combobox, and returns
 * the modal's own chips — the fourth render site, measured before submit
 * because these chips exist only while the form is open. */
async function fillCreateForm(page: Page, title: string): Promise<Locator> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  // One comma-separated fill: `addLabel` splits on comma, so this commits
  // three chips in a single Enter (SH-164's own paste path).
  const labelInput = page.locator("#create-labels-field .label-combobox input");
  await labelInput.fill([ORDINARY, ...RESERVED].join(","));
  await labelInput.press("Enter");
  const chips = page.locator("#create-labels-field .label-chip");
  await expect(chips).toHaveCount(3);
  return chips;
}

/** Submits the filled create form and returns the new card. */
async function submit(page: Page, title: string): Promise<Locator> {
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await expect(card).toBeVisible();
  await awaitNoOverlay(page);
  return card;
}

/** Asserts that exactly the reserved chips among `chips` carry the tint class.
 *
 * Both directions on purpose: a rule that tinted every chip would satisfy
 * "the reserved ones are tinted" and destroy the distinction the story
 * exists to draw. */
async function onlyReservedAreTinted(chips: Locator, where: string): Promise<void> {
  const count = await chips.count();
  expect(count, `${where} must render every label`).toBe(3);
  for (let i = 0; i < count; i++) {
    const chip = chips.nth(i);
    const text = ((await chip.textContent()) ?? "").replace("×", "").trim();
    const tinted = (await chip.getAttribute("class"))!.split(/\s+/).includes("chip-reserved");
    expect(tinted, `${where}: "${text}" should ${RESERVED.includes(text) ? "" : "not "}be tinted`).toBe(
      RESERVED.includes(text),
    );
  }
}

test("every label render site tints exactly the reserved names", async ({ page }) => {
  const title = `SH-454 reserved labels ${Date.now()}`;

  // Site 4: the create modal's combobox, while the form is still open.
  await onlyReservedAreTinted(await fillCreateForm(page, title), "the create modal");

  // Site 1: the board card.
  const card = await submit(page, title);
  await onlyReservedAreTinted(card.locator(".card-labels .chip"), "the board card");

  // Site 3: the detail drawer.
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await onlyReservedAreTinted(page.locator("#drawer-body .label-chip"), "the drawer");
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // Site 2: List view.
  await page.locator('#view-toggle button[data-view="list"]').click();
  const row = page.locator("tr[data-id]", { hasText: title });
  await expect(row).toBeVisible();
  await onlyReservedAreTinted(row.locator(".chip"), "List view");
});

test("a reserved label is never collapsed into the card's overflow chip", async ({ page }) => {
  const title = `SH-454 overflow ${Date.now()}`;
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  // Four ordinary labels ahead of the reserved one: without the reserved-first
  // ordering the card shows three of them and hides `human-only` behind "+2",
  // which is a tint nobody can see.
  const labelInput = page.locator("#create-labels-field .label-combobox input");
  await labelInput.fill("alpha,beta,gamma,delta,human-only");
  await labelInput.press("Enter");
  const card = await submit(page, title);

  const chips = card.locator(".card-labels .chip");
  await expect(chips).toHaveCount(4); // three labels plus the "+2" overflow
  await expect(chips.first()).toHaveText("human-only");
  await expect(chips.first()).toHaveClass(/chip-reserved/);
  await expect(chips.last()).toHaveText("+2");
});

test("the reserved tint resolves and stays readable in all four themes", async ({ page }) => {
  const title = `SH-454 contrast ${Date.now()}`;
  await fillCreateForm(page, title);
  const card = await submit(page, title);

  const reserved = card.locator(".card-labels .chip-reserved").first();
  const plain = card.locator(`.card-labels .chip:not(.chip-reserved)`).first();
  await expect(reserved).toBeVisible();
  await expect(plain).toBeVisible();

  for (const theme of THEMES) {
    await theme.apply(page);

    const probe = async (chip: Locator) =>
      chip.evaluate((node: Element) => {
        const style = getComputedStyle(node);
        // The chip's own background first, then every ancestor's, so a
        // transparent or semi-transparent chip is composited against what is
        // really behind it rather than measured against nothing. Same walk
        // `probeIndicator` does for a focus ring.
        const backgrounds: string[] = [style.backgroundColor];
        for (let a = node.parentElement; a; a = a.parentElement) {
          backgrounds.push(getComputedStyle(a).backgroundColor);
        }
        return { color: style.color, backgrounds };
      });

    const tinted = await probe(reserved);
    const neutral = await probe(plain);

    const ink = parseColor(tinted.color);
    const behind = backdropOf(tinted.backgrounds);
    const ratio = contrastRatio(ink, behind);
    expect(
      ratio,
      `${theme.name}: the reserved chip's text must stay readable — ${tinted.color} on ${tinted.backgrounds[0]}`,
    ).toBeGreaterThanOrEqual(MIN_CONTRAST);

    // The tint has to have *resolved*. An undefined `--warn-soft` paints as
    // nothing at all rather than erroring, and a fully transparent chip would
    // inherit the card behind it and could still pass the contrast check
    // above — so the reserved chip and an ordinary one differing is the
    // assertion that actually catches a missing token.
    expect(
      tinted.backgrounds[0],
      `${theme.name}: the reserved chip must not paint the same background as an ordinary one`,
    ).not.toBe(neutral.backgrounds[0]);
    expect(
      tinted.color,
      `${theme.name}: the reserved chip must not use the ordinary chip's ink`,
    ).not.toBe(neutral.color);
  }
});

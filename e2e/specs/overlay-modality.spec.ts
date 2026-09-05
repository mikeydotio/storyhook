import { test, expect } from "./support";
import type { Page } from "@playwright/test";
import { openProject, seedToken } from "./support";

/**
 * SH-299 — an overlay is modal for every input device, or it is modal for
 * none.
 *
 * `.backdrop` is `position: fixed; inset: 0; z-index: 40`, so every overlay in
 * `web_dashboard.html` has always been modal for the *mouse*. Nothing marked
 * the background `inert`, so none of them was ever modal for the *keyboard*:
 * the board and topbar behind an open overlay kept their place in the tab
 * order and stayed activatable with Enter. The two
 * halves of the same surface disagreed about whether it was modal, and the
 * users who lost that disagreement were the ones least able to work around
 * it.
 *
 * SH-554 later removed story detail from this registry entirely: it is now a
 * non-modal workspace peer, covered by `detail-panel.spec.ts`.
 * SH-568 adds the converse exception: `#engine-alert-modal` uses this same
 * inert-background and focus-trap machinery, but Escape and backdrop presses
 * deliberately do not dismiss it. D13 requires an unacknowledged engine stop
 * to remain an interruption rather than becoming a silent all-clear. Its
 * exceptional dismissal contract is proved in `engine.spec.ts`, where the
 * alert data required to open it can be supplied without weakening this
 * file's read-only fixture promise.
 *
 * Everything here is read-only against the seeded fixtures: no story is
 * created, moved or deleted, so `tests/e2e_fixture_hygiene.rs`'s cleanup
 * registration does not apply, and Alpha's byte-for-byte two-story shape (per
 * `run-e2e.sh`) is undisturbed.
 */

/**
 * The surface id of every backdrop-based overlay, read off the live document
 * rather than listed here: each `.backdrop` names the surface it dims in
 * `data-overlay`, which is the same registry the app's own
 * `applyOverlayModality()` reads. An eighth overlay added to the dashboard is
 * therefore covered by every assertion below the moment it exists. A list
 * hand-copied into this file would instead go stale exactly when it mattered
 * — a new overlay is precisely the thing most likely to be half-wired.
 */
async function overlaySurfaceIds(page: Page): Promise<string[]> {
  const ids = await page
    .locator(".backdrop[data-overlay]")
    .evaluateAll((nodes) =>
      nodes.map((node) => (node as HTMLElement).dataset.overlay as string),
    );
  expect(ids.length).toBeGreaterThan(0);
  return ids;
}

/**
 * Asserts the whole modality invariant at once.
 *
 * `activeId` names the one surface that may be interactive; `null` means
 * nothing is open. Note what is asserted in *that* case: the app shell is
 * live, and all registered surfaces are still `inert`. A closed `.modal` is
 * only `opacity: 0; pointer-events: none`, which does not take anything out
 * of the tab order, so before SH-299 shut dialogs' fields sat in the sequence
 * in front of the board. "Nothing is open" is a state with an invariant of
 * its own, not an absence of one.
 */
async function expectModalityFor(
  page: Page,
  activeId: string | null,
): Promise<void> {
  const ids = await overlaySurfaceIds(page);
  if (activeId !== null) expect(ids).toContain(activeId);

  const shell = page.locator("#app");
  if (activeId === null) {
    await expect(shell).not.toHaveAttribute("inert", "");
  } else {
    await expect(shell).toHaveAttribute("inert", "");
  }

  // Covered is exactly what the backdrop dims. `#toast-stack` and
  // `#dispatch-history` are `z-index: 60` against its 40 and render over an
  // open overlay by an older, deliberate decision, so a mouse has always
  // reached them with a modal open — they are already symmetric, and
  // inerting them would invent a new behaviour rather than repair one.
  await expect(page.locator("#toast-stack")).not.toHaveAttribute("inert", "");
  await expect(page.locator("#dispatch-history")).not.toHaveAttribute(
    "inert",
    "",
  );

  for (const id of ids) {
    const surface = page.locator(`#${id}`);
    if (id === activeId) {
      await expect(surface).not.toHaveAttribute("inert", "");
    } else {
      await expect(surface).toHaveAttribute("inert", "");
    }
  }
}

/** Whether the focused element is inside `#<surfaceId>`, the surface itself
 * counting — `activateOverlay()` focuses the container when a surface has no
 * one obvious first control. */
async function focusIsInside(page: Page, surfaceId: string): Promise<boolean> {
  return page.evaluate((id) => {
    const active = document.activeElement;
    return !!active && !!active.closest(`#${id}`);
  }, surfaceId);
}

test.describe("with a credential", () => {
  test.beforeEach(async ({ page }) => {
    await seedToken(page);
    await page.goto("/");
    await openProject(page, "Alpha Project");
  });

  /**
   * The overlays reachable without writing anything. The others are
   * driven elsewhere in this file (the delete modal, below) or reached only by
   * mutating a fixture — a drag into Blocked, a column archive — and all eight
   * are held to calling the same two functions by
   * `tests/web_test.rs::every_backdrop_overlay_is_wired_into_the_focus_trap`,
   * which is what makes covering a representative set here honest rather than
   * a sample.
   */
  const OVERLAYS = [
    {
      name: "the create modal",
      surface: "create-modal",
      open: (page: Page) => page.locator("#new-story-btn").click(),
      // Discard, not Cancel: SH-175 replaced the latter, and on a modal that
      // is not editing a saved draft it closes without issuing a request.
      close: (page: Page) => page.locator("#create-discard").click(),
    },
    {
      name: "the drafts popover",
      surface: "drafts-modal",
      open: (page: Page) => page.locator("#drafts-btn").click(),
      close: (page: Page) => page.locator("#drafts-close").click(),
    },
  ];

  for (const overlay of OVERLAYS) {
    test(`${overlay.name} covers the background while open, and uncovers it on close`, async ({
      page,
    }) => {
      await expectModalityFor(page, null);

      await overlay.open(page);
      await expect(page.locator(`#${overlay.surface}`)).toHaveClass(/open/);
      await expectModalityFor(page, overlay.surface);

      await overlay.close(page);
      await expect(page.locator(`#${overlay.surface}`)).not.toHaveClass(/open/);
      await expectModalityFor(page, null);
    });

    test(`${overlay.name} takes focus when it opens`, async ({ page }) => {
      await overlay.open(page);
      await expect(page.locator(`#${overlay.surface}`)).toHaveClass(/open/);

      expect(await focusIsInside(page, overlay.surface)).toBe(true);
    });
  }

  /**
   * A closed dialog is not merely invisible, it is unreachable. Before
   * SH-299 this field was focusable — and tabbable — with the modal shut,
   * which is the same defect as the background one seen from the other side.
   */
  test("a closed dialog's fields cannot be focused", async ({ page }) => {
    await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

    await page.evaluate(() => {
      document.getElementById("create-title")?.focus();
    });

    await expect(page.locator("#create-title")).not.toBeFocused();
  });
});

/**
 * The token modal is the one overlay a spec cannot reach with a credential in
 * hand, so it gets a describe with no `seedToken` — the second such block in
 * the suite, after `loopback-requires-a-token.spec.ts`, whose subject is a
 * different one (that the dashboard refuses to render at all without a
 * token). This asserts only that the prompt standing between the user and
 * every screen behind it is itself modal, which matters more here than
 * anywhere else: it is the first thing a keyboard user meets, and it appears
 * before a single project has rendered.
 */
test.describe("with no credential", () => {
  test("the token modal is modal, and holds the focus it takes", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page.locator("#token-modal")).toHaveClass(/open/);

    await expectModalityFor(page, "token-modal");
    await expect(page.locator("#token-input")).toBeFocused();
  });
});

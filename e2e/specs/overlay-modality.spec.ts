import { test, expect } from "./support";
import type { Page } from "@playwright/test";
import { fullKeyboardAccess, openProject, seedToken } from "./support";

/**
 * SH-299 — an overlay is modal for every input device, or it is modal for
 * none.
 *
 * `.backdrop` is `position: fixed; inset: 0; z-index: 40`, so every overlay in
 * `web_dashboard.html` has always been modal for the *mouse*. Nothing marked
 * the background `inert`, so none of them was ever modal for the *keyboard*:
 * the board, the topbar and the settings table behind an open drawer kept
 * their place in the tab order and stayed activatable with Enter. The two
 * halves of the same surface disagreed about whether it was modal, and the
 * users who lost that disagreement were the ones least able to work around
 * it.
 *
 * That asymmetry is also the mechanism SH-290's defect was reachable through:
 * with a drawer open over Settings, the Statuses button could not be clicked
 * (the click landed on the backdrop, which dismissed the drawer) and could be
 * pressed. `drawer-screen-scope.spec.ts` was built on exactly that route and
 * had to be re-driven when this closed it — see its own header.
 *
 * Everything here is read-only against the seeded fixtures: no story is
 * created, moved or deleted, so `tests/e2e_fixture_hygiene.rs`'s cleanup
 * registration does not apply, and Alpha's byte-for-byte two-story shape (per
 * `run-e2e.sh`) is undisturbed.
 */

/** Alpha's first seeded story — opened, inspected, and left exactly as found. */
const ALPHA_CARD_TITLE = "Wire up the auth flow";

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
 * live, and all seven surfaces are still `inert`. A closed `.modal` is only
 * `opacity: 0; pointer-events: none` and a closed `.drawer` only
 * `translateX(100%)`, neither of which takes anything out of the tab order,
 * so before SH-299 six shut dialogs' fields sat in the sequence in front of
 * the board. "Nothing is open" is a state with an invariant of its own, not
 * an absence of one.
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
  // reached them with a drawer open — they are already symmetric, and
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
   * The three overlays reachable without writing anything. The other four are
   * driven elsewhere in this file (the delete modal, below) or reached only by
   * mutating a fixture — a drag into Blocked, a column archive — and all seven
   * are held to calling the same two functions by
   * `tests/web_test.rs::every_backdrop_overlay_is_wired_into_the_focus_trap`,
   * which is what makes covering a representative set here honest rather than
   * a sample.
   */
  const OVERLAYS = [
    {
      name: "the drawer",
      surface: "drawer",
      open: (page: Page) =>
        page.locator(".card-title", { hasText: ALPHA_CARD_TITLE }).click(),
      close: (page: Page) => page.locator("#drawer-close").click(),
    },
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
   * The property the story is named for, asserted against the exact control
   * SH-290 rode in on. `#settings-btn` is asked to take focus through the DOM
   * API itself rather than through a Playwright action, so what is being
   * proved is that the *platform* refuses — an inert element is not a
   * focusable area — rather than that a helper declined to try.
   */
  test("no background control can be focused while the drawer is open", async ({
    page,
  }) => {
    await page.locator(".card-title", { hasText: ALPHA_CARD_TITLE }).click();
    await expect(page.locator("#drawer")).toHaveClass(/open/);

    for (const id of ["settings-btn", "home-btn", "new-story-btn"]) {
      await page.evaluate((target) => {
        document.getElementById(target)?.focus();
      }, id);
      await expect(page.locator(`#${id}`)).not.toBeFocused();
    }
    expect(await focusIsInside(page, "drawer")).toBe(true);
  });

  /**
   * The same property from the user's side rather than the API's: Tab, over
   * and over, never leaves the drawer. Twenty presses is more than the drawer
   * holds focusable controls, so this crosses whatever wrap the browser
   * performs at the end of the document — which is the moment a hole in the
   * trap would show, and the one a single Tab would miss.
   *
   * "Never leaves the drawer" is exact here rather than approximate: the two
   * uncovered regions (`#toast-stack`, `#dispatch-history`) are empty in this
   * arrangement, and the first holds nothing focusable in any arrangement, so
   * the only thing a stray Tab could reach is the dimmed background.
   */
  test("Tab never escapes the open drawer", async ({ page, browserName }) => {
    // WebKit's Tab order skips buttons and links unless this machine has
    // Full Keyboard Access on (`AppleKeyboardUIMode >= 2`) -- real Safari's
    // own out-of-box behavior, not a bug this suite can assert around
    // (SH-335 -- story show SH-335 carries the verdict). The trap
    // itself is unconditional in `web_dashboard.html`; what's untestable
    // here without that setting is Tab actually reaching every control it
    // traps. Fully load-bearing on `chromium`, and on a `webkit` this
    // machine has configured for full keyboard access.
    test.skip(
      browserName === "webkit" && !fullKeyboardAccess(),
      "WebKit's Tab order skips buttons/links unless AppleKeyboardUIMode>=2 (SH-335)",
    );
    await page.locator(".card-title", { hasText: ALPHA_CARD_TITLE }).click();
    await expect(page.locator("#drawer")).toHaveClass(/open/);

    for (let press = 1; press <= 20; press += 1) {
      await page.keyboard.press("Tab");
      expect(
        await focusIsInside(page, "drawer"),
        `focus left the drawer after ${press} Tab press(es)`,
      ).toBe(true);
    }
  });

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

  /**
   * Focus goes in on open and comes back out on close, driven entirely from
   * the keyboard — the input device the whole story is about. Enter on a
   * focused card opens the drawer (SH-197's roving tabindex); Escape closes
   * it, and the card that opened it is where the user is put back.
   */
  test("the drawer hands focus back to the card that opened it", async ({
    page,
  }) => {
    const card = page.locator(".card", { hasText: ALPHA_CARD_TITLE });
    await card.focus();
    await expect(card).toBeFocused();

    await page.keyboard.press("Enter");
    await expect(page.locator("#drawer")).toHaveClass(/open/);
    expect(await focusIsInside(page, "drawer")).toBe(true);

    await page.keyboard.press("Escape");
    await expect(page.locator("#drawer")).not.toHaveClass(/open/);
    await expect(card).toBeFocused();
  });

  /**
   * Overlays nest, so the trap is a stack rather than a flag: the drawer's own
   * footer opens the delete modal over it, and while that modal is up the
   * drawer underneath must be as inert as the board. Closing it hands the
   * drawer back, live, with focus on the button that left.
   *
   * Cancel throughout — nothing is deleted, and the fixture story this opens
   * is the same one every other test here opens.
   */
  test("a modal opened from the drawer covers the drawer, then gives it back", async ({
    page,
    browserName,
  }) => {
    // The same macOS setting governs both halves of this assertion: without
    // Full Keyboard Access, WebKit does not focus a `<button>` on click, so
    // `deleteButton.click()` below never makes it the element
    // `activateOverlay()` captures to restore -- the closing assertion would
    // find focus back on whatever WAS active before, not this button. That
    // is the exact WebKit behavior SH-324/SH-334 are about; asserting past
    // it here needs the same machine configuration `fullKeyboardAccess()`
    // reports (SH-335 -- story show SH-335 carries the verdict).
    test.skip(
      browserName === "webkit" && !fullKeyboardAccess(),
      "WebKit doesn't focus a <button> on click unless AppleKeyboardUIMode>=2 (SH-335)",
    );
    await page.locator(".card-title", { hasText: ALPHA_CARD_TITLE }).click();
    await expect(page.locator("#drawer")).toHaveClass(/open/);

    const deleteButton = page.locator("#drawer-footer button", {
      hasText: "Delete",
    });
    await deleteButton.click();
    await expect(page.locator("#delete-modal")).toHaveClass(/open/);

    await expectModalityFor(page, "delete-modal");
    await expect(page.locator("#delete-reason")).toBeFocused();

    await page.locator("#delete-modal-cancel").click();
    await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
    await expectModalityFor(page, "drawer");
    await expect(deleteButton).toBeFocused();
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

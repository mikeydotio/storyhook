import { test, expect } from "@playwright/test";
import type { Page, Request } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  deleteStatus,
  openDeleteModal,
  openProject,
  openStatuses,
  refuseTheFirstReposRead,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * SH-368 — a key the user's input method is still composing reaches nothing.
 *
 * The sibling of `modal-enter-autorepeat.spec.ts` (SH-362) and, deliberately, a
 * separate file: `event.repeat` and `event.isComposing` are different bits
 * describing different things, and the story was carved out of SH-362 precisely
 * so that its both-directions mutation battery stayed unambiguous about which
 * guard each test kills. A composition-commit Enter is `repeat: false` — one
 * deliberate keypress — so SH-362's guard passes it through, correctly, having
 * no opinion about it.
 *
 * ## The defect
 *
 * Composing text through an IME — Japanese, Chinese, Korean, and every other
 * input method with a composition step — ends by pressing a key that *commits*
 * what has been composed so far. That key is delivered to the page as an
 * ordinary `keydown`: `key: "Enter"`, `repeat: false`, and one bit to tell it
 * apart from a deliberate submit, `isComposing: true`. Nothing in this file's
 * handlers read that bit, so a user still mid-word had their form submitted
 * with whatever partial text the composition had produced.
 *
 * Escape is the same defect wearing the other key. It cancels a composition,
 * and the dashboard's page-level handler read it as "close whatever is open" —
 * so cancelling a mis-typed reading threw away the modal, and the text in it.
 *
 * ## The sites
 *
 * | Surface | What a composing key used to do |
 * |---|---|
 * | `#create-title` | created the story, part-titled |
 * | `#delete-reason` | deleted the story, part-reasoned |
 * | `#token-input` | exchanged a part-pasted token |
 * | `[data-field="label-add"]` | POSTed a part-typed label |
 * | `.status-add-slug` | created a part-named status |
 * | `#drop-blocked-reason-input` | moved the card, part-reasoned |
 * | `[data-field="title"]` (drawer) | blurred, PATCHing a part-typed title |
 * | the create modal's label combobox | added a part-typed chip |
 * | the description textarea (Escape) | reverted the edit and left edit mode |
 * | any field, page-level (Escape) | closed the drawer and every open modal |
 *
 * The first six are the story as filed; all six funnel through
 * `bindEnterSubmit`. The last four were found by reading the file's other
 * keydown handlers while fixing them and adopted rather than filed, per this
 * project's scope rubric: same mechanism, same file, and found by being here.
 * They are the reason the guard sits in `bindTypedKeys` — one door for every
 * listener that can see a field's keys — rather than in `bindEnterSubmit`,
 * where the story expected it.
 *
 * ## What these tests are, and what they are not
 *
 * **They dispatch a constructed `KeyboardEvent` carrying the composing bit.
 * They do not drive a real input method.** No browser automation protocol can:
 * a composition is a conversation between the OS input method and the engine,
 * and Playwright has no way to start one. So what is proved here is that the
 * page does the right thing with an event that says it is part of a
 * composition — the whole of the guard, and none of the platform below it. The
 * same disclaimer `modal-enter-autorepeat.spec.ts` carries about `repeat`, for
 * the same reason, and it is worth being exact about the residue: if some
 * engine were ever to deliver a commit key with the bit *unset*, every test
 * here would still pass and the defect would still be live.
 *
 * `the constructed event really carries the bit` is the control that keeps the
 * rest non-vacuous in the other direction. An engine that ignored the
 * `isComposing` init member would hand every test below an ordinary Enter,
 * which the page would submit — so those tests fail loudly rather than
 * silently, but they would fail naming the dashboard, and the defect would be
 * in the harness.
 *
 * Every site is asserted in **both** directions, per SH-362's precedent: a
 * guard that made Enter never submit anything satisfies the composing half of
 * every test here perfectly. The twin is the same event, dispatched the same
 * way, with only the one bit changed.
 */

const CREATE_STORY = matches("POST", /\/api\/repos\/[^/]+\/story$/);
const DELETE_STORY = matches("DELETE", /\/api\/repos\/[^/]+\/story\/[^/]+$/);
const PATCH_STORY = matches("PATCH", /\/api\/repos\/[^/]+\/story\/[^/]+$/);
const EXCHANGE_TOKEN = matches("POST", /\/token$/);
const ADD_LABEL = matches("POST", /\/api\/repos\/[^/]+\/story\/[^/]+\/labels$/);
const CREATE_STATUS = matches("POST", /\/api\/repos\/[^/]+\/states$/);
const MOVE_STORY = matches("POST", /\/api\/repos\/[^/]+\/story\/[^/]+\/move$/);

/** The project the add-status test mints scratch statuses in — Delta, for the
 * reason `status-destination-prompt.spec.ts` gives: it has a checkout (writes
 * to a checkout-less project answer 422), and nothing asserts a column shape
 * for it byte-for-byte the way Alpha's fixture is asserted on. */
const SCRATCH_PROJECT = "Delta Project";

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

// ============================================================
// The control: the harness can set the bit at all
// ============================================================

test("the constructed event really carries the bit", async ({ page }) => {
  // Every assertion in this file is a claim about what the page does with
  // `isComposing`, and all of them are claims about nothing if the engine
  // drops the init member — a `KeyboardEvent` whose bit is always `false` is
  // just an Enter, and "the page ignored it" would be a statement about the
  // constructor. This is that measurement, made once, in the engine actually
  // running (SH-364: a check that agrees with itself passes while matching
  // zero rows).
  await page.goto("/");
  const bits = await page.evaluate(() => [
    new KeyboardEvent("keydown", { key: "Enter", isComposing: true }).isComposing,
    new KeyboardEvent("keydown", { key: "Enter", isComposing: false }).isComposing,
  ]);
  expect(bits, "KeyboardEventInit.isComposing must reach the constructed event").toEqual([
    true,
    false,
  ]);
});

// ============================================================
// The six submitting sites
// ============================================================

test("a composing Enter in the create modal creates nothing", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-368 — composing create ${Date.now()}`;
  const creates = count(page, CREATE_STORY);

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await dispatch(page, "#create-title", "Enter", true);

  expect(creates(), "a composing Enter must not POST a story").toBe(0);
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  await dispatch(page, "#create-title", "Enter", false);
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
  expect(creates(), "the same Enter without the bit must still create").toBe(1);
});

test("a composing Enter in the delete modal deletes nothing", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-368 — composing delete ${Date.now()}`;
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  const deletes = count(page, DELETE_STORY);

  await openDeleteModal(page, card, "SH-368 composing");
  await dispatch(page, "#delete-reason", "Enter", true);

  expect(deletes(), "a composing Enter must not DELETE").toBe(0);
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await expect(card).toBeVisible();

  await dispatch(page, "#delete-reason", "Enter", false);
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(card).not.toBeVisible();
  expect(deletes(), "the same Enter without the bit must still delete").toBe(1);
});

test("a composing Enter in the token modal exchanges nothing", async ({ page }) => {
  await refuseTheFirstReposRead(page);
  await page.goto("/");
  await expect(page.locator("#token-modal")).toHaveClass(/open/);
  const exchanges = count(page, EXCHANGE_TOKEN);

  await page.locator("#token-input").fill(requiredEnv("DASHBOARD_NAMED_TOKEN"));
  await dispatch(page, "#token-input", "Enter", true);

  expect(exchanges(), "a composing Enter must not POST /token").toBe(0);
  await expect(page.locator("#token-modal")).toHaveClass(/open/);

  await dispatch(page, "#token-input", "Enter", false);
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
  expect(exchanges(), "the same Enter without the bit must still exchange").toBe(1);
});

test("a composing Enter in the drawer's label field labels nothing", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-368 — composing label ${Date.now()}`;
  await createStory(page, title);
  await page.locator('.column[data-state="todo"] .card', { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  const labels = count(page, ADD_LABEL);

  const field = '#drawer [data-field="label-add"]';
  await page.locator(field).fill("sh368-composing");
  await dispatch(page, field, "Enter", true);

  expect(labels(), "a composing Enter must not POST a label").toBe(0);
  await expect(
    page.locator("#drawer .label-chip", { hasText: "sh368-composing" }),
  ).toHaveCount(0);
  expect(
    await page.locator(field).inputValue(),
    "and it must leave the composed text in the field, which is where the user is still typing",
  ).toBe("sh368-composing");

  await dispatch(page, field, "Enter", false);
  await expect(
    page.locator("#drawer .label-chip", { hasText: "sh368-composing" }),
  ).toBeVisible();
  expect(labels(), "the same Enter without the bit must still label").toBe(1);
});

test("a composing Enter in the add-status form creates nothing", async ({ page, request }) => {
  await page.goto("/");
  await openStatuses(page, SCRATCH_PROJECT);
  const slug = `sh368c-${Date.now()}`;
  const statuses = count(page, CREATE_STATUS);

  await page.locator(".status-add-slug").fill(slug);
  await dispatch(page, ".status-add-slug", "Enter", true);

  expect(statuses(), "a composing Enter must not POST a status").toBe(0);
  await expect(page.locator(".status-row", { hasText: slug })).toHaveCount(0);

  await dispatch(page, ".status-add-slug", "Enter", false);
  await expect(page.locator(".status-row", { hasText: slug })).toBeVisible();
  expect(statuses(), "the same Enter without the bit must still create").toBe(1);
  await deleteStatus(request, SCRATCH_PROJECT, slug);
});

test("a composing Enter in the blocked-drop reason modal moves nothing", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-368 — composing move ${Date.now()}`;
  await createStory(page, title);
  const moves = count(page, MOVE_STORY);

  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .dragTo(page.locator('.column[data-state="blocked"]'));
  await expect(page.locator("#drop-blocked-reason-modal")).toHaveClass(/open/);
  await page.locator("#drop-blocked-reason-input").fill("SH-368 composing");
  await dispatch(page, "#drop-blocked-reason-input", "Enter", true);

  expect(moves(), "a composing Enter must not move the card").toBe(0);
  await expect(page.locator("#drop-blocked-reason-modal")).toHaveClass(/open/);

  await dispatch(page, "#drop-blocked-reason-input", "Enter", false);
  await expect(
    page.locator('.column[data-state="blocked"] .card', { hasText: title }),
  ).toBeVisible();
  expect(moves(), "the same Enter without the bit must still move").toBe(1);
});

// ============================================================
// The four adopted sites — no submit function anywhere near them
// ============================================================

test("a composing Enter in the drawer title neither blurs nor saves", async ({ page }) => {
  // Enter here does not submit: it calls `input.blur()`, and the field's own
  // blur handler is what PATCHes. A guard written only into `bindEnterSubmit`
  // would leave this exactly as broken as it was, which is why the fix went a
  // layer down.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-368 — composing title ${Date.now()}`;
  await createStory(page, title);
  await page.locator('.column[data-state="todo"] .card', { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  const patches = count(page, PATCH_STORY);

  const field = '#drawer [data-field="title"]';
  await page.locator(field).fill(`${title} — half a word`);
  await dispatch(page, field, "Enter", true);

  expect(patches(), "a composing Enter must not PATCH the title").toBe(0);
  await expect(page.locator(field)).toBeFocused();

  await dispatch(page, field, "Enter", false);
  await expect(page.locator(field)).not.toBeFocused();
  expect(patches(), "the same Enter without the bit must still commit the title").toBe(1);
});

test("a composing Escape in the description editor keeps the edit", async ({ page }) => {
  // Escape cancels a composition, and this handler read it as "cancel the
  // edit" — restoring the stored description over whatever the user had
  // typed. The most destructive of the ten sites, and the only one whose
  // damage is invisible: no request is made either way, so nothing in the
  // network tab would ever have shown it.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-368 — composing escape ${Date.now()}`;
  await createStory(page, title);
  await page.locator('.column[data-state="todo"] .card', { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const editor = '#drawer [data-field="description"]';
  await page.locator("#drawer .description-view").click();
  await expect(page.locator(editor)).toBeVisible();
  await page.locator(editor).fill("half a composed sentence");
  await dispatch(page, editor, "Escape", true);

  await expect(page.locator(editor)).toBeVisible();
  expect(
    await page.locator(editor).inputValue(),
    "a composing Escape must not revert the textarea to the stored description",
  ).toBe("half a composed sentence");

  await dispatch(page, editor, "Escape", false);
  await expect(page.locator(editor)).toBeHidden();
});

test("a composing Enter in the create modal's label combobox adds no chip", async ({
  page,
}) => {
  // Client-side only — no request at any point — so the damage is the chip
  // itself plus the field being emptied under the user mid-word. Asserted on
  // the DOM for that reason, not on the network.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  const combobox = "#create-labels-field .label-combobox input";
  await page.locator(combobox).fill("sh368-chip");
  await dispatch(page, combobox, "Enter", true);

  await expect(page.locator("#create-labels-field .label-chip")).toHaveCount(0);
  expect(
    await page.locator(combobox).inputValue(),
    "a composing Enter must leave the field alone — emptying it discards the composition",
  ).toBe("sh368-chip");

  await dispatch(page, combobox, "Enter", false);
  await expect(
    page.locator("#create-labels-field .label-chip", { hasText: "sh368-chip" }),
  ).toBeVisible();
});

test("a composing Escape closes no modal from the page level", async ({ page }) => {
  // The page-level handler, which no per-field guard can reach: it is bound on
  // `document` and sees the focused field's keys on the way up. Guarding the
  // fields alone would have left a composing Escape closing the very modal
  // whose field was being typed into — the SH-362 council's "the other door"
  // shape, one story later.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-368 — composing page escape ${Date.now()}`;
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });

  await openDeleteModal(page, card, "a reason worth not losing");
  await page.locator("#delete-reason").focus();
  await dispatch(page, "#delete-reason", "Escape", true);

  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  expect(
    await page.locator("#delete-reason").inputValue(),
    "and the reason typed into it survives",
  ).toBe("a reason worth not losing");

  await dispatch(page, "#delete-reason", "Escape", false);
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
});

// ============================================================
// Fixtures
// ============================================================

/** Dispatches one synthetic `keydown` at `selector` with the composing bit set
 * as asked, and lets the page finish reacting to it.
 *
 * A constructed `KeyboardEvent` rather than `page.keyboard`, because there is
 * no other way: `keyboard.press` has no composing bit to set, and no browser
 * automation protocol can start a real IME composition. It models no user,
 * deliberately — `support.ts`'s `activateBehindOverlay` says the same of
 * itself — and it reaches the real listener, the real guard and the real
 * handler behind them.
 *
 * `bubbles` is not tidiness: the page-level Escape handler is bound on
 * `document`, so an event that does not bubble would never reach the surface
 * `a composing Escape closes no modal from the page level` is about, and that
 * test would pass against a build with no guard at all. */
async function dispatch(
  page: Page,
  selector: string,
  key: string,
  isComposing: boolean,
): Promise<void> {
  await page.locator(selector).evaluate((node, init) => {
    node.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: init.key,
        isComposing: init.isComposing,
        bubbles: true,
        cancelable: true,
      }),
    );
  }, { key, isComposing });
  // One macrotask, so the handler's synchronous work — and any request it
  // started — has certainly happened before the caller reads a count.
  await page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 0)));
}

/** Counts matching requests from now on.
 *
 * Deliberately not `modal-enter-autorepeat.spec.ts`'s `countAndHold`: that file
 * counts a *burst*, where the first reply landing early changes the number, so
 * it has to hold every request open. Here the expected counts are 0 and 1, and
 * every count is read after an `await expect(...)` on the outcome the single
 * permitted request produces — so anything the composing dispatch had started
 * has certainly been seen by the time the number is compared. Nothing to hold,
 * and nothing to race. */
function count(page: Page, predicate: (request: Request) => boolean): () => number {
  let seen = 0;
  page.on("request", (request) => {
    if (predicate(request)) seen += 1;
  });
  return () => seen;
}

/** A request predicate over method and URL. */
function matches(method: string, url: RegExp): (request: Request) => boolean {
  return (request) => request.method() === method && url.test(request.url());
}

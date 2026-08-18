import { test, expect } from "@playwright/test";
import type { Page, Route } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  deleteStatus,
  holdKey,
  openDeleteModal,
  openProject,
  openStatuses,
  refuseTheFirstReposRead,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * SH-362 — a held Enter in a text input submits exactly once.
 *
 * The sibling of `notice-autorepeat.spec.ts` (SH-339) and, deliberately, a
 * separate file: that one's subject is **activation** — whether a key event
 * reaches a `<button>`'s default action — and its fix is one delegated listener
 * calling `preventDefault()`. Nothing here is a button, and `preventDefault()`
 * reaches none of it: these are the page's own `keydown` listeners on `<input>`
 * elements, which call a submit function directly, so a cancelled default
 * changes nothing. Same key, same OS behaviour, different mechanism and a
 * different repair — which is why SH-339's council routed this to a filing
 * rather than into that story's diff.
 *
 * ## The sites, and how the list was arrived at
 *
 * Three were filed. A sibling sweep found a fourth. The sweep itself then missed
 * a fifth, and that miss is the most useful thing this story produced:
 * `buildLabelsSection`'s label-add input is spelled `if (e.key !== "Enter" || …)
 * return;`, and every grep in the investigation had been written `e.key ===
 * "Enter"`. It was found by a council seat reading the file rather than
 * grepping it. `tests/dashboard_enter_submit_guard.rs` is the durable answer to
 * that class; this file is the behavioural half.
 *
 * | Input | Submits | One held Enter, before the fix |
 * |---|---|---|
 * | `#delete-reason` | `submitDeleteStory` | **9** DELETEs |
 * | `#token-input` | `submitTokenModal` | **9** `POST /token` |
 * | `.status-add-slug`, `.status-add-desc` | `statusMutation` | one POST per repeat |
 * | label-add (`[data-field="label-add"]`) | `POST …/labels` | one POST per repeat |
 * | `#drop-blocked-reason-input` | `finishBlockedDrop` | **1** — already correct |
 * | `#create-title` | `submitCreate` | **1** — already guarded (SH-312) |
 *
 * The last two rows are pinned rather than skipped. `finishBlockedDrop` reads
 * `dropBlockedReasonMoveId` and nulls it **synchronously**, before anything
 * awaits, so every repeat after the first finds `null` and returns — correct by
 * construction rather than by a guard anyone wrote for it. A future edit moving
 * that clear into the request's `.then`, the way `submitDeleteStory` once
 * cleared its own subject, would reintroduce the defect on a surface nobody was
 * watching. The test is the difference between "we checked once" and "it stays
 * true".
 *
 * ## Why both mechanisms, and the test that is the only reason to believe it
 *
 * The fix is two guards: `bindEnterSubmit` ignores auto-repeats, and each
 * mutating submit takes an in-flight claim synchronously at entry, released on
 * settle. Neither subsumes the other — a claim alone loses to a fast server (a
 * repeat arriving after a sub-repeat-interval reply finds it cleared, and the
 * token modal deliberately stays open on failure), and a repeat check alone
 * loses to a fast double-press and is inert wherever `KeyboardEvent.repeat` is
 * never set.
 *
 * That argument was nearly unfalsifiable here, and the flaw was found by this
 * story's council rather than by its author. **Every held-key test below holds
 * the first request open**, which pins the in-flight claim `true` for the whole
 * burst — so those tests never exercise the repeat check at all, and an
 * implementation with the claim but no repeat check is an *equivalent mutant*
 * under all of them. This is SH-339's own finding one file over, where dropping
 * the Enter check survived all six of its tests.
 *
 * `a synthetic repeat-flagged Enter is ignored at a freshly opened surface` is
 * the kill. It dispatches one `KeyboardEvent` with `repeat: true` at a surface
 * whose claim is known `false` and which has never submitted, so the claim
 * cannot be what refuses it. Its `repeat: false` twin asserts the same surface
 * still submits, without which "dispatched events never work" would pass both.
 *
 * ## Both directions, everywhere else too
 *
 * Discrete presses must still submit at every site; the token modal's discrete
 * retry after a *refused* token must still work, that being the one surface
 * whose failure path deliberately stays open and primed; and a held **Backspace**
 * inside these same fields must still delete more than one character — SH-339's
 * ArrowDown lesson at a caret rather than a scroller. That last one is why
 * `bindEnterSubmit` tests `e.key` before `e.repeat`, and it is asserted here
 * rather than inherited from SH-339's reasoning.
 *
 * ## What is NOT claimed
 *
 * **Blink only** (SH-335), and **Playwright sets the `repeat` bit itself** on a
 * second `keyboard.down` of a key already held. So these tests speak to what the
 * page does with a repeat-flagged event, not to what a physically held key
 * produces — the same disclaimer `notice-autorepeat.spec.ts` carries, for the
 * same reason.
 */

const AUTH_HEADERS = {
  "X-Storyhook": "1",
  "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
};

/** How many auto-repeats each held key delivers after its deliberate press.
 *
 * Eight, matching `notice-autorepeat.spec.ts`: comfortably more than one and far
 * more than any plausible off-by-one, so a failure reports a number that names
 * the defect rather than one that could be a rounding argument. */
const REPEATS = 8;

/** The project the add-status tests mint scratch statuses in — Delta, for the
 * reason `status-destination-prompt.spec.ts` gives: it has a checkout (writes to
 * a checkout-less project answer 422), and nothing asserts a column shape for it
 * byte-for-byte the way Alpha's fixture is asserted on. */
const SCRATCH_PROJECT = "Delta Project";

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

/** Counts matching requests and holds **every one** of them open until
 * `release()`.
 *
 * Holding is what makes the count deterministic. Without it the first reply can
 * land between two repeats on a fast local daemon, so the number a buggy build
 * produces depends on the machine, and a fix that merely narrowed the window
 * would pass on a slow one.
 *
 * Holding *every* request rather than only the first is a correction this story's
 * council required, and the site it was required for is the add-status form. Its
 * repeats 409 against the slug the first POST just created; that `.catch` calls
 * `renderStatuses()`, which rebuilds the form and **detaches the very input the
 * key is still being typed into**. So with only the first request held, the
 * measured count was a race between CDP round trips and a microtask — it read 3
 * where the other sites read 9, and on a faster machine it could read 1 **against
 * the unfixed build**, which is a regression test that passes while the defect is
 * present. The SH-263 shape exactly: the gate asked the right question and its own
 * fixture lied to it.
 *
 * `route.fetch()` sends the real request to the real daemon, so effects genuinely
 * happen and the first request really does delete the story or create the status;
 * only the *replies* wait for the test. Same technique as
 * `duplicate-create.spec.ts` and `support.ts`'s `holdFetch`. */
async function countAndHold(
  page: Page,
  matches: (route: Route) => boolean,
): Promise<{ seen: () => number; release: () => Promise<void> }> {
  let seen = 0;
  let open!: () => void;
  const held = new Promise<void>((resolve) => {
    open = resolve;
  });
  // Every handler's own completion, awaited by `release()` so a test cannot
  // return while a `route.fulfill()` is still pending — the "Fetch response has
  // been disposed" failure `duplicate-create.spec.ts` records.
  const settled: Promise<unknown>[] = [];

  await page.route("**/*", async (route) => {
    if (!matches(route)) {
      await route.continue();
      return;
    }
    seen += 1;
    const done = (async () => {
      const response = await route.fetch({ headers: credentialsFor(route) });
      await held;
      await route.fulfill({ response });
    })();
    settled.push(done.catch(() => undefined));
    await done;
  });

  return {
    seen: () => seen,
    release: async () => {
      open();
      await Promise.all(settled);
    },
  };
}

/** The headers to replay a held request with: its own, plus the suite's bearer
 * token *only* where the request does not already carry one.
 *
 * `route.fetch()` does not send the browser context's cookie jar, and since
 * SH-255 the page's credential is an `HttpOnly` cookie — so an `/api/**` request
 * replayed verbatim answers 401, whose body the page then fails to parse, and
 * the reply is never applied. `support.ts`'s `holdFetch` injects the token for
 * exactly that reason.
 *
 * The condition is not defensive tidiness. `POST /token` carries its own
 * `X-Storyhook-Token` — the value the user pasted, which is the entire subject
 * of that exchange — and overwriting it with the suite's master token makes the
 * daemon answer SH-319's 422 ("that's the daemon's own bearer token, not a
 * named one") instead of 204. The count assertion still passed; the modal simply
 * never closed, which is a harness bug wearing the costume of a product bug. */
function credentialsFor(route: Route): Record<string, string> {
  const own = route.request().headers();
  if (own["x-storyhook-token"]) return own;
  return { ...own, ...AUTH_HEADERS };
}

const isMethodTo = (method: string, pattern: RegExp) => (route: Route) =>
  route.request().method() === method && pattern.test(route.request().url());

const DELETE_STORY = isMethodTo("DELETE", /\/api\/repos\/[^/]+\/story\/[^/]+$/);
const EXCHANGE_TOKEN = isMethodTo("POST", /\/token$/);
const CREATE_STATUS = isMethodTo("POST", /\/api\/repos\/[^/]+\/states$/);
const MOVE_STORY = isMethodTo("POST", /\/api\/repos\/[^/]+\/story\/[^/]+\/move$/);
const ADD_LABEL = isMethodTo("POST", /\/api\/repos\/[^/]+\/story\/[^/]+\/labels$/);

// ============================================================
// The defect: one held Enter, one request
// ============================================================

test("a held Enter in the delete modal issues exactly one DELETE", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-362 — held Enter deletes once ${Date.now()}`;
  await createStory(page, title);

  const deletes = await countAndHold(page, DELETE_STORY);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await openDeleteModal(page, card, "SH-362 repro");
  await page.locator("#delete-reason").focus();
  await holdKey(page, "Enter", REPEATS);

  // Read before releasing: every repeat has already run its handler by the time
  // `holdKey` resolves, so this is the complete count, not a snapshot of one
  // still in progress. Nine, before the fix.
  expect(deletes.seen(), "a held Enter must not issue a DELETE per auto-repeat").toBe(1);

  await deletes.release();
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(card).not.toBeVisible();
  // The one DELETE that ran was the real one. Nothing failed into the closed
  // modal behind it — the invisible-failure half of this defect.
  await expect(page.locator("#toast-stack .toast.success")).toContainText("deleted");
});

test("a held Enter in the token modal issues exactly one token exchange", async ({ page }) => {
  await refuseTheFirstReposRead(page);
  await page.goto("/");
  // `api()`'s own 401 handler opens this, which is the only way a user meets it:
  // a credential that went stale under a tab that was working.
  await expect(page.locator("#token-modal")).toHaveClass(/open/);

  const exchanges = await countAndHold(page, EXCHANGE_TOKEN);
  await page.locator("#token-input").fill(requiredEnv("DASHBOARD_NAMED_TOKEN"));
  await page.locator("#token-input").focus();
  await holdKey(page, "Enter", REPEATS);

  expect(
    exchanges.seen(),
    "a held Enter must not exchange the token once per auto-repeat",
  ).toBe(1);

  await exchanges.release();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
});

test("a held Enter in the add-status form issues exactly one create", async ({
  page,
  request,
}) => {
  const slug = `sh362-${Date.now()}`;
  await page.goto("/");
  await openStatuses(page, SCRATCH_PROJECT);

  const creates = await countAndHold(page, CREATE_STATUS);
  const input = page.locator(".status-add-slug");
  await input.fill(slug);
  await input.focus();
  await holdKey(page, "Enter", REPEATS);

  expect(creates.seen(), "a held Enter must not POST a status per auto-repeat").toBe(1);

  await creates.release();
  await expect(page.locator(".status-row", { hasText: slug })).toBeVisible();
  await deleteStatus(request, SCRATCH_PROJECT, slug);
});

test("a held Enter in the drawer's label field issues exactly one label POST", async ({
  page,
}) => {
  // The site both the filing and the sibling sweep missed, spelled with an
  // inverted key test. It clears the field only in the `.then`, so before the
  // fix every repeat re-read a value the reply had not yet emptied.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-362 — held Enter labels once ${Date.now()}`;
  await createStory(page, title);
  await page.locator('.column[data-state="todo"] .card', { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const labels = await countAndHold(page, ADD_LABEL);
  const input = page.locator('#drawer [data-field="label-add"]');
  await input.fill("sh362-held");
  await input.focus();
  await holdKey(page, "Enter", REPEATS);

  expect(labels.seen(), "a held Enter must not POST a label per auto-repeat").toBe(1);

  await labels.release();
  await expect(page.locator("#drawer .label-chip", { hasText: "sh362-held" })).toBeVisible();
});

test("a held Enter in the blocked-drop reason modal issues exactly one move", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-362 — held Enter moves once ${Date.now()}`;
  await createStory(page, title);

  const moves = await countAndHold(page, MOVE_STORY);
  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .dragTo(page.locator('.column[data-state="blocked"]'));
  await expect(page.locator("#drop-blocked-reason-modal")).toHaveClass(/open/);
  await page.locator("#drop-blocked-reason-input").fill("SH-362 repro");
  await page.locator("#drop-blocked-reason-input").focus();
  await holdKey(page, "Enter", REPEATS);

  // Already true before this story, and pinned so it stays true — see the header
  // on `finishBlockedDrop`'s synchronous clear.
  expect(moves.seen(), "a held Enter must not move the card once per auto-repeat").toBe(1);

  await moves.release();
  await expect(
    page.locator('.column[data-state="blocked"] .card', { hasText: title }),
  ).toBeVisible();
});

// ============================================================
// The kill: proving the repeat check exists at all
// ============================================================

test("a synthetic repeat-flagged Enter is ignored at a freshly opened surface", async ({
  page,
}) => {
  // This is the only test in the file that can distinguish the two guards, and
  // the reason is worth stating where it will be read. Every held-key test above
  // holds its first request open, which pins the in-flight claim `true` for the
  // whole burst — so an implementation with the claim and NO repeat check passes
  // all of them. The claim is not what refuses the event below: this surface has
  // never submitted, so its claim is `false`, and the only thing that can refuse
  // a `repeat: true` keydown here is `bindEnterSubmit`'s own check.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-362 — synthetic repeat ${Date.now()}`;
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });

  const deletes = await countAndHold(page, DELETE_STORY);
  await openDeleteModal(page, card, "SH-362 synthetic repeat");
  await dispatchEnter(page, "#delete-reason", true);

  expect(
    deletes.seen(),
    "a repeat-flagged Enter must be refused even when nothing is in flight",
  ).toBe(0);
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await expect(card).toBeVisible();

  // The twin, and it is not optional: without it "a dispatched KeyboardEvent
  // never reaches the handler" — a broken harness, or a guard that refused
  // every synthetic event — would satisfy the assertion above perfectly.
  await dispatchEnter(page, "#delete-reason", false);
  expect(
    deletes.seen(),
    "an ordinary Enter must still submit — the same event, the same way, with only the repeat bit changed",
  ).toBe(1);

  await deletes.release();
  await expect(card).not.toBeVisible();
});

// ============================================================
// The other kill: proving the in-flight claim carries its own weight
// ============================================================

test("a second discrete Enter while the first request is in flight issues one request", async ({
  page,
}) => {
  // The mirror of the synthetic-repeat test, and it exists because a mutation
  // survived without it. Deleting `if (deleteModalInFlight) return;` left all
  // ten other tests green: the claim's setter also disables
  // `#delete-modal-submit`, and a disabled button cannot be activated, so even
  // the button-door test was passing on the disabled attribute rather than on
  // the check it was written for.
  //
  // Two *discrete* presses are what isolate the claim. Both carry
  // `repeat: false`, so `bindEnterSubmit` passes both through by design, and the
  // input — unlike the button — is never disabled. Nothing but the claim can
  // refuse the second. This is also the real SH-312 gesture: a user who presses
  // again because the first press appeared to do nothing.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-362 — double press ${Date.now()}`;
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });

  const deletes = await countAndHold(page, DELETE_STORY);
  await openDeleteModal(page, card, "SH-362 double press");
  await page.locator("#delete-reason").press("Enter");
  await page.locator("#delete-reason").press("Enter");

  expect(
    deletes.seen(),
    "a second press while the first DELETE is outstanding must be refused by the claim",
  ).toBe(1);

  await deletes.release();
  await expect(card).not.toBeVisible();
});

test("a second discrete Enter in the label field while its POST is in flight adds one label", async ({
  page,
}) => {
  // The same property at the other shape of claim. The delete modal's is a
  // module variable with a setter that also disables a button; this one is a
  // closure-local `pending` in `buildLabelsSection` with no button at all, so
  // there is no disabled attribute here to mask a missing check.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-362 — double press labels ${Date.now()}`;
  await createStory(page, title);
  await page.locator('.column[data-state="todo"] .card', { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const labels = await countAndHold(page, ADD_LABEL);
  const input = page.locator('#drawer [data-field="label-add"]');
  await input.fill("sh362-double");
  await input.press("Enter");
  await input.press("Enter");

  expect(
    labels.seen(),
    "a second press while the first label POST is outstanding must be refused by the claim",
  ).toBe(1);

  await labels.release();
  await expect(page.locator("#drawer .label-chip", { hasText: "sh362-double" })).toBeVisible();
});

// ============================================================
// Over-reach: the guard must not suppress anything else
// ============================================================

test("discrete Enter presses still submit at every site", async ({ page, request }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");

  // Blocked-drop: one press, one move.
  const moved = `SH-362 — discrete Enter still moves ${Date.now()}`;
  await createStory(page, moved);
  await page
    .locator('.column[data-state="todo"] .card', { hasText: moved })
    .dragTo(page.locator('.column[data-state="blocked"]'));
  await expect(page.locator("#drop-blocked-reason-modal")).toHaveClass(/open/);
  await page.locator("#drop-blocked-reason-input").fill("SH-362 discrete press");
  await page.locator("#drop-blocked-reason-input").press("Enter");
  await expect(
    page.locator('.column[data-state="blocked"] .card', { hasText: moved }),
  ).toBeVisible();

  // Labels: two discrete presses add two labels. The second is the half that
  // matters — a claim that failed to release would swallow it silently.
  const labelled = `SH-362 — discrete Enter still labels ${Date.now()}`;
  await createStory(page, labelled);
  await page.locator('.column[data-state="todo"] .card', { hasText: labelled }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  const labelInput = page.locator('#drawer [data-field="label-add"]');
  for (const label of ["sh362-first", "sh362-second"]) {
    await labelInput.fill(label);
    await labelInput.press("Enter");
    await expect(page.locator("#drawer .label-chip", { hasText: label })).toBeVisible();
  }
  await page.locator("#drawer-close").click();

  // Delete: one press, one deletion.
  const doomed = `SH-362 — discrete Enter still deletes ${Date.now()}`;
  await createStory(page, doomed);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: doomed });
  await openDeleteModal(page, card, "SH-362 discrete press");
  await page.locator("#delete-reason").press("Enter");
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(card).not.toBeVisible();

  // Add-status: one press, one status.
  const slug = `sh362d-${Date.now()}`;
  await openStatuses(page, SCRATCH_PROJECT);
  await page.locator(".status-add-slug").fill(slug);
  await page.locator(".status-add-slug").press("Enter");
  await expect(page.locator(".status-row", { hasText: slug })).toBeVisible();
  await deleteStatus(request, SCRATCH_PROJECT, slug);
});

test("a refused token can be retried with a second discrete Enter", async ({ page }) => {
  await refuseTheFirstReposRead(page);
  await page.goto("/");
  await expect(page.locator("#token-modal")).toHaveClass(/open/);

  // The token modal is the one site whose failure path deliberately leaves the
  // surface open and primed, so a claim that latched on the first submission
  // would strand the user in a modal that had silently stopped answering — a
  // worse defect than the nine exchanges the claim was added to prevent. A wrong
  // token first, then the right one, both by discrete press.
  await page.locator("#token-input").fill("not-a-real-token");
  await page.locator("#token-input").press("Enter");
  await expect(page.locator("#token-error")).toContainText("not accepted");
  await expect(page.locator("#token-modal")).toHaveClass(/open/);
  await expect(page.locator("#token-submit")).toBeEnabled();

  await page.locator("#token-input").fill(requiredEnv("DASHBOARD_NAMED_TOKEN"));
  await page.locator("#token-input").press("Enter");
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
});

test("a held Backspace still deletes more than one character", async ({ page }) => {
  // SH-339's ArrowDown finding at a caret rather than a scroller. A guard
  // written `if (e.repeat) return;` — testing the repeat bit before the key —
  // would satisfy every count assertion in this file and quietly take held
  // Backspace, held Arrow and held Delete away from anyone who edits that way.
  // Measuring against one discrete press is what makes this non-vacuous: the
  // deliberate first keydown is never a repeat, so a bare "the field changed"
  // assertion would hold against the very mutant it exists to kill.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-362 — held Backspace ${Date.now()}`;
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await openDeleteModal(page, card, "");

  const field = page.locator("#delete-reason");
  await field.fill("abcdefghij");
  await field.focus();
  await page.keyboard.press("Backspace");
  expect(await field.inputValue(), "one discrete Backspace deletes one character").toBe(
    "abcdefghi",
  );

  await field.fill("abcdefghij");
  await holdKey(page, "Backspace", REPEATS);
  // Nine characters: one deliberate press plus eight repeats, all of which must
  // reach the field.
  expect(
    await field.inputValue(),
    "a held Backspace must keep repeating — the guard names Enter, not every repeated key",
  ).toBe("a");

  await page.locator("#delete-modal-cancel").click();
});

test("a held Enter on a focused submit button issues exactly one request", async ({
  page,
}) => {
  // The other door, and no guard on an input can see it: a `<button>` runs its
  // activation behaviour on keydown for Enter, so a held key on a focused
  // `#delete-modal-submit` fires one click per repeat straight into
  // `submitDeleteStory`. `refuseAutoRepeatActivation` does not reach here — it
  // is bound to `#notice-dock` alone — so what refuses these is the in-flight
  // claim, and this is the only test that proves the claim carries its own
  // weight rather than being shadowed by the repeat check.
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-362 — held Enter on the button ${Date.now()}`;
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });

  const deletes = await countAndHold(page, DELETE_STORY);
  await openDeleteModal(page, card, "SH-362 button door");
  await page.locator("#delete-modal-submit").focus();
  await holdKey(page, "Enter", REPEATS);

  expect(
    deletes.seen(),
    "a held Enter on the submit button must not issue a DELETE per auto-repeat",
  ).toBe(1);

  await deletes.release();
  await expect(card).not.toBeVisible();
});

// ============================================================
// Fixtures
// ============================================================

/** Dispatches one synthetic Enter keydown at `selector`, with the `repeat` bit
 * set as asked.
 *
 * A constructed `KeyboardEvent` rather than `page.keyboard`, and the difference
 * is the entire point: `keyboard.down` sets the repeat bit only on a *second*
 * press of a key already held, which necessarily means a submission has already
 * been attempted and a claim may already be standing. This puts a repeat-flagged
 * event in front of a surface that has done nothing at all, which no sequence of
 * real key presses can arrange.
 *
 * It models no user, deliberately — `support.ts`'s `activateBehindOverlay` says
 * the same of itself — and it reaches the real listener, the real guard and the
 * real submit function. */
async function dispatchEnter(page: Page, selector: string, repeat: boolean): Promise<void> {
  await page.locator(selector).evaluate((node, isRepeat) => {
    node.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", repeat: isRepeat, bubbles: true }),
    );
  }, repeat);
  // One macrotask, so the handler's synchronous work — and any request it
  // started — has certainly happened before the caller reads a count.
  await page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 0)));
}

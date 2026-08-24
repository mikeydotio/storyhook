import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  heldReadDeadlineMs,
  holdUntilRefused,
  openProject,
  projectSlug,
  seedToken,
  storiesInProject,
} from "./support";

/**
 * Exercises SH-439: the create modal's `#create-project` dropdown -- the
 * modal's first field, preselected to the open project, letting a story be
 * filed into a project other than the one on screen.
 *
 * Fixtures, from `scripts/run-e2e.sh`:
 *
 *   - "Alpha Project" (prefix AA) -- the stock state catalog (todo,
 *     in-progress, blocked, done) plus an appended `review` OPEN state.
 *     Used the same way `create-story-defaults.spec.ts` does: a state no
 *     other project has, to prove a rebuilt option list actually dropped
 *     it rather than merely never having asserted on it.
 *   - "Beta Project" (prefix BB) -- the stock catalog, untouched. Chosen as
 *     the cross-project target deliberately over "Delta Project": Delta is
 *     reserved as Dispatch Auto's own claim target
 *     (`scripts/run-e2e.sh`'s own comment), and a stray story there could
 *     change what `story next --claim` hands out.
 *   - "Gamma Archive" (prefix GA) -- no checkout (`--no-attach`), so its
 *     `#create-project` option must be disabled: writing to it would hit
 *     the server's own `pathless_refusal`.
 */

cleanUpCreatedStories("Alpha Project");
cleanUpCreatedStories("Beta Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("the project dropdown is the modal's first field and preselects the open project", async ({
  page,
  request,
}) => {
  const alphaSlug = await projectSlug(request, "Alpha Project");

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  await expect(
    page.locator("#create-modal .modal-body > *").first(),
  ).toHaveAttribute("id", "create-project-field");
  await expect(page.locator("#create-project")).toHaveValue(alphaSlug);

  await page.locator("#create-discard").click();
});

test("a project with no checkout is listed but not selectable", async ({
  page,
}) => {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  const alpha = page.locator("#create-project option", { hasText: "Alpha Project" });
  const beta = page.locator("#create-project option", { hasText: "Beta Project" });
  const gamma = page.locator("#create-project option", { hasText: "Gamma Archive" });
  await expect(alpha).toBeEnabled();
  await expect(beta).toBeEnabled();
  await expect(gamma).toBeDisabled();

  await page.locator("#create-discard").click();
});

test("switching projects resets state and type to the new project's own defaults, and drops the old project's own state", async ({
  page,
  request,
}) => {
  const betaSlug = await projectSlug(request, "Beta Project");

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  // Alpha-only, before the switch: proves the option list this assertion
  // relies on losing "review" actually came FROM Alpha, not merely that
  // Beta never had it.
  await expect(
    page.locator("#create-state option", { hasText: "review" }),
  ).toHaveCount(1);

  const title = "Carried across a project switch";
  await page.locator("#create-title").fill(title);
  await page.locator("#create-description").fill("carried too");

  await page.locator("#create-project").selectOption(betaSlug);
  // The vocabulary GET is debounced and asynchronous; wait for it to land
  // rather than asserting immediately against stale (Alpha's own) options.
  await expect(
    page.locator("#create-state option", { hasText: "review" }),
  ).toHaveCount(0);

  // Beta's own stock catalog defaults to the same values Alpha's untouched
  // catalog does (todo/normal) -- the meaningful proof here is the option
  // LIST changed, asserted above; this confirms the selection itself is
  // Beta's own default rather than a carried-over Alpha slug (which would
  // read identically for state/type here, but see SH-439's plan for why
  // preserving the slug -- as opposed to landing on the same value by
  // coincidence -- would have been the wrong rule regardless).
  await expect(page.locator("#create-state")).toHaveValue("todo");
  await expect(page.locator("#create-type")).toHaveValue("normal");

  // Project-independent fields survive the switch untouched.
  await expect(page.locator("#create-title")).toHaveValue(title);
  await expect(page.locator("#create-description")).toHaveValue("carried too");

  await page.locator("#create-discard").click();
});

test("submitting files the story in the selected project, not the one on screen, and names both in a toast", async ({
  page,
  request,
}) => {
  const betaSlug = await projectSlug(request, "Beta Project");
  const title = "Filed into a different project entirely";

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-project").selectOption(betaSlug);
  await expect(page.locator("#create-state")).toBeEnabled();

  await page.locator("#create-title").fill(title);
  // Set explicitly to dodge SH-358's unassessed-priority warn toast, which
  // would otherwise sit in #toast-stack beside the assertion below.
  await page.locator("#create-priority").selectOption("medium");

  const created = page.waitForResponse(
    (resp) =>
      resp.request().method() === "POST" &&
      new URL(resp.url()).pathname === `/api/repos/${betaSlug}/story`,
  );
  await page.locator("#create-submit").click();
  const payload = await (await created).json();
  const createdId: string = payload.story.story.id;

  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  // The board on screen is still Alpha's -- SH-439's own decision, not a
  // side effect -- so nothing here is the confirmation SH-127 relies on
  // for a same-project create. The toast is.
  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toContainText(createdId);
  await expect(toast).toContainText("Beta Project");

  const beta = await storiesInProject(request, "Beta Project");
  const alpha = await storiesInProject(request, "Alpha Project");
  expect(beta.some((s) => s.id === createdId)).toBe(true);
  expect(alpha.some((s) => s.id === createdId)).toBe(false);
});

test("a same-project create raises no toast (SH-127 unchanged)", async ({
  page,
}) => {
  const title = "Stays quiet like it always has";
  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(title);
  await page.locator("#create-priority").selectOption("medium");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#toast-stack .toast.success")).toHaveCount(0);
});

test("editing a draft pins the project dropdown, disabled", async ({
  page,
  request,
}) => {
  const alphaSlug = await projectSlug(request, "Alpha Project");
  const title = "A draft that stays put";

  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(title);
  await page.locator("#create-save-draft").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  await page.locator("#drafts-btn").click();
  await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
  await page.locator("#drafts-list .drafts-row", { hasText: title }).click();
  await expect(page.locator("#create-modal-header")).toHaveText("Edit draft");

  await expect(page.locator("#create-project")).toBeDisabled();
  await expect(page.locator("#create-project")).toHaveValue(alphaSlug);

  await page.locator("#create-discard").click();
});

/**
 * The regression test for SH-439's own sharpest hazard, found reviewing the
 * design before implementation: `bindEnterSubmit($("create-title"),
 * submitCreate)` calls `submitCreate()` directly and never consults a
 * button's own `disabled` attribute, so guarding only the three footer
 * buttons while a vocabulary fetch is in flight would leave Enter in
 * `#create-title` free to submit while `#create-state`/`#create-type`
 * still showed the PREVIOUS project's slugs -- exactly the ambiguity
 * `createModalBusy()` exists to close.
 *
 * `holdUntilRefused` never delivers the vocabulary GET at all for the
 * duration of the assertion, so `createVocabPending` is provably still
 * true (observed here via `#create-state` staying disabled) when Enter is
 * pressed -- not merely likely to be, on a fast connection.
 */
test("Enter in the title cannot submit while a project's vocabulary fetch is in flight", async ({
  page,
  request,
}) => {
  const alphaSlug = await projectSlug(request, "Alpha Project");
  const betaSlug = await projectSlug(request, "Beta Project");
  // Deep-linked rather than routed through openProject()/the Home screen
  // (board-readiness.spec.ts's own idiom for the same reason): the
  // beforeEach above already opened Alpha once, which `bootstrap()`
  // remembers in localStorage, so a second bare `page.goto("/")` here would
  // restore straight onto Alpha's board rather than landing on Home, and
  // openProject()'s click on a Home-screen repo card would find nothing
  // visible to click. The vocabulary GET this test holds is an ordinary
  // api()-issued GET, governed by apiGetTimeoutMs (API_GET_TIMEOUT_MS) --
  // not the board's own boardFetchTimeoutMs, which this test never holds.
  // Set past the test's own budget so the page's client-side clock can't
  // settle it out from under the held route before the assertions below
  // run (SH-347).
  await page.goto(
    `/?project=${encodeURIComponent(alphaSlug)}&apiGetTimeoutMs=${heldReadDeadlineMs()}`,
  );
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#filter-count")).not.toHaveText("");

  const heldVocab = await holdUntilRefused(
    page,
    (url) => url.pathname === `/api/repos/${betaSlug}/data`,
  );

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-project").selectOption(betaSlug);

  // Proof the fetch is genuinely stuck, not merely "probably still running":
  // #create-state is disabled only while createVocabPending is true.
  await expect(page.locator("#create-state")).toBeDisabled();

  let posted = 0;
  await page.route(
    (url) => /\/story$/.test(url.pathname) && url.pathname.startsWith("/api/repos/"),
    async (route) => {
      if (route.request().method() === "POST") posted++;
      await route.continue();
    },
  );

  await page.locator("#create-title").fill("Should never be created");
  // No wall-clock wait needed to prove the absence: bindEnterSubmit's
  // handler calls submitCreate() synchronously in the same task, and
  // createModalBusy()'s check-and-return happens before any `api()` call
  // is ever reached -- so by the time `press("Enter")` resolves, either the
  // POST has already been dispatched or it never will be for this press.
  await page.locator("#create-title").press("Enter");
  expect(posted).toBe(0);
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await expect(page.locator("#create-submit")).toBeDisabled();

  await heldVocab.refuse();
  await page.locator("#create-discard").click();
});

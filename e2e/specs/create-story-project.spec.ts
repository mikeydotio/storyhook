import { test, expect } from "./support";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  heldReadDeadlineMs,
  holdUntilRefused,
  latch,
  onAFrozenClock,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
  storiesInProject,
} from "./support";

/**
 * Exercises SH-439's create-modal project dropdown and SH-485's selection-
 * time safety boundary: the modal may visibly select another project at once,
 * but no mutation is eligible until that exact project's vocabulary settles.
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
 *     change what `story claim --next` hands out. SH-485 reads Delta only as
 *     the third writable project's vocabulary and suppresses its target-proof
 *     POST before bytes leave the renderer, so that reservation stays intact.
 *   - "Gamma Archive" (prefix GA) -- no checkout (`--no-attach`), so its
 *     `#create-project` option must be disabled: writing to it would hit
 *     the server's own `pathless_refusal`.
 */

cleanUpCreatedStories("Alpha Project");
cleanUpCreatedStories("Beta Project");

/** The production constant whose timer boundary these tests drive with
 * Playwright's fake clock. Kept named, not used as a wall-clock sleep: every
 * assertion that cares about the pre-debounce window runs in one renderer
 * task, and `page.clock.runFor()` is what proves the timer did or did not fire. */
const CREATE_VOCAB_DEBOUNCE_MS = 150;

interface CreateGuardSnapshot {
  selected: string;
  activeId: string;
  activeInsideModal: boolean;
  submitDisabled: boolean;
  saveDisabled: boolean;
  discardDisabled: boolean;
  stateDisabled: boolean;
  typeDisabled: boolean;
}

/** Changes the project and snapshots every safety-relevant fact before the
 * renderer can leave the same JavaScript task. `selectOption()` is right for
 * ordinary interaction tests; this helper is intentionally lower-level
 * because SH-485's regression window exists between `change` and its timer. */
async function dispatchProjectChangeNow(
  page: Page,
  target: string,
  focusId?: string,
): Promise<CreateGuardSnapshot> {
  return page.evaluate(
    ({ nextProject, focused }) => {
      const byId = (id: string) => {
        const node = document.getElementById(id);
        if (!node) throw new Error(`missing #${id}`);
        return node as HTMLElement;
      };
      if (focused) byId(focused).focus();
      const project = byId("create-project") as HTMLSelectElement;
      project.value = nextProject;
      project.dispatchEvent(new Event("change", { bubbles: true }));
      const modal = byId("create-modal");
      return {
        selected: project.value,
        activeId: (document.activeElement as HTMLElement | null)?.id || "",
        activeInsideModal: Boolean(document.activeElement && modal.contains(document.activeElement)),
        submitDisabled: (byId("create-submit") as HTMLButtonElement).disabled,
        saveDisabled: (byId("create-save-draft") as HTMLButtonElement).disabled,
        discardDisabled: (byId("create-discard") as HTMLButtonElement).disabled,
        stateDisabled: (byId("create-state") as HTMLSelectElement).disabled,
        typeDisabled: (byId("create-type") as HTMLSelectElement).disabled,
      };
    },
    { nextProject: target, focused: focusId || "" },
  );
}

/** Intercepts create-story POSTs at the page's XHR transport seam before any
 * bytes leave the renderer. That makes an immediate-submit assertion itself
 * synchronous: a delayed Playwright network event cannot turn a false zero
 * into a passing one after the assertion has already read it. */
async function installStoryPostRecorder(page: Page): Promise<void> {
  await page.evaluate(() => {
    type RecordedXhr = XMLHttpRequest & {
      __sh485Request?: { method: string; url: string };
    };
    const browserWindow = window as Window & { __sh485StoryPosts?: string[] };
    browserWindow.__sh485StoryPosts = [];
    const proto = XMLHttpRequest.prototype as unknown as {
      open: (...args: unknown[]) => unknown;
      send: (...args: unknown[]) => unknown;
    };
    const originalOpen = proto.open;
    const originalSend = proto.send;
    proto.open = function (this: RecordedXhr, ...args: unknown[]) {
      this.__sh485Request = { method: String(args[0]), url: String(args[1]) };
      return originalOpen.apply(this, args);
    };
    proto.send = function (this: RecordedXhr, ...args: unknown[]) {
      const request = this.__sh485Request;
      const path = request ? new URL(request.url, window.location.href).pathname : "";
      if (request?.method === "POST" && /\/story$/.test(path)) {
        browserWindow.__sh485StoryPosts?.push(path);
        return undefined;
      }
      return originalSend.apply(this, args);
    };
  });
}

async function recordedStoryPosts(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const browserWindow = window as Window & { __sh485StoryPosts?: string[] };
    return [...(browserWindow.__sh485StoryPosts || [])];
  });
}

async function dispatchProjectChangeAndAttemptNow(
  page: Page,
  target: string,
  action: "save" | "enter",
): Promise<string[]> {
  return page.evaluate(
    ({ nextProject, attemptedAction }) => {
      const project = document.getElementById("create-project") as HTMLSelectElement;
      const title = document.getElementById("create-title") as HTMLInputElement;
      const save = document.getElementById("create-save-draft") as HTMLButtonElement;
      project.value = nextProject;
      project.dispatchEvent(new Event("change", { bubbles: true }));
      if (attemptedAction === "save") {
        save.click();
      } else {
        title.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
      }
      const browserWindow = window as Window & { __sh485StoryPosts?: string[] };
      return [...(browserWindow.__sh485StoryPosts || [])];
    },
    { nextProject: target, attemptedAction: action },
  );
}

async function attemptCreateEnterNow(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const title = document.getElementById("create-title") as HTMLInputElement;
    title.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    const browserWindow = window as Window & { __sh485StoryPosts?: string[] };
    return [...(browserWindow.__sh485StoryPosts || [])];
  });
}

/** Reopens Alpha after installing the fake clock. `install()` freezes
 * nothing; `onAFrozenClock()` scopes the later deterministic timer window. */
async function openClockedAlpha(page: Page, alphaSlug: string): Promise<void> {
  await page.clock.install();
  await page.goto(`/?project=${encodeURIComponent(alphaSlug)}`);
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#filter-count")).not.toHaveText("");
}

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
  const alphaSlug = await projectSlug(request, "Alpha Project");
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

test("a selection synchronously guards every mutation and field, then one settled submit targets Beta", async ({
  page,
  request,
}) => {
  const betaSlug = await projectSlug(request, "Beta Project");
  const requested = latch();
  const release = latch();
  await page.route(
    (url) => url.pathname === `/api/repos/${betaSlug}/data`,
    async (route) => {
      requested.release();
      await release.held;
      await route.continue();
    },
  );

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  // Focus a control that is about to become disabled. The handler must rescue
  // it into the modal before toggling attributes, in the same task that raises
  // the guard -- no timer or request has had an opportunity to run yet.
  const immediate = await dispatchProjectChangeNow(page, betaSlug, "create-submit");
  expect(immediate).toEqual({
    selected: betaSlug,
    activeId: "create-modal",
    activeInsideModal: true,
    submitDisabled: true,
    saveDisabled: true,
    discardDisabled: true,
    stateDisabled: true,
    typeDisabled: true,
  });
  await expect(page.locator("#create-project")).toBeEnabled();

  await requested.held;
  release.release();
  await expect(
    page.locator("#create-state option", { hasText: "review" }),
  ).toHaveCount(0);
  await expect(page.locator("#create-submit")).toBeEnabled();
  await expect(page.locator("#create-save-draft")).toBeEnabled();
  await expect(page.locator("#create-discard")).toBeEnabled();
  await expect(page.locator("#create-state")).toBeEnabled();
  await expect(page.locator("#create-type")).toBeEnabled();

  const postedPaths: string[] = [];
  await page.route(
    (url) => /\/story$/.test(url.pathname) && url.pathname.startsWith("/api/repos/"),
    async (route) => {
      if (route.request().method() === "POST") {
        postedPaths.push(new URL(route.request().url()).pathname);
      }
      await route.continue();
    },
  );
  await page.locator("#create-title").fill("One safe Beta submission");
  await page.locator("#create-priority").selectOption("medium");
  const created = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === `/api/repos/${betaSlug}/story`,
  );
  await page.locator("#create-submit").click();
  await created;
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  expect(postedPaths).toEqual([`/api/repos/${betaSlug}/story`]);
});

test("an immediate Save Draft after Alpha to Beta cannot POST to Alpha", async ({
  page,
  request,
}) => {
  const alphaSlug = await projectSlug(request, "Alpha Project");
  const betaSlug = await projectSlug(request, "Beta Project");
  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill("Immediate draft must stay local");
  await installStoryPostRecorder(page);

  expect(await dispatchProjectChangeAndAttemptNow(page, betaSlug, "save")).toEqual([]);
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await expect(page.locator("#create-save-draft")).toBeDisabled();

  // Return to the mounted Alpha vocabulary before the debounce can fire; the
  // modal is then dismissible without leaving delayed work behind this test.
  const restored = await dispatchProjectChangeNow(page, alphaSlug);
  expect(restored.selected).toBe(alphaSlug);
  expect(restored.saveDisabled).toBe(false);
  expect(await recordedStoryPosts(page)).toEqual([]);
  await page.locator("#create-discard").click();
});

test("an immediate title Enter after Alpha to Beta cannot POST to Alpha", async ({
  page,
  request,
}) => {
  const alphaSlug = await projectSlug(request, "Alpha Project");
  const betaSlug = await projectSlug(request, "Beta Project");
  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill("Immediate Enter must stay local");
  await installStoryPostRecorder(page);

  expect(await dispatchProjectChangeAndAttemptNow(page, betaSlug, "enter")).toEqual([]);
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await expect(page.locator("#create-submit")).toBeDisabled();

  const restored = await dispatchProjectChangeNow(page, alphaSlug);
  expect(restored.selected).toBe(alphaSlug);
  expect(restored.submitDisabled).toBe(false);
  expect(await recordedStoryPosts(page)).toEqual([]);
  await page.locator("#create-discard").click();
});

test("Alpha to Beta to Alpha before debounce reuses Alpha vocabulary without an Alpha refetch", async ({
  page,
  request,
}) => {
  const alphaSlug = await projectSlug(request, "Alpha Project");
  const betaSlug = await projectSlug(request, "Beta Project");
  await openClockedAlpha(page, alphaSlug);
  await page.locator("#new-story-btn").click();

  let alphaDataGets = 0;
  let betaDataGets = 0;
  page.on("request", (requestEvent) => {
    if (requestEvent.method() !== "GET") return;
    const path = new URL(requestEvent.url()).pathname;
    if (path === `/api/repos/${alphaSlug}/data`) alphaDataGets++;
    if (path === `/api/repos/${betaSlug}/data`) betaDataGets++;
  });

  await onAFrozenClock(page, async () => {
    const snapshots = await page.evaluate(
      ({ alpha, beta }) => {
        const project = document.getElementById("create-project") as HTMLSelectElement;
        const state = document.getElementById("create-state") as HTMLSelectElement;
        const submit = document.getElementById("create-submit") as HTMLButtonElement;
        project.value = beta;
        project.dispatchEvent(new Event("change", { bubbles: true }));
        const afterBeta = { stateDisabled: state.disabled, submitDisabled: submit.disabled };
        project.value = alpha;
        project.dispatchEvent(new Event("change", { bubbles: true }));
        return {
          afterBeta,
          afterAlpha: {
            selected: project.value,
            stateDisabled: state.disabled,
            submitDisabled: submit.disabled,
          },
        };
      },
      { alpha: alphaSlug, beta: betaSlug },
    );
    expect(snapshots.afterBeta).toEqual({ stateDisabled: true, submitDisabled: true });
    expect(snapshots.afterAlpha).toEqual({
      selected: alphaSlug,
      stateDisabled: false,
      submitDisabled: false,
    });
    await expect(
      page.locator("#create-state option", { hasText: "review" }),
    ).toHaveCount(1);

    await page.clock.runFor(CREATE_VOCAB_DEBOUNCE_MS);
    expect(alphaDataGets).toBe(0);
    expect(betaDataGets).toBe(0);
  });

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
  // Enabled is not a settlement signal: Alpha leaves this control enabled
  // too. Its fixture-only review option disappears only when Beta lands.
  await expect(
    page.locator("#create-state option", { hasText: "review" }),
  ).toHaveCount(0);

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

test("Drafts is global on Home and edits a cross-project draft through its owner", async ({
  page,
  request,
}) => {
  const alphaSlug = await projectSlug(request, "Alpha Project");
  const betaSlug = await projectSlug(request, "Beta Project");
  const alphaTitle = "Alpha draft in the global list";
  const betaTitle = "Beta draft opened from Home";
  const editedBetaTitle = "Beta draft edited from Home";

  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(alphaTitle);
  await page.locator("#create-save-draft").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  await page.locator("#new-story-btn").click();
  await page.locator("#create-project").selectOption(betaSlug);
  await expect(
    page.locator("#create-state option", { hasText: "review" }),
  ).toHaveCount(0);
  await page.locator("#create-title").fill(betaTitle);
  await page.locator("#create-save-draft").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  await page.locator("#home-btn").click();
  await expect(page.locator("#home-view")).toBeVisible();
  await expect(page.locator("#drafts-btn")).toBeVisible();
  await expect(page.locator("#drafts-btn-text")).toHaveText("2 Drafts");

  await page.locator("#drafts-btn").click();
  const rows = page.locator("#drafts-list .drafts-row");
  await expect(rows).toHaveCount(2);
  await expect(rows.filter({ hasText: alphaTitle })).toContainText("AA · Alpha Project");
  const betaRow = rows.filter({ hasText: betaTitle });
  await expect(betaRow).toContainText("BB · Beta Project");

  const ownerData = page.waitForResponse(
    (resp) =>
      resp.request().method() === "GET" &&
      new URL(resp.url()).pathname === `/api/repos/${betaSlug}/data`,
  );
  await betaRow.click();
  await ownerData;
  await expect(page.locator("#create-modal-header")).toHaveText("Edit draft");
  await expect(page.locator("#create-project")).toBeDisabled();
  await expect(page.locator("#create-project")).toHaveValue(betaSlug);
  await expect(page.locator("#create-title")).toHaveValue(betaTitle);

  await page.locator("#create-title").fill(editedBetaTitle);
  const patched = page.waitForResponse(
    (resp) =>
      resp.request().method() === "PATCH" &&
      new URL(resp.url()).pathname.startsWith(`/api/repos/${betaSlug}/story/`),
  );
  await page.locator("#create-save-draft").click();
  const patchedPayload = await (await patched).json();
  const editedId: string = patchedPayload.story.story.id;
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  await page.locator("#drafts-btn").click();
  const editedBetaRow = page.locator("#drafts-list .drafts-row", {
    hasText: editedBetaTitle,
  });
  await expect(editedBetaRow).toContainText("BB · Beta Project");
  const refreshedOwnerData = page.waitForResponse(
    (resp) =>
      resp.request().method() === "GET" &&
      new URL(resp.url()).pathname === `/api/repos/${betaSlug}/data`,
  );
  await editedBetaRow.click();
  await refreshedOwnerData;
  const published = page.waitForResponse(
    (resp) =>
      resp.request().method() === "POST" &&
      new URL(resp.url()).pathname === `/api/repos/${betaSlug}/story/${editedId}/publish`,
  );
  await page.locator("#create-submit").click();
  await published;
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");

  await page.locator("#drafts-btn").click();
  const alphaRow = page.locator("#drafts-list .drafts-row", { hasText: alphaTitle });
  const alphaOwnerData = page.waitForResponse(
    (resp) =>
      resp.request().method() === "GET" &&
      new URL(resp.url()).pathname === `/api/repos/${alphaSlug}/data`,
  );
  await alphaRow.click();
  await alphaOwnerData;
  const discarded = page.waitForResponse(
    (resp) =>
      resp.request().method() === "DELETE" &&
      new URL(resp.url()).pathname.startsWith(`/api/repos/${alphaSlug}/story/`),
  );
  await page.locator("#create-discard").click();
  await discarded;
  await expect(page.locator("#drafts-btn-text")).toHaveText("No Drafts");

  const betaStories = await storiesInProject(request, "Beta Project");
  const alphaStories = await storiesInProject(request, "Alpha Project");
  expect(betaStories.some((story) => story.id === editedId)).toBe(true);
  expect(alphaStories.some((story) => story.id === editedId)).toBe(false);
});

test("a stale global draft row cannot open an editor for a draft that is gone", async ({
  page,
  request,
}) => {
  const alphaSlug = await projectSlug(request, "Alpha Project");
  const title = "Stale catalog draft";
  const created = page.waitForResponse(
    (resp) =>
      resp.request().method() === "POST" &&
      new URL(resp.url()).pathname === `/api/repos/${alphaSlug}/story`,
  );
  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(title);
  await page.locator("#create-save-draft").click();
  const createdPayload = await (await created).json();
  const id: string = createdPayload.story.story.id;
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");

  await page.route(
    (url) => url.pathname === `/api/repos/${alphaSlug}/data`,
    async (route) => {
      const response = await route.fetch({
        headers: {
          ...route.request().headers(),
          "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
        },
      });
      const data = await response.json();
      data.drafts = [];
      await route.fulfill({ response, json: data });
    },
  );

  await page.locator("#drafts-btn").click();
  const refreshedCatalog = page.waitForResponse(
    (resp) =>
      resp.request().method() === "GET" &&
      new URL(resp.url()).pathname === "/api/repos",
  );
  await page.locator("#drafts-list .drafts-row", { hasText: title }).click();
  await refreshedCatalog;

  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#toast-stack .toast.error")).toContainText(
    `${id} is no longer an available draft.`,
  );
});

test("a global draft editor closes when its owning project is deleted elsewhere", async ({
  page,
  request,
}) => {
  const betaSlug = await projectSlug(request, "Beta Project");
  const title = "Owner disappears while editing from Home";

  await page.locator("#new-story-btn").click();
  await page.locator("#create-project").selectOption(betaSlug);
  await expect(
    page.locator("#create-state option", { hasText: "review" }),
  ).toHaveCount(0);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-save-draft").click();
  await page.locator("#home-btn").click();
  await page.locator("#drafts-btn").click();
  await page.locator("#drafts-list .drafts-row", { hasText: title }).click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await expect(page.locator("#create-project")).toHaveValue(betaSlug);

  await page.route("**/api/repos", async (route) => {
    const response = await route.fetch({
      headers: {
        ...route.request().headers(),
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
    });
    const repos = (await response.json()) as Array<{ id: string }>;
    await route.fulfill({
      response,
      json: repos.filter((repo) => repo.id !== betaSlug),
    });
  });
  const catalog = page.waitForResponse(
    (resp) =>
      resp.request().method() === "GET" &&
      new URL(resp.url()).pathname === "/api/repos",
  );
  const alphaSlug = await projectSlug(request, "Alpha Project");
  const nudge = await request.post(
    `/api/repos/${encodeURIComponent(alphaSlug)}/story`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
      data: { title: "Trigger the external catalog refresh" },
    },
  );
  if (!nudge.ok()) {
    throw new Error(
      `catalog-refresh nudge answered ${nudge.status()}: ${await nudge.text()}`,
    );
  }
  await catalog;

  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#home-view")).toBeVisible();
  await expect(page.locator("#toast-stack .toast.error")).toContainText(
    "This project was deleted",
  );
  // Cleanup emits another catalog refresh. Drain the response-rewriting
  // handler before fixture teardown can dispose its fetched response.
  await page.unrouteAll({ behavior: "wait" });
});

test("a held Beta reply cannot clear pending, replace Alpha vocabulary, or target Beta after Delta is selected", async ({
  page,
  request,
}) => {
  const alphaSlug = await projectSlug(request, "Alpha Project");
  const betaSlug = await projectSlug(request, "Beta Project");
  const deltaSlug = await projectSlug(request, "Delta Project");
  await openClockedAlpha(page, alphaSlug);

  const betaTaken = latch();
  const releaseBeta = latch();
  await page.route(
    (url) => url.pathname === `/api/repos/${betaSlug}/data`,
    async (route) => {
      const response = await route.fetch({
        headers: {
          ...route.request().headers(),
          "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
        },
      });
      betaTaken.release();
      await releaseBeta.held;
      await route.fulfill({ response });
    },
  );

  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill("Only Delta may become the target");
  await installStoryPostRecorder(page);

  await onAFrozenClock(page, async () => {
    const betaPending = await dispatchProjectChangeNow(page, betaSlug);
    expect(betaPending.stateDisabled).toBe(true);
    await page.clock.runFor(CREATE_VOCAB_DEBOUNCE_MS);
    await betaTaken.held;

    // Alpha's fixture-only state remains mounted while Beta is held. Delta's
    // change synchronously invalidates Beta and schedules its own later read.
    await expect(
      page.locator("#create-state option", { hasText: "review" }),
    ).toHaveCount(1);
    const deltaPending = await dispatchProjectChangeNow(page, deltaSlug);
    expect(deltaPending.selected).toBe(deltaSlug);
    expect(deltaPending.stateDisabled).toBe(true);

    const staleArrived = page.waitForResponse(
      (response) =>
        response.request().method() === "GET" &&
        new URL(response.url()).pathname === `/api/repos/${betaSlug}/data`,
    );
    releaseBeta.release();
    await staleArrived;

    // The clock is still frozen inside Delta's newer debounce window. A stale
    // Beta apply would remove review, enable the controls, and make this Enter
    // POST to Beta; the current ticket must permit none of the three.
    await expect(page.locator("#create-project")).toHaveValue(deltaSlug);
    await expect(page.locator("#create-state")).toBeDisabled();
    await expect(page.locator("#create-submit")).toBeDisabled();
    await expect(
      page.locator("#create-state option", { hasText: "review" }),
    ).toHaveCount(1);
    expect(await attemptCreateEnterNow(page)).toEqual([]);

    // Once Delta's own delayed read succeeds, it alone becomes the mutation
    // target. The recorder suppresses the POST but captures its exact path.
    await page.clock.runFor(CREATE_VOCAB_DEBOUNCE_MS);
    await expect(page.locator("#create-state")).toBeEnabled();
    await expect(
      page.locator("#create-state option", { hasText: "review" }),
    ).toHaveCount(0);
    expect(await attemptCreateEnterNow(page)).toEqual([
      `/api/repos/${deltaSlug}/story`,
    ]);
  });
});

test("the current vocabulary failure reverts to Alpha, restores controls, and keeps rescued focus in the modal", async ({
  page,
  request,
}) => {
  const alphaSlug = await projectSlug(request, "Alpha Project");
  const betaSlug = await projectSlug(request, "Beta Project");
  await page.route(
    (url) => url.pathname === `/api/repos/${betaSlug}/data`,
    async (route) => route.abort("failed"),
  );

  await page.locator("#new-story-btn").click();
  const pending = await dispatchProjectChangeNow(page, betaSlug, "create-state");
  expect(pending.activeId).toBe("create-modal");
  expect(pending.stateDisabled).toBe(true);

  await expect(page.locator("#create-error")).toContainText(
    "Failed to load BB · Beta Project's states and types",
  );
  await expect(page.locator("#create-error")).toContainText(
    "still creating in AA · Alpha Project",
  );
  await expect(page.locator("#create-project")).toHaveValue(alphaSlug);
  await expect(page.locator("#create-submit")).toBeEnabled();
  await expect(page.locator("#create-save-draft")).toBeEnabled();
  await expect(page.locator("#create-discard")).toBeEnabled();
  await expect(page.locator("#create-state")).toBeEnabled();
  await expect(page.locator("#create-type")).toBeEnabled();
  await expect(page.locator("#create-modal")).toBeFocused();

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

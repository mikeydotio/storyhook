import { execFileSync } from "node:child_process";
import { test, expect } from "./support";
import { openFilters, openProject, requiredEnv, seedToken, storyBinary } from "./support";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

test("filters and display choices follow one token into another browser", async ({
  page, browser,
}) => {
  await openProject(page, "Alpha Project");
  const filterSave = page.waitForResponse((response) =>
    response.url().endsWith("/api/preferences") &&
    response.request().method() === "PATCH" &&
    response.request().postData()?.includes('"filter"') === true,
  );
  await page.locator("#search-input").fill("flow");
  await filterSave;
  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect.poll(async () => {
    const response = await page.context().request.get(new URL("/api/preferences", page.url()).toString(), {
      headers: { "X-Storyhook": "1" },
    });
    return response.json();
  }).toMatchObject({ view: "list" });

  const otherContext = await browser.newContext();
  try {
    const other = await otherContext.newPage();
    await seedToken(other);
    await other.goto("/");
    await expect(other.locator("#list-view")).toBeVisible();
    await expect(other.locator("#search-input")).toHaveValue("flow");
    await expect(other.locator("#filter-count")).toHaveText("1 / 2");
  } finally {
    await otherContext.close();
  }
});

test("a different named token starts with defaults", async ({ page }) => {
  await openProject(page, "Alpha Project");
  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect.poll(async () => {
    const response = await page.context().request.get(new URL("/api/preferences", page.url()).toString(), {
      headers: { "X-Storyhook": "1" },
    });
    return response.json();
  }).toMatchObject({ view: "list" });

  const name = `prefs-${Date.now()}`;
  const otherToken = execFileSync(storyBinary(), ["token", "new", name], {
    encoding: "utf8",
  }).trim();
  try {
    await page.context().addCookies([{
      name: requiredEnv("DASHBOARD_COOKIE_NAME"), value: otherToken,
      domain: "127.0.0.1", path: "/", httpOnly: true,
      sameSite: "Strict", secure: false,
    }]);
    await page.reload();
    await expect(page.locator("#board-view")).toBeVisible();
    await expect(page.locator("#search-input")).toHaveValue("");
  } finally {
    execFileSync(storyBinary(), ["token", "revoke", name]);
  }
});

test("a refused preference save restores the confirmed control", async ({ page }) => {
  await openProject(page, "Alpha Project");
  await page.locator("#filter-toggle-btn").click();
  await page.route("**/api/preferences", async (route) => {
    if (route.request().method() === "PATCH") {
      await route.fulfill({ status: 500, body: "write failed" });
    } else {
      await route.continue();
    }
  });
  await page.locator("#toggle-hide-empty-columns").click();
  await expect(page.locator("#toggle-hide-empty-columns")).not.toBeChecked();
  await expect(page.locator("#toast-stack")).toContainText("Could not save dashboard preference");
});

test("a refused column choice restores its checkbox and board", async ({ page }) => {
  await openProject(page, "Alpha Project");
  await openFilters(page);
  await page.locator("#fdd-columns .fdd-btn").click();
  const todo = page.locator("#fdd-columns .fdd-option", { hasText: "todo" })
    .locator("input[type=checkbox]");
  await page.route("**/api/preferences", async (route) => {
    if (route.request().method() === "PATCH" &&
        route.request().postData()?.includes('"hiddenColumns"')) {
      await route.fulfill({ status: 500, body: "write failed" });
    } else {
      await route.continue();
    }
  });
  await todo.click();
  await expect(todo).toBeChecked();
  await expect(page.locator('.column[data-state="todo"]')).toHaveCount(1);
  await expect(page.locator("#toast-stack")).toContainText("Could not save dashboard preference");
});

test("a preference write is not replayed with a replacement token", async ({ page }) => {
  await openProject(page, "Alpha Project");
  let refused = false;
  let replayed = false;
  await page.route("**/api/preferences", async (route) => {
    const isViewWrite = route.request().method() === "PATCH" &&
      route.request().postData()?.includes('"view"') === true;
    if (isViewWrite && !refused) {
      refused = true;
      await route.fulfill({ status: 401, body: "Unauthorized" });
    } else {
      if (isViewWrite) replayed = true;
      await route.continue();
    }
  });

  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#token-modal")).toHaveClass(/open/);
  const newTokenLoad = page.waitForResponse((response) =>
    response.url().endsWith("/api/preferences") &&
    response.request().method() === "GET" && response.ok(),
  );
  await page.locator("#token-input").fill(requiredEnv("DASHBOARD_NAMED_TOKEN"));
  await page.locator("#token-submit").click();
  await newTokenLoad;
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#board-view")).toBeVisible();
  await page.waitForTimeout(250);
  expect(replayed).toBe(false);
  const preferences = await page.context().request.get(new URL("/api/preferences", page.url()).toString(), {
    headers: { "X-Storyhook": "1" },
  });
  expect((await preferences.json()).view).toBe("board");
});

test("submitted dispatch defaults reopen with the same token", async ({ page }) => {
  await openProject(page, "Alpha Project");
  await page.locator(".card-title", { hasText: "Wire up the auth flow" }).click();
  await page.route(/\/api\/repos\/[^/]+\/story\/[^/]+\/dispatch\?/, async (route) => {
    await route.fulfill({ status: 503, body: "dispatch disabled in this preference test" });
  });
  await page.locator("#dispatch-btn").click();
  await page.locator("#dispatch-agent").selectOption("codex");
  await page.locator("#dispatch-model").selectOption("gpt-5.6-sol");
  await page.locator("#dispatch-effort").selectOption("low");
  await page.locator("#dispatch-auto").check();
  await page.locator("#dispatch-modal-submit").click();

  const preferencesUrl = new URL("/api/preferences", page.url()).toString();
  await expect.poll(async () => {
    const response = await page.context().request.get(preferencesUrl, {
      headers: { "X-Storyhook": "1" },
    });
    return (await response.json()).dispatchDefaults;
  }).toMatchObject({
    agent: "codex", auto: true,
    byAgent: { codex: { model: "gpt-5.6-sol", effort: "low" } },
  });

  const another = await page.context().newPage();
  await another.goto("/");
  await expect(another.locator("#board-view")).toBeVisible();
  await another.locator(".card-title", { hasText: "Wire up the auth flow" }).click();
  await another.locator("#dispatch-btn").click();
  await expect(another.locator("#dispatch-agent")).toHaveValue("codex");
  await expect(another.locator("#dispatch-model")).toHaveValue("gpt-5.6-sol");
  await expect(another.locator("#dispatch-effort")).toHaveValue("low");
  await expect(another.locator("#dispatch-auto")).toBeChecked();
});

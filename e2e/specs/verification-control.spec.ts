import { test, expect, openProject, seedToken, projectSlug } from "./support";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("verifier menu hosts non-editable commands and supports keyboard navigation", async ({ page }) => {
  const stop = page.locator('.column[data-state="verifying"]')
    .getByRole("button", { name: "Stop verifier", exact: true });
  await stop.focus();
  await stop.press("Enter");
  const menu = page.getByRole("menu", { name: "Stop verifier" });
  const items = menu.getByRole("menuitem");
  await expect(items).toHaveCount(2);
  // This is the no-typing invariant behind the direct keydown receiver entry.
  await expect(menu.locator('input, textarea, select, [contenteditable]:not([contenteditable="false"])')).toHaveCount(0);
  for (const item of await items.all()) {
    await expect(item).toHaveJSProperty("isContentEditable", false);
  }
  await expect(items.nth(0)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(items.nth(1)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(items.nth(0)).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await expect(items.nth(1)).toBeFocused();
  await page.keyboard.press("Home");
  await expect(items.nth(0)).toBeFocused();
  await page.keyboard.press("End");
  await expect(items.nth(1)).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
  await expect(stop).toBeFocused();
});

test("verifier stop menu drains, persists after reload, and starts again", async ({ page }) => {
  const column = page.locator('.column[data-state="verifying"]');
  const stop = column.getByRole("button", { name: "Stop verifier", exact: true });
  await stop.click();
  const menu = page.getByRole("menu", { name: "Stop verifier" });
  await expect(menu.getByRole("menuitem", { name: "Let inflight verifications finish" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Stop inflight verifications" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(stop).toBeFocused();
  await stop.press("Enter");
  await page.getByRole("menuitem", { name: "Let inflight verifications finish" }).press("Enter");
  const start = column.getByRole("button", { name: "Start verifier", exact: true });
  await expect(start).toBeVisible();
  await expect(start.locator('[data-emoji="play"]')).toBeVisible();
  await page.reload();
  await expect(start).toBeVisible();
  await start.click();
  await expect(stop).toBeVisible();
  await expect(stop.locator('[data-emoji="stop"]')).toBeVisible();
});

test("stop inflight choice leaves an idle verifier stopped until explicitly started", async ({ page }) => {
  const column = page.locator('.column[data-state="verifying"]');
  await column.getByRole("button", { name: "Stop verifier", exact: true }).click();
  await page.getByRole("menuitem", { name: "Stop inflight verifications" }).click();
  const start = column.getByRole("button", { name: "Start verifier", exact: true });
  await expect(start).toBeVisible();
  await start.click();
  await expect(column.getByRole("button", { name: "Stop verifier", exact: true })).toBeVisible();
});

test("a rejected stop keeps the running control enabled", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const path = `/api/repos/${encodeURIComponent(slug)}/verification/control`;
  await page.route((url) => url.pathname === path, (route) => route.fulfill({
    status: 422, json: { error: "Verifier control fixture refusal" },
  }));
  const column = page.locator('.column[data-state="verifying"]');
  const stop = column.getByRole("button", { name: "Stop verifier", exact: true });
  await stop.click();
  await page.getByRole("menuitem", { name: "Stop inflight verifications" }).click();
  await expect(page.getByText("Verifier control fixture refusal", { exact: false })).toBeVisible();
  await expect(stop).toBeEnabled();
  await expect(column.getByRole("button", { name: "Start verifier", exact: true })).toHaveCount(0);
});

test("draining can escalate and restart waits for owned cleanup", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const base = `/api/repos/${encodeURIComponent(slug)}`;
  let mode = "draining";
  await page.route((url) => url.pathname === `${base}/data`, async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    data.verification_control = { state: mode };
    await route.fulfill({ response, json: data });
  });
  await page.route((url) => url.pathname === `${base}/verification/control`, async (route) => {
    expect(route.request().postDataJSON()).toEqual({ action: "stop" });
    mode = "stopping";
    await route.fulfill({ status: 200, json: { state: mode } });
  });
  await page.reload();
  const column = page.locator('.column[data-state="verifying"]');
  await expect(column.getByRole("status")).toHaveText("Finishing inflight verification…");
  await column.getByRole("button", { name: "Stop verifier", exact: true }).click();
  const menu = page.getByRole("menu", { name: "Stop verifier" });
  await expect(menu.getByRole("menuitem", { name: "Let inflight verifications finish" })).toHaveAttribute("aria-disabled", "true");
  const cancel = menu.getByRole("menuitem", { name: "Stop inflight verifications" });
  await expect(cancel).toBeFocused();
  await cancel.press("Enter");
  await expect(column.getByRole("button", { name: "Stopping verifier…", exact: true })).toBeDisabled();
  await expect(column.getByRole("status")).toHaveText("Stopping inflight verification…");
  mode = "stopped";
  await page.reload();
  await expect(column.getByRole("button", { name: "Start verifier", exact: true })).toBeEnabled();
});

test("stopped admission remains visible after an incident is acknowledged", async ({ page }) => {
  const column = page.locator('.column[data-state="verifying"]');
  await column.getByRole("button", { name: "Stop verifier", exact: true }).click();
  await page.getByRole("menuitem", { name: "Stop inflight verifications" }).click();
  const banner = page.locator('#verification-banner-region');
  await expect(banner).toBeVisible();
  await expect(banner).toContainText("Central verification stopped");
  await expect(banner.getByRole("button", { name: "Start verifier", exact: true })).toBeVisible();
  await page.reload();
  await expect(banner).toContainText("Central verification stopped");
  await banner.getByRole("button", { name: "Start verifier", exact: true }).click();
  await expect(column.getByRole("button", { name: "Stop verifier", exact: true })).toBeVisible();
});

test("overdue evidence warning is visible outside the verifier column", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  await page.route((url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`, async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    data.verification_incident = null;
    data.verification_control = { state: "running" };
    data.verifier = { control: "running", warning: "fixture verifier has no progress evidence for 61s; story verifier status; story daemon logs", recovery: { acknowledgement: null, request: null } };
    await route.fulfill({ response, json: data });
  });
  await page.reload();
  const banner = page.locator('#verification-banner-region');
  await expect(banner).toContainText("no progress evidence for 61s");
  await expect(banner).toContainText("story verifier status");
});

test("a batch preview reads as activity in its column, not as attention", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  let preview: Record<string, unknown> = {
    computed_at: "2026-01-01T00:00:00Z",
    head: "ALPHA-7",
    cap: 2,
    queue_depth: 3,
    outcome: "batch",
    members: [
      { story_id: "ALPHA-7", commit: "a".repeat(40) },
      { story_id: "ALPHA-8", commit: "b".repeat(40) },
    ],
    excluded: [{ story_id: "ALPHA-9", reason: "conflict-with-member" }],
  };
  await page.route((url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`, async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    data.verification_incident = null;
    data.verification_control = { state: "running" };
    data.verifier = {
      ...data.verifier,
      control: "running",
      warning: null,
      reservation: null,
      recovery: { acknowledgement: null, request: null },
      batch_preview: preview,
    };
    await route.fulfill({ response, json: data });
  });
  await page.reload();
  const status = page.locator('.column[data-state="verifying"] .verification-control-status');
  await expect(status).toBeVisible();
  await expect(status).toHaveText("Batch preview · ALPHA-7 + ALPHA-8 would verify together · 1 excluded (cap 2)");
  await expect(page.locator("#verification-banner-region")).toBeHidden();

  preview = { ...preview, outcome: "head-conflict", members: [{ story_id: "ALPHA-7", commit: "a".repeat(40) }], excluded: [] };
  await page.reload();
  await expect(status).toHaveText("Batch preview · ALPHA-7 conflicts with its base; no batch");
  await expect(page.locator("#verification-banner-region")).toBeHidden();
});

test("a reserved verifier reads as activity in its column, not as attention", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  let mode = "running";
  await page.route((url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`, async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    data.verification_incident = null;
    data.verification_control = { state: mode };
    data.verifier = {
      ...data.verifier,
      control: mode,
      warning: null,
      recovery: { acknowledgement: null, request: null },
      reservation: {
        story_id: "ALPHA-7",
        generation: 42,
        reason: "reconcile",
        reserved_at: "2026-01-01T00:00:00Z",
        age_seconds: 540,
        queued_behind: 2,
      },
    };
    await route.fulfill({ response, json: data });
  });
  await page.reload();
  const status = page.locator('.column[data-state="verifying"] .verification-control-status');
  await expect(status).toBeVisible();
  await expect(status).toContainText("Held for ALPHA-7 · merge-conflict reconcile · since");
  await expect(status).toContainText("2 queued");
  await expect(status.locator("time")).toHaveAttribute("datetime", "2026-01-01T00:00:00Z");
  await expect(page.locator("#verification-banner-region")).toBeHidden();

  mode = "draining";
  await page.reload();
  await expect(status).toContainText("Finishing inflight verification…");
  await expect(status).toContainText("Held for ALPHA-7");
});

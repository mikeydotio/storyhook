import { cleanUpCreatedStories, expect, openProject, seedToken, test, waitForBoardData } from "./support";

cleanUpCreatedStories("Beta Project");

async function useFreshToken(page: import("@playwright/test").Page) {
  const name = `visibility-${Date.now()}-${Math.floor(Math.random() * 100000)}`;
  const minted = await page.request.post(`/api/v1/tokens?name=${name}`, {
    headers: {
      "X-Storyhook-Token": process.env.DASHBOARD_TOKEN!,
      "X-Storyhook": "1",
      Host: new URL(page.url()).host,
    },
  });
  expect(minted.ok()).toBeTruthy();
  const secret = (await minted.json()).token as string;
  await page.context().addCookies([{
    name: process.env.DASHBOARD_COOKIE_NAME!, value: secret,
    url: new URL(page.url()).origin, httpOnly: true, sameSite: "Strict",
  }]);
  await page.reload();
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await useFreshToken(page);
});

test("Settings checkboxes persist and filter Home cards, totals, and picker", async ({ page }) => {
  const catalog = await page.request.get("/api/repos", { headers: { "X-Storyhook": "1" } });
  expect(catalog.headers()["cache-control"]).toBe("no-store");
  const initialOpen = parseInt(await page.locator("#home-summary .home-stat").first().innerText(), 10);
  await page.locator("#settings-btn").click();
  const beta = page.getByRole("checkbox", { name: "Show Beta Project" });
  await expect(beta).toBeChecked();
  await beta.uncheck();
  await expect(beta).not.toBeChecked();

  await page.locator("#home-btn").click();
  await expect(page.locator(".repo-card-name", { hasText: "Beta Project" })).toHaveCount(0);
  await expect(page.locator("#home-summary .home-stat").first()).toContainText(String(initialOpen - 1));
  await page.locator("#projsel-btn").click();
  await expect(page.locator("#projsel-menu .projsel-item", { hasText: "Beta Project" })).toHaveCount(0);
  await page.reload();
  await page.locator("#settings-btn").click();
  await expect(page.getByRole("checkbox", { name: "Show Beta Project" })).not.toBeChecked();
  await page.getByRole("checkbox", { name: "Show Beta Project" }).check();
  await page.locator("#home-btn").click();
  await expect(page.locator(".repo-card-name", { hasText: "Beta Project" })).toBeVisible();
  await expect(page.locator("#home-summary .home-stat").first()).toContainText(String(initialOpen));
});

test("all hidden projects have a Settings route and direct links remain usable", async ({ page }) => {
  const response = await page.request.get("/api/repos", { headers: { "X-Storyhook": "1" } });
  const repos: Array<{ id: string; name: string }> = await response.json();
  const betaId = repos.find((repo) => repo.name === "Beta Project")!.id;

  await page.locator("#settings-btn").click();
  for (const repo of repos) {
    await page.getByRole("checkbox", { name: `Show ${repo.name}` }).uncheck();
  }
  await page.locator("#home-btn").click();
  await expect(page.getByText("No projects selected.")).toBeVisible();
  await expect(page.getByRole("button", { name: "Choose projects in Settings" })).toBeVisible();
  await expect(page.locator("#projsel-menu .projsel-item")).toHaveCount(0);

  await page.goto(`/?project=${encodeURIComponent(betaId)}`);
  await expect(page.locator("#projsel-btn")).toContainText("Beta Project");
  await page.locator("#home-btn").click();
  await expect(page.locator(".repo-card-name", { hasText: "Beta Project" })).toHaveCount(0);
});

test("preferences belong to the named token", async ({ page }) => {
  await page.locator("#settings-btn").click();
  await page.getByRole("checkbox", { name: "Show Beta Project" }).uncheck();

  await useFreshToken(page);
  await page.locator("#settings-btn").click();
  await expect(page.getByRole("checkbox", { name: "Show Beta Project" })).toBeChecked();
});

test("hidden projects leave global Drafts and new-story choices", async ({ page }) => {
  await openProject(page, "Beta Project");
  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill("Visibility draft");
  await page.locator("#create-save-draft").click();
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");

  await page.locator("#settings-btn").click();
  await page.getByRole("checkbox", { name: "Show Beta Project" }).uncheck();
  await expect(page.locator("#drafts-btn-text")).toHaveText("No Drafts");
  await page.locator("#drafts-btn").click();
  await expect(page.locator("#drafts-list .drafts-row", { hasText: "Visibility draft" })).toHaveCount(0);
  await page.locator("#drafts-backdrop").click({ position: { x: 4, y: 4 } });

  await page.locator("#home-btn").click();
  await openProject(page, "Alpha Project");
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-project option", { hasText: "Beta Project" })).toHaveCount(0);
  await page.locator("#create-discard").click();

  const response = await page.request.get("/api/repos", { headers: { "X-Storyhook": "1" } });
  const repos: Array<{ id: string; name: string }> = await response.json();
  const betaId = repos.find((repo) => repo.name === "Beta Project")!.id;
  await page.goto(`/?project=${encodeURIComponent(betaId)}`);
  await waitForBoardData(page);
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-project")).toHaveValue(betaId);
  await page.locator("#create-discard").click();

  await page.locator("#settings-btn").click();
  await page.getByRole("checkbox", { name: "Show Beta Project" }).check();
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");
});

test("keyboard toggle restores the choice and reports a failed save", async ({ page }) => {
  await page.route("**/api/repos/*/visibility", async (route) => {
    await route.fulfill({ status: 500, contentType: "text/plain", body: "storage unavailable" });
  });
  await page.locator("#settings-btn").click();
  const beta = page.getByRole("checkbox", { name: "Show Beta Project" });
  await beta.focus();
  await page.keyboard.press("Space");
  await expect(beta).toBeChecked();
  await expect(page.locator("#toast-stack .toast.error")).toContainText("storage unavailable");
});

test("another tab refreshes its checkbox after a catalog change", async ({ page }) => {
  const other = await page.context().newPage();
  await other.goto("/");
  await other.locator("#settings-btn").click();
  await expect(other.getByRole("checkbox", { name: "Show Beta Project" })).toBeChecked();

  await page.locator("#settings-btn").click();
  await page.getByRole("checkbox", { name: "Show Beta Project" }).uncheck();
  await expect(other.getByRole("checkbox", { name: "Show Beta Project" })).not.toBeChecked();
  await other.close();
});

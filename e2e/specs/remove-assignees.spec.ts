import { test, expect, cleanUpCreatedStories, createStory, openFilters, openProject, seedToken } from "./support";

cleanUpCreatedStories("Alpha Project");

test("creation, editing, Board and List work without assignment controls", async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await openFilters(page);
  await expect(page.locator("#fdd-assignees")).toHaveCount(0);
  const title = "SH-752 single-user story";
  const id = await createStory(page, title);
  const card = page.locator(`.card[data-id="${id}"]`);
  await expect(card.locator(".avatar")).toHaveCount(0);
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-body .field", { hasText: "Assignee" })).toHaveCount(0);
  const priority = page.locator("#drawer-body .field", { hasText: "Priority" }).locator("select");
  await priority.selectOption("high");
  await expect(priority).toHaveValue("high");
  await page.locator("#drawer-close").click();
  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Assignee" })).toHaveCount(0);
  await expect(page.locator(`#list-body tr[data-id="${id}"]`)).toContainText("high");
  await page.locator('th[data-col="title"]').click();
  await page.reload();
  await expect(page.locator(`#list-body tr[data-id="${id}"]`)).toContainText(title);
});

test("legacy assignment preferences normalize without losing other selections", async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const url = new URL("/api/preferences", page.url()).toString();
  const response = await page.context().request.patch(url, {
    headers: { "X-Storyhook": "1" },
    data: {
      filter: { text: "", priorities: [], types: null, states: null, assignees: ["ada"] },
      sort: { col: "assignee", dir: 1 }, view: "list",
    },
  });
  expect(response.ok()).toBeTruthy();
  const saved = await response.json();
  expect(saved.filter).toEqual({ text: "", priorities: [], types: null, states: null });
  expect(saved.sort).toEqual({ col: "updated", dir: -1 });
  await page.reload();
  await expect(page.locator("#list-view")).toBeVisible();
  await expect(page.locator("#filter-count")).toHaveText("0 / 2");
  await expect(page.locator("#fdd-assignees")).toHaveCount(0);
  const loaded = await page.context().request.get(url, { headers: { "X-Storyhook": "1" } });
  expect((await loaded.json()).filter).toEqual(saved.filter);
});

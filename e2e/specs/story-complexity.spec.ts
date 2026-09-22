import { test, expect, cleanUpCreatedStories, createStory, openProject, seedToken } from "./support";

cleanUpCreatedStories("Alpha Project");

test("complexity edit persists and default dispatch shows the resolved choice", async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const id = await createStory(page, "SH-756 complexity edit");
  const card = page.locator(`.card[data-id="${id}"]`);
  await expect(card).toContainText("medium · unassessed");
  await card.click();
  await page.locator('#drawer-body select[aria-label="Complexity"]').selectOption("high");
  await expect(page.locator('#drawer-body select[aria-label="Complexity"]')).toHaveValue("high");
  await page.reload();
  await expect(page.locator(`.card[data-id="${id}"]`)).toContainText("high");
  await page.locator(`.card[data-id="${id}"]`).click();
  await page.getByRole("button", { name: "Dispatch", exact: true }).click();
  await page.locator("#dispatch-agent").selectOption("codex");
  await page.locator("#dispatch-model").selectOption("");
  await page.locator("#dispatch-effort").selectOption("");
  await expect(page.locator("#dispatch-policy-preview")).toContainText("gpt-6-astra");
  await expect(page.locator("#dispatch-policy-preview")).toContainText("xhigh");
});

test("explicit medium creation is assessed", async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill("SH-756 assessed medium");
  await page.locator("#create-complexity").selectOption("medium");
  await page.locator("#create-submit").click();
  const card = page.locator('.card', { hasText: "SH-756 assessed medium" });
  await expect(card.locator(".story-complexity")).toHaveText("medium");
});

test("installation and project policy fields inherit and reset independently", async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await page.locator("#settings-btn").click();
  const scope = page.locator("#policy-scope");
  const model = page.getByRole("combobox", { name: "codex medium model", exact: true });
  const effort = page.getByRole("combobox", { name: "codex medium effort", exact: true });
  const row = page.locator("#policy-rows tr", { has: model });
  await model.selectOption("gpt-5.6-sol");
  await expect(page.locator("#policy-status")).toHaveText("Policy saved.");
  await expect(row).toContainText("gpt-5.6-sol (installation)");
  await scope.selectOption({ label: "Alpha Project" });
  await expect(model).toHaveValue("");
  await expect(row).toContainText("gpt-5.6-sol (installation)");
  await effort.selectOption("xhigh");
  await expect(row).toContainText("xhigh (project)");
  await page.reload();
  await page.locator("#settings-btn").click();
  await scope.selectOption({ label: "Alpha Project" });
  await expect(effort).toHaveValue("xhigh");
  await expect(row).toContainText("gpt-5.6-sol (installation)");
  await effort.selectOption("");
  await expect(row).toContainText("high (builtin)");
  await scope.selectOption("");
  await model.selectOption("");
  await expect(row).toContainText("gpt-6-astra (builtin)");
});

test("policy scope follows a project catalog that arrives after Settings opens", async ({ page }) => {
  await seedToken(page);
  let release!: () => void;
  const catalog = new Promise<void>(resolve => { release = resolve; });
  await page.route("**/api/repos", async route => {
    await catalog;
    await route.continue();
  });
  try {
    await page.goto("/");
    await page.locator("#settings-btn").click();
    await expect(page.locator("#policy-scope option")).toHaveText(["Installation"]);
  } finally {
    release();
  }
  await expect(page.locator("#policy-scope option", { hasText: "Alpha Project" })).toHaveCount(1);
});

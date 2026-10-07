import { isBoardMetadata } from "./board-network";
import { test, expect, openProject, projectSlug, seedToken } from "./support";

for (const infrastructureHalted of [false, true]) {
  test(`project recovery keeps its own diagnosis with infrastructure halt=${infrastructureHalted}`, async ({ page, request }) => {
    await seedToken(page);
    const slug = await projectSlug(request, "Alpha Project");
    await page.route((url) => isBoardMetadata(url, slug), async route => {
      if (route.request().method() !== "GET") { await route.continue(); return; }
      const response = await route.fetch();
      const data = await response.json();
      data.verification_control = { state: "running" };
      data.verification_incident = infrastructureHalted ? {
        incident_id: "unrelated-host-halt", halted: true, attempts: 1,
        story_id: "SH-999", detail: "Host credentials unavailable", first_failed_at: "2026-01-01T00:00:00Z",
      } : null;
      data.verifier = { ...data.verifier, warning: null, project_recoveries: [{
        id: "recovery-fixture", fault: "missing-certification", locus: ".storyhook.toml#verify.gate",
        affected_stories: ["SH-1", "SH-3"], assessment_owner: "SH-1", repair_story: "SH-2",
        repair_link: "https://github.com/example/project/pull/2", phase: "repair-pending",
        completed_attempts: 1, attempt_limit: 3,
        next_action: "Wait for managed repair delivery for SH-2. <script> must remain text.",
      }] };
      await route.fulfill({ response, json: data });
    });
    await page.goto("/");
    await openProject(page, "Alpha Project");
    const recovery = page.locator(".project-recovery-banner");
    await expect(recovery).toBeVisible();
    await expect(recovery).toContainText("missing-certification");
    await expect(recovery).toContainText("repair-pending");
    await expect(recovery).toContainText("SH-1, SH-3");
    await expect(recovery).toContainText("1/3");
    await expect(recovery).toContainText("<script> must remain text");
    await expect(recovery.getByRole("link", { name: "Repair PR" })).toHaveAttribute("href", "https://github.com/example/project/pull/2");
    await expect(recovery.getByRole("button", { name: /acknowledge|retry/i })).toHaveCount(0);
    await expect(page.locator(".verification-halted-banner")).toHaveCount(infrastructureHalted ? 1 : 0);
  });
}

test("invalid project recovery shows literal diagnostics without ownership or counts", async ({ page, request }) => {
  await seedToken(page);
  const slug = await projectSlug(request, "Alpha Project");
  const diagnostic = 'Recovery invalid-fixture is invalid: <script>alert("invalid")</script>. Inspect: story verifier repair show invalid-fixture --json';
  await page.route((url) => isBoardMetadata(url, slug), async route => {
    if (route.request().method() !== "GET") { await route.continue(); return; }
    const response = await route.fetch();
    const data = await response.json();
    data.verifier = { ...data.verifier, project_recoveries: [{
      id: "invalid-fixture", fault: "missing-certification", locus: "gate/<invalid>",
      affected_stories: [], assessment_owner: "", repair_story: null, repair_link: null,
      phase: "invalid", completed_attempts: 0, attempt_limit: 0, next_action: diagnostic,
    }] };
    await route.fulfill({ response, json: data });
  });
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const recovery = page.locator(".project-recovery-banner");
  await expect(recovery).toBeVisible();
  for (const text of ["invalid-fixture", "missing-certification", "invalid", "gate/<invalid>", diagnostic]) {
    await expect(recovery).toContainText(text);
  }
  for (const text of ["Affected:", "assessor", "undecided", "Completed repair attempts:", "0/0"]) {
    await expect(recovery).not.toContainText(text);
  }
  await expect(recovery.locator("script, invalid")).toHaveCount(0);
  await expect(recovery.getByRole("link", { name: "Repair PR" })).toHaveCount(0);
  await expect(recovery.locator("code")).toHaveText("story verifier repair show invalid-fixture --json");
});

test("external-scope recovery names its scope instead of an undecided repair", async ({ page, request }) => {
  await seedToken(page);
  const slug = await projectSlug(request, "Alpha Project");
  await page.route((url) => isBoardMetadata(url, slug), async route => {
    if (route.request().method() !== "GET") { await route.continue(); return; }
    const response = await route.fetch();
    const data = await response.json();
    data.verifier = { ...data.verifier, warning: null, project_recoveries: [{
      id: "external-fixture", fault: "missing-certification", locus: ".storyhook.toml#verify.gate",
      affected_stories: ["SH-1"], assessment_owner: "SH-1", repair_story: null, repair_link: null,
      phase: "external-prerequisite", scope: "external", completed_attempts: 0, attempt_limit: 3,
      next_action: "The owner must restore access to the signing service. An operator, not an agent, must restore it.",
    }] };
    await route.fulfill({ response, json: data });
  });
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const recovery = page.locator(".project-recovery-banner");
  await expect(recovery).toBeVisible();
  await expect(recovery).toContainText("external-prerequisite");
  await expect(recovery).toContainText("repair none (external)");
  await expect(recovery).not.toContainText("undecided");
});

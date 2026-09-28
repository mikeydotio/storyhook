import { test, expect, openProject, projectSlug, seedToken } from "./support";

// SH-771: a repository that already committed files under .storyhook/logs
// cannot be fixed by the journal's own ignore file. The daemon's hygiene
// sweep reports it on `verifier.journal_warning`, and the banner must show
// it whether or not the verification queue itself needs attention.
const WARNING =
  "Alpha Project: git tracks 2 activity journal files in /checkouts/alpha/.storyhook/logs; " +
  "an ignore file cannot hide them. Run `git rm -r --cached .storyhook/logs` in that checkout, " +
  "then commit. Storyhook never changes the index. <script> must remain text.";

for (const infrastructureHalted of [false, true]) {
  test(`tracked journal files keep their own banner with infrastructure halt=${infrastructureHalted}`, async ({ page, request }) => {
    await seedToken(page);
    const slug = await projectSlug(request, "Alpha Project");
    await page.route((url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`, async route => {
      const response = await route.fetch();
      const data = await response.json();
      data.verification_control = { state: "running" };
      data.verification_incident = infrastructureHalted ? {
        incident_id: "unrelated-host-halt", halted: true, attempts: 1,
        story_id: "SH-999", detail: "Host credentials unavailable", first_failed_at: "2026-01-01T00:00:00Z",
      } : null;
      data.verifier = { ...data.verifier, warning: null, project_recoveries: [], journal_warning: WARNING };
      await route.fulfill({ response, json: data });
    });
    await page.goto("/");
    await openProject(page, "Alpha Project");
    const banner = page.locator(".journal-hygiene-banner");
    await expect(banner).toBeVisible();
    await expect(banner).toContainText("Activity journal tracked by git");
    await expect(banner).toContainText("git rm -r --cached .storyhook/logs");
    await expect(banner).toContainText("<script> must remain text");
    await expect(banner.getByRole("button")).toHaveCount(0);
    await expect(page.locator(".verification-halted-banner")).toHaveCount(infrastructureHalted ? 1 : 0);
  });
}

test("no journal banner appears without a journal warning", async ({ page, request }) => {
  await seedToken(page);
  const slug = await projectSlug(request, "Alpha Project");
  await page.route((url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`, async route => {
    const response = await route.fetch();
    const data = await response.json();
    data.verification_control = { state: "running" };
    data.verification_incident = null;
    data.verifier = { ...data.verifier, warning: null, project_recoveries: [] };
    delete data.verifier.journal_warning;
    await route.fulfill({ response, json: data });
  });
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await expect(page.locator(".journal-hygiene-banner")).toHaveCount(0);
  await expect(page.locator("#verification-banner-region")).toBeHidden();
});

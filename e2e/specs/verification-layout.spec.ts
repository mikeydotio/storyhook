import type { Locator } from "@playwright/test";
import { test, expect, onAFrozenClock, openProject, projectSlug, seedToken } from "./support";

/** SH-611: text presence alone cannot detect a path painted outside its badge. */
async function expectContainedText(chip: Locator): Promise<void> {
  const geometry = await chip.evaluate((node) => {
    const card = node.closest<HTMLElement>(".card");
    if (!card) throw new Error("verification badge has no story card");
    const bounds = (element: Element) => {
      const box = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return {
        left: box.left + parseFloat(style.borderLeftWidth) + parseFloat(style.paddingLeft),
        right: box.right - parseFloat(style.borderRightWidth) - parseFloat(style.paddingRight),
        top: box.top + parseFloat(style.borderTopWidth) + parseFloat(style.paddingTop),
        bottom: box.bottom - parseFloat(style.borderBottomWidth) - parseFloat(style.paddingBottom),
      };
    };
    const rect = (box: DOMRect) => ({
      left: box.left, right: box.right, top: box.top, bottom: box.bottom,
    });
    const range = document.createRange();
    range.selectNodeContents(node);
    return {
      lines: Array.from(range.getClientRects(), rect),
      content: bounds(node),
      badge: rect(node.getBoundingClientRect()),
      card: bounds(card),
      // A single physical pixel accommodates fractional glyph/layout rounding.
      tolerance: 1 / window.devicePixelRatio,
    };
  });
  expect(geometry.lines.length).toBeGreaterThan(0);
  for (const [inner, outer] of [
    ...geometry.lines.map((line) => [line, geometry.content]),
    [geometry.badge, geometry.card],
  ]) {
    expect(inner.left).toBeGreaterThanOrEqual(outer.left - geometry.tolerance);
    expect(inner.right).toBeLessThanOrEqual(outer.right + geometry.tolerance);
    expect(inner.top).toBeGreaterThanOrEqual(outer.top - geometry.tolerance);
    expect(inner.bottom).toBeLessThanOrEqual(outer.bottom + geometry.tolerance);
  }
}

test("verification text stays inside cards on initial render, refresh, and timer ticks", async ({
  page, request, isMobile,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.clock.install();
  await seedToken(page);
  await page.goto("/");
  const slug = await projectSlug(request, "Alpha Project");
  const compiler = "compiling storyhook_test_support v0.0.0 " +
    "(/Volumes/Code/mikeyward/storyhook/.git/storyhook/verifier-worktree/crates/storyhook_test_support)";
  const token = "uninterrupted".repeat(32);
  let label = compiler;
  let stalled = false;

  // Clone the daemon's real wire shape, replacing data only. The dashboard's
  // render/reconcile and elapsed-label timer execute unchanged.
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    async (route) => {
      const response = await route.fetch();
      const data = await response.json();
      const template = data.stories?.[0];
      if (!template) throw new Error("verification layout fixture has no story to clone");
      const view = JSON.parse(JSON.stringify(template));
      view.story.id = "SH-9611";
      view.story.title = "SH-611 verification layout fixture";
      view.story.state = "verifying";
      view.story.superstate = "OPEN";
      view.story.story_type = "bug";
      view.story.labels = [];
      view.story.relationships = [];
      view.story.awaiting = null;
      view.display_state = null;
      view.is_ready = false;
      view.is_blocked = false;
      view.open_prs = [];
      view.verification = stalled
        ? { status: "stalled", attempts: 3, detail: label, halted: true }
        : { status: "running", elapsed_seconds: 144,
            current_step: { label, elapsed_seconds: 18 } };
      data.stories.push(view);
      await route.fulfill({ response, json: data });
    },
  );

  for (const width of isMobile ? [320, 375, 390] : [1280]) {
    await page.setViewportSize({ width, height: 844 });
    for (const sample of [compiler, token, "compiling", "stalled " + token]) {
      label = sample;
      stalled = sample.startsWith("stalled ");
      await test.step(`${width}px: ${stalled ? "stalled" : sample === compiler ? "compiler path" : sample === token ? "single token" : "short status"}`, async () => {
        await onAFrozenClock(page, async () => {
          await openProject(page, "Alpha Project");
          const card = page.locator('.card[data-id="SH-9611"]');
          const chip = card.locator(".verification-chip");
          const description = stalled
            ? "Verification halted · attempt 3 · " + sample
            : "Verifying · 2m 24s total · " + sample.replace(/[-_]+/g, " ") + " 18s";
          await expect(chip).toHaveText(description);
          await expect(card).toHaveAttribute("aria-label", "SH-9611: SH-611 verification layout fixture — " + description);
          await expectContainedText(chip);
          await page.clock.runFor(1000);
          const updated = stalled ? description : description.replace("2m 24s total", "2m 25s total").replace(/18s$/, "19s");
          await expect(chip).toHaveText(updated);
          await expect(card).toHaveAttribute("aria-label", "SH-9611: SH-611 verification layout fixture — " + updated);
          await expectContainedText(chip);
        });
        // Returning through the real project navigation obtains fresh status
        // data for the next sample rather than writing directly into the DOM.
        await page.locator("#home-btn").click();
      });
    }
  }
});

import { writeFileSync } from "node:fs";
import type { Locator, Page, TestInfo } from "@playwright/test";
import { test, expect, latch, openProject, seedToken } from "./support";

/** Observe native picker state without replacing focus or activation behavior. */
async function pickerState(page: Page) {
  return page.evaluate(() => ({
    supported: CSS.supports("selector(:open)"),
    open: CSS.supports("selector(:open)")
      ? Array.from(document.querySelectorAll("select:open"), (node) => node.id)
      : null,
    focus: document.activeElement?.id,
    provider: (document.getElementById("dispatch-agent") as HTMLSelectElement).value,
    events: (window as unknown as { providerEvents: unknown[] }).providerEvents,
    userAgent: navigator.userAgent,
  }));
}

/** Keep successful observations as well as failure traces for the investigation. */
async function record(page: Page, info: TestInfo, name: string) {
  const observed = await pickerState(page);
  const path = info.outputPath(`${name}.json`);
  writeFileSync(path, JSON.stringify(observed, null, 2));
  await info.attach(name, { path, contentType: "application/json" });
  return observed;
}

/** Activate the actual invoker; keyboard cases keep the complete key sequence. */
async function activate(locator: Locator, mode: string) {
  if (mode === "pointer") await locator.click();
  else {
    await locator.focus();
    await locator.press(mode);
  }
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.evaluate(() => {
    const events: unknown[] = [];
    (window as unknown as { providerEvents: unknown[] }).providerEvents = events;
    for (const type of ["pointerdown", "pointerup", "mousedown", "mouseup", "click", "keydown", "keyup", "focusin", "input", "change"]) {
      document.addEventListener(type, (event) => {
        const target = event.target as HTMLElement;
        events.push({ type, target: target.id || target.className, key: (event as KeyboardEvent).key });
      }, true);
    }
  });
});

for (const mode of ["pointer", "Enter", "Space"]) {
  test(`New does not activate a provider picker: ${mode}`, async ({ page }, info) => {
    await activate(page.locator("#new-story-btn"), mode);
    await expect(page.locator("#create-modal")).toHaveClass(/open/);
    await expect(page.locator("#create-title")).toBeFocused();
    await expect(page.locator("#dispatch-modal")).not.toHaveClass(/open/);
    await expect(page.locator("#engine-modal")).not.toHaveClass(/open/);
    await page.keyboard.type("SH-716 title input");
    await expect(page.locator("#create-title")).toHaveValue("SH-716 title input");
    const observed = await record(page, info, "new-activation");
    expect(observed.open).toEqual([]);
  });

  for (const entry of ["drawer", "menu"]) {
    test(`Dispatch does not activate its provider picker: ${entry} ${mode}`, async ({ page }, info) => {
      const card = page.locator(".card", { hasText: "Wire up the auth flow" });
      if (entry === "drawer") {
        await card.click();
        await activate(page.locator("#dispatch-btn"), mode);
      } else {
        await card.click({ button: "right" });
        const dispatch = page.getByRole("menuitem", { name: "Dispatch", exact: true });
        if (mode === "pointer") await dispatch.click();
        else {
          const index = await dispatch.evaluate((node) =>
            Array.from(node.parentElement!.querySelectorAll(".ctxmenu-item")).indexOf(node),
          );
          expect(index).toBeGreaterThanOrEqual(0);
          await page.keyboard.press("Home");
          for (let i = 0; i < index; i++) await page.keyboard.press("ArrowDown");
          await expect(dispatch).toBeFocused();
          await page.keyboard.press(mode);
        }
      }
      await expect(page.locator("#dispatch-modal")).toHaveClass(/open/);
      const initial = await record(page, info, "initial-activation");
      expect(initial.open, "opening the dialog must not open a native picker").toEqual([]);
      await expect(page.locator("#dispatch-agent")).toHaveValue("claude");

      // A negative observation is evidence only if the same observer sees an
      // explicitly opened picker in this browser. Do not silently skip it.
      await page.locator("#dispatch-agent").click();
      const explicit = await record(page, info, "explicit-activation");
      expect(explicit.open, "positive control: native picker must be observable").toContain("dispatch-agent");
      await page.keyboard.press("Escape");
    });
  }
}

test("catalog arrival and repeated Dispatch-to-New opens do not activate Provider", async ({ page }, info) => {
  const gate = latch();
  const requested = latch();
  await page.route("**/api/dispatch-options", async (route) => {
    requested.release();
    await gate.held;
    await route.continue();
  });
  try {
    await page.locator(".card", { hasText: "Wire up the auth flow" }).click();
    await page.locator("#dispatch-btn").click();
    await requested.held;
    expect((await record(page, info, "catalog-pending")).open).toEqual([]);
    gate.release();
    await expect(page.locator("#dispatch-modal-submit")).toBeEnabled();
    expect((await record(page, info, "catalog-arrived")).open).toEqual([]);
    for (let i = 0; i < 3; i++) {
      await page.locator("#dispatch-modal-cancel").click();
      await page.locator("#new-story-btn").click();
      await expect(page.locator("#create-title")).toBeFocused();
      expect((await record(page, info, `new-after-dispatch-${i}`)).open).toEqual([]);
      await page.locator("#create-discard").click();
      await page.locator("#dispatch-btn").click();
      expect((await record(page, info, `dispatch-reopened-${i}`)).open).toEqual([]);
    }
  } finally {
    gate.release();
    await page.unrouteAll({ behavior: "wait" });
  }
});

import { test, expect } from "./support";
import { withAssertionGrace } from "../expect-grace";
import { BASE_EXPECT_TIMEOUT_MS, gracedPatience } from "../load-grace";

/** Greater than the stale default and below the controlled ratio-two grant. */
const READY_DELAY_MS = BASE_EXPECT_TIMEOUT_MS * 1.2;
/** An exact deadline used to prove that explicit options outrank grace. */
const PROOF_TIMEOUT_MS = BASE_EXPECT_TIMEOUT_MS / 50;

for (const matcher of ["toBeVisible", "toHaveText", "toContainText"] as const) {
  test(`${matcher} uses a new sample after the startup default`, async ({ page }) => {
    let ratio = 0.3;
    const checked = withAssertionGrace(expect, () => gracedPatience(ratio));
    await page.setContent('<div id="result" hidden>waiting</div>');
    ratio = 2;
    await page.evaluate((delay) => {
      setTimeout(() => {
        const result = document.querySelector<HTMLElement>("#result")!;
        result.hidden = false;
        result.textContent = "ready";
      }, delay);
    }, READY_DELAY_MS);
    const assertion = checked(page.locator("#result"));
    if (matcher === "toBeVisible") await assertion.toBeVisible();
    else await assertion[matcher]("ready");
  });
}

test("explicit matcher deadlines and hidden-text refusals survive the adapter", async ({ page }) => {
  const checked = withAssertionGrace(expect, () => BASE_EXPECT_TIMEOUT_MS * 2);
  await page.setContent('<div id="result">waiting</div><button>Label<span aria-hidden="true">*</span></button>');
  const result = page.locator("#result");
  for (const matcher of ["toHaveText", "toContainText"] as const) {
    await expect(checked(result)[matcher]("ready", { timeout: PROOF_TIMEOUT_MS }))
      .rejects.toThrow(`${PROOF_TIMEOUT_MS}ms`);
    await checked(result)[matcher]("waiting", { timeout: 0 });
    await checked(result).not[matcher]("ready", { timeout: 0 });
  }
  await expect(checked(page.locator("#missing")).toBeVisible({ timeout: PROOF_TIMEOUT_MS }))
    .rejects.toThrow(`${PROOF_TIMEOUT_MS}ms`);
  await expect(checked(page.locator("button")).toHaveText("Label*"))
    .rejects.toThrow(/aria-hidden/);
  await expect(checked(page.locator("button")).toContainText("*"))
    .rejects.toThrow(/aria-hidden/);
});

test("text delegates use the effective default instead of ambient config patience", async ({ page }) => {
  await page.setContent('<div id="result">waiting</div>');
  const result = page.locator("#result");
  for (const matcher of ["toHaveText", "toContainText"] as const) {
    // Error diagnostics establish the actual deadline without an elapsed-time
    // assertion that could itself flake when the worker is descheduled.
    for (const budget of [PROOF_TIMEOUT_MS, PROOF_TIMEOUT_MS * 2]) {
      const checked = withAssertionGrace(expect, () => budget);
      await expect(checked(result)[matcher]("ready")).rejects.toThrow(`${budget}ms`);
      await expect(checked(result).not[matcher]("waiting")).rejects.toThrow(`${budget}ms`);
    }
    const fixed = expect.configure({ timeout: PROOF_TIMEOUT_MS });
    await expect(fixed(result)[matcher]("ready")).rejects.toThrow(`${PROOF_TIMEOUT_MS}ms`);
  }
});

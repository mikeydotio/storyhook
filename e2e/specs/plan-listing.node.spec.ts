import { expect, test } from "./support";
import { PLAN_LISTING_FLAG, planListingPlaceholder } from "../plan-listing";

/**
 * Executed control for `e2e/plan-listing.ts` (SH-792). `tests/e2e_selection.rs`
 * can only confirm the runner scopes the flag to its plan listing and that
 * `requiredEnv` consults this module; this runs the policy itself. No `page`
 * fixture: plain function calls, so this file never launches a browser.
 */

const LISTING_ARGV = ["node", "playwright", "test", "--project=chromium", "--list"];
const RUN_ARGV = ["node", "playwright", "test", "--project=chromium"];

test.describe("plan-listing placeholder mode", () => {
  test("without the flag there is no placeholder, listing or not", () => {
    for (const argv of [LISTING_ARGV, RUN_ARGV]) {
      expect(planListingPlaceholder("DASHBOARD_TOKEN", {}, argv)).toBeUndefined();
      expect(planListingPlaceholder("DASHBOARD_TOKEN", { [PLAN_LISTING_FLAG]: "0" }, argv)).toBeUndefined();
    }
  });

  test("the plan listing gets a placeholder that names the variable", () => {
    expect(planListingPlaceholder("DASHBOARD_ALPHA_STORY_ID", { [PLAN_LISTING_FLAG]: "1" }, LISTING_ARGV)).toBe(
      "plan-listing-placeholder:DASHBOARD_ALPHA_STORY_ID",
    );
  });

  test("the flag outside a --list run is refused, never a placeholder", () => {
    expect(() => planListingPlaceholder("DASHBOARD_TOKEN", { [PLAN_LISTING_FLAG]: "1" }, RUN_ARGV)).toThrow(
      /only for scripts\/run-e2e\.sh's plan listing/,
    );
  });
});

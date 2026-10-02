import type { Page } from "@playwright/test";

/**
 * Keeps completing response-rewrite handlers inside their page lifetime, even
 * when the body fails. Use inside the owning page fixture or test body, before
 * context teardown can dispose fetched responses. The body must release any
 * deliberately held requests; this is not safe as a suite-wide automatic drain.
 */
export async function withDrainedRoutes<T>(page: Page, body: () => Promise<T>): Promise<T> {
  try {
    return await body();
  } finally {
    await page.unrouteAll({ behavior: "wait" });
  }
}

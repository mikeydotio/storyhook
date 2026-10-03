/**
 * How the browser suite's fixture administration reaches the daemon it runs
 * against: the environment `scripts/run-e2e.sh` exports, and the loopback URL
 * every fixture request is sent to.
 *
 * A module of its own, not part of `specs/support.ts`, because the run's
 * global setup (`fixture-baseline.ts`) needs both and runs in Playwright's
 * runner process, where the test-scoped fixtures `support.ts` declares have
 * no meaning. `support.ts` re-exports {@link requiredEnv} so specs keep one
 * import site.
 */

import { planListingPlaceholder } from "./plan-listing";
import { gracedOperationBudget } from "./load-grace";

/** Playwright APIRequestContext's default per-request patience, in milliseconds. */
export const BASE_REQUEST_TIMEOUT_MS = 30_000;

/** Sample the fixture API's patience at request entry; an explicit ratio
 * supports deterministic policy tests without changing the machine's load. */
export function gracedRequestBudget(ratio?: number): number {
  return gracedOperationBudget(BASE_REQUEST_TIMEOUT_MS, ratio);
}

/**
 * An environment variable this suite cannot run without. Throws rather than
 * defaulting, so a spec run outside `scripts/run-e2e.sh` fails loudly
 * instead of quietly hitting a dashboard with no fixtures and no token --
 * mirrors `playwright.config.ts`'s own `DASHBOARD_URL` check. The one
 * exception is the runner's plan listing, which loads every spec before any
 * fixture exists and runs none of them (`./plan-listing.ts`, SH-792).
 */
export function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    const placeholder = planListingPlaceholder(name);
    if (placeholder !== undefined) {
      return placeholder;
    }
    throw new Error(
      `${name} is not set — run this suite through scripts/run-e2e.sh, which starts an ` +
        "isolated daemon, seeds its fixtures, and exports the variables this file needs.",
    );
  }
  return value;
}

/**
 * The absolute URL of `path` on this run's daemon, always addressed through
 * loopback. `DASHBOARD_URL` is the browser's address, and for the
 * untrusted-origin project its host (`storyhook.e2e.test`) resolves only
 * inside the browser, through a host-resolver rule; Node's
 * `APIRequestContext` would fail to resolve it. The daemon always binds
 * loopback (`scripts/run-e2e.sh`), so fixture administration never depends
 * on the browser's host mapping.
 */
export function fixtureApiUrl(path: string): string {
  const url = new URL(path, requiredEnv("DASHBOARD_URL"));
  url.hostname = "127.0.0.1";
  return url.toString();
}

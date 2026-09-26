/**
 * The plan listing's placeholder mode (SH-792).
 *
 * `scripts/run-e2e.sh` lists the whole selection once, before any daemon or
 * seed exists, to cut it into slices that then run at the same time. A
 * `--list` run loads every selected spec module, and a dozen specs read
 * fixture values through `requiredEnv` at module load -- values nothing has
 * created yet. Under `E2E_PLAN_LISTING=1`, and only in a `--list` run,
 * `requiredEnv` hands out a placeholder instead. `--list` runs no test body
 * and no global setup, so no placeholder is ever used for anything.
 *
 * Anywhere else a placeholder is refused: a real run on made-up fixture
 * values would test nothing, and would say so only through whatever each
 * spec happened to do with a wrong story id. The runner scopes the flag to
 * the one listing command with `env`; `tests/e2e_selection.rs` pins that.
 */

/** The variable `scripts/run-e2e.sh` sets on its plan listing alone. */
export const PLAN_LISTING_FLAG = "E2E_PLAN_LISTING";

/**
 * A placeholder for the unset fixture variable `name`, or `undefined` when
 * this is not the plan listing. Throws when the flag is set outside a
 * `--list` run.
 */
export function planListingPlaceholder(
  name: string,
  env: Readonly<Record<string, string | undefined>> = process.env,
  argv: readonly string[] = process.argv,
): string | undefined {
  if (env[PLAN_LISTING_FLAG] !== "1") {
    return undefined;
  }
  if (!argv.includes("--list")) {
    throw new Error(
      `${PLAN_LISTING_FLAG}=1 is only for scripts/run-e2e.sh's plan listing, a --list run; ` +
        `refusing a placeholder for ${name} in a run that would execute tests.`,
    );
  }
  return `plan-listing-placeholder:${name}`;
}

import { execFile } from "node:child_process";
import { resolve } from "node:path";
import {
  BASE_TEST_TIMEOUT_MS,
  contention,
  gracedPatience,
  gracedTestBudget,
  loadGraceEnabled,
} from "./load-grace";

/** Pinned to the Python suite's case count and per-case bound by the Rust audit. */
export const REPORTER_CASE_COUNT = 4;
/** Existing nested-runner patience; the outer budget includes all four cases. */
export const REPORTER_CASE_TIMEOUT_MS = 60_000;
/** Python startup, result reporting and cleanup share the normal test allowance. */
const REPORTER_BASE_MS = REPORTER_CASE_COUNT * REPORTER_CASE_TIMEOUT_MS + BASE_TEST_TIMEOUT_MS;

/** The process deadline leaves room inside the enclosing Playwright test. */
export function reporterBudget(ratio = contention()): { testMs: number; processMs: number } {
  const testMs = loadGraceEnabled() ? gracedTestBudget(REPORTER_BASE_MS, ratio) : REPORTER_BASE_MS;
  const processMs = testMs - gracedPatience(ratio);
  if (!Number.isSafeInteger(processMs) || processMs < 1) {
    throw new Error(`invalid reporter process budget: ${processMs}`);
  }
  return { testMs, processMs };
}

/** Run only the reviewed reporter suite, waiting for cleanup before settling. */
export async function runReporterTests(boundMs: number, signal?: AbortSignal): Promise<string> {
  if (!Number.isSafeInteger(boundMs) || boundMs < 1) {
    throw new Error(`reporter bound must be a positive integer: ${boundMs}`);
  }
  signal?.throwIfAborted();
  return new Promise((resolveResult, reject) => {
    let failure: Error | null = null;
    let output = "";
    let errors = "";
    const child = execFile("python3", [
      resolve(__dirname, "../scripts/test-browser-launch-reporter.py"), "--watch-parent",
    ], { encoding: "utf8", timeout: boundMs, signal }, (error, stdout, stderr) => {
      failure = error;
      output = stdout;
      errors = stderr;
    });
    // Aborting can deliver the callback before Python has reaped its runner.
    // The closed pipes are the lifecycle receipt; callback delivery is not.
    child.once("close", (code, exitSignal) => {
      if (failure || child.killed || code !== 0) {
        reject(new Error(
          `reporter python3 test-browser-launch-reporter.py failed: bound=${boundMs}ms ` +
          `code=${code} signal=${exitSignal} killed=${child.killed}\n${output}\n${errors}`,
          { cause: failure },
        ));
      } else {
        resolveResult(errors);
      }
    });
  });
}

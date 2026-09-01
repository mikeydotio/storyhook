import { appendFileSync } from "node:fs";
import type { Reporter, TestCase, TestResult } from "@playwright/test/reporter";

/**
 * The Playwright half of the SH-524 gate progress journal.
 *
 * `scripts/run-e2e.sh` owns each project's "item" lifecycle (running →
 * passed/failed, with the total read from its own `--list` count) —
 * this reporter's only job is one "case" line per finished test, so the
 * two writers never race over the same event shape. `scripts/gate-
 * progress.sh` is the shell-side twin; both append the identical
 * `{"kind":"case","path":...,"outcome":"pass"|"fail"}` line.
 *
 * A no-op, silently, unless BOTH `STORYHOOK_GATE_PROGRESS` (the journal
 * path) and `STORYHOOK_GATE_PROGRESS_PATH` (which item this project's
 * cases nest under) are set — the same contract every shell emitter
 * follows, so an ordinary `npx playwright test` outside `run-e2e.sh`
 * behaves exactly as it does today.
 *
 * A "skipped" test is not reported, the same way `scripts/test-progress.
 * awk` never reports a Rust test cargo marks `ignored`: neither producer
 * states an opinion about a test that did not attempt to run.
 */
export default class GateProgressReporter implements Reporter {
  private readonly journal = process.env.STORYHOOK_GATE_PROGRESS;
  private readonly path = process.env.STORYHOOK_GATE_PROGRESS_PATH;

  onTestEnd(_test: TestCase, result: TestResult): void {
    if (!this.journal || !this.path) {
      return;
    }
    if (result.status === "skipped") {
      return;
    }
    const outcome = result.status === "passed" ? "pass" : "fail";
    const line = `${JSON.stringify({ kind: "case", path: this.path, outcome })}\n`;
    appendFileSync(this.journal, line);
  }
}

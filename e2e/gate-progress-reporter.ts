import { appendFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import type { Reporter, TestCase, TestResult } from "@playwright/test/reporter";

/**
 * The Playwright half of the SH-524 gate progress journal.
 *
 * `scripts/run-e2e.sh` owns each project's "item" lifecycle (running →
 * passed/failed, with the total read from its own `--list` count) —
 * this reporter records exact cases and runner execution boundaries, so
 * the two writers do not race over checklist state. `scripts/gate-
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
  private readonly execution = randomUUID();

  /** Mark the runner boundary after discovery, including fixtures and teardown. */
  onBegin(): void {
    this.cost("start");
  }

  /** Close a completed runner interval; a killed process leaves it unknown. */
  onEnd(): void {
    this.cost("end");
  }

  private cost(event: "start" | "end"): void {
    if (!this.journal || !this.path) return;
    appendFileSync(this.journal, `${JSON.stringify({
      kind: "cost", event, phase: "execution", id: this.execution,
      path: this.path, at: new Date().toISOString(),
      monotonic_ns: Number(process.hrtime.bigint()),
    })}\n`);
  }

  /** Retain literal identity for each completed case, including retries. */
  onTestEnd(test: TestCase, result: TestResult): void {
    if (!this.journal || !this.path) {
      return;
    }
    if (result.status === "skipped") {
      return;
    }
    const outcome = result.status === "passed" ? "pass" : "fail";
    const line = `${JSON.stringify({
      kind: "case", path: this.path, outcome,
      name: test.titlePath().join(" > "), target: test.location.file,
      identity: test.id, title_path: test.titlePath(),
    })}\n`;
    appendFileSync(this.journal, line);
  }
}

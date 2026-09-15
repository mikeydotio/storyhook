import type { Reporter, TestCase, TestResult } from "@playwright/test/reporter";

/** Stops repeated worker launches when a browser becomes unavailable mid-run. */
export default class BrowserLaunchReporter implements Reporter {
  private stopping = false;

  /** Preserve the failed result, then let Playwright interrupt and clean up its workers. */
  onTestEnd(test: TestCase, result: TestResult): void {
    if (this.stopping || !result.errors.some((error) =>
      /^(?:\w*Error:\s*)?browserType\.launch(?:PersistentContext)?:/.test(error.message ?? ""),
    )) return;

    this.stopping = true;
    console.error(
      `browser-launch: stopping project ${test.parent.project()?.name ?? "unknown"}: ` +
      "the browser could not start. The failed result and traces remain; " +
      "remaining tests are unrun. Restore the browser or graphical session before rerunning.",
    );
    // SIGINT uses the runner's normal interruption and worker-cleanup path.
    // Exiting here would discard reporters and strand browser descendants.
    process.kill(process.pid, "SIGINT");
  }
}

import { readFileSync, realpathSync, renameSync, writeFileSync } from "node:fs";
import { relative } from "node:path";
import type { FullConfig, FullResult, Reporter, Suite, TestCase, TestResult } from "@playwright/test/reporter";

/** One file's selection and observed execution, independent of progress events. */
interface FileObservation {
  /** Playwright project, including the browser configuration. */
  project: string;
  /** File relative to Playwright config.rootDir, matching --list and --test-list. */
  file: string;
  /** Tests discovered before execution. */
  count: number;
  /** Results delivered, including deliberate skips. */
  completed: number;
  /** Results that passed on their first attempt. */
  passed: number;
  /** Deliberately skipped tests, never timing observations. */
  skipped: number;
  /** Retried results, which disqualify the observation from history. */
  retries: number;
  /** Sum of Playwright's test durations in seconds. */
  seconds: number;
}

/** A completed receipt lets the shell detect even swallowed reporter failures. */
export default class SliceReporter implements Reporter {
  private readonly output = process.env.E2E_SLICE_REPORT;
  private root: string | undefined;
  private readonly expected = process.env.E2E_SLICE_EXPECTED;
  private readonly files = new Map<string, FileObservation>();
  private readonly errors: string[] = [];

  private identity(test: TestCase): [string, string, string] {
    const project = test.parent.project()?.name ?? "";
    const file = relative(this.root!, realpathSync(test.location.file)).split("\\").join("/");
    if (!project || !file || file.startsWith("../") || /[\t\r\n]/.test(project + file)) {
      throw new Error(`invalid slice identity: ${project}: ${file}`);
    }
    return [project, file, `${project}\t${file}`];
  }

  /** Compare real discovery with the planned file/count partition. */
  onBegin(config: FullConfig, suite: Suite): void {
    if (!this.output) return;
    try {
      this.root = realpathSync(config.rootDir);
      if (!this.expected) throw new Error("slice reporter requires expected manifest");
      for (const test of suite.allTests()) {
        const [project, file, key] = this.identity(test);
        const row = this.files.get(key) ?? { project, file, count: 0, completed: 0, passed: 0, skipped: 0, retries: 0, seconds: 0 };
        row.count++;
        this.files.set(key, row);
      }
      const actual = [...this.files.values()].map(row => `${row.project}\t${row.file}\t${row.count}`).sort();
      const expected = readFileSync(this.expected, "utf8").trim().split("\n").sort();
      if (!actual.length || JSON.stringify(actual) !== JSON.stringify(expected)) {
        throw new Error(`slice selection mismatch: expected ${JSON.stringify(expected)}, discovered ${JSON.stringify(actual)}`);
      }
    } catch (error) {
      this.errors.push(String(error));
      console.error(`slice-reporter: ${error}`);
    }
  }

  /** Record each attempt; retries and incomplete files cannot train the cache. */
  onTestEnd(test: TestCase, result: TestResult): void {
    if (!this.output) return;
    try {
      const row = this.files.get(this.identity(test)[2]);
      if (!row) throw new Error(`result absent from discovered selection: ${test.title}`);
      row.completed++;
      row.passed += result.status === "passed" ? 1 : 0;
      row.skipped += result.status === "skipped" ? 1 : 0;
      row.retries += result.retry > 0 ? 1 : 0;
      row.seconds += result.duration / 1000;
    } catch (error) {
      this.errors.push(String(error));
      console.error(`slice-reporter: ${error}`);
    }
  }

  /** Atomically publish completion; a missing receipt is independently refused. */
  onEnd(result: FullResult): { status?: FullResult["status"] } | void {
    if (!this.output) return;
    try {
      const status = this.errors.length ? "failed" : result.status;
      const temporary = `${this.output}.${process.pid}.tmp`;
      writeFileSync(temporary, JSON.stringify({ version: 1, status,
        observed: Date.now() / 1000, errors: this.errors, files: [...this.files.values()] }) + "\n");
      renameSync(temporary, this.output);
      if (this.errors.length) return { status: "failed" };
    } catch (error) {
      console.error(`slice-reporter: cannot publish ${this.output}: ${error}`);
      return { status: "failed" };
    }
  }

  /** Retain Playwright's ordinary human-readable reporter. */
  printsToStdio(): boolean { return false; }
}

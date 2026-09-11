import type { FullConfig, FullProject } from "@playwright/test";
import * as playwright from "@playwright/test";

/**
 * The browser tier's launch probe (SH-627): before a project's first test is
 * handed to a worker, launch the project's own engine once and close it.
 *
 * Why this exists. Playwright's `browser` fixture is worker-scoped and
 * launches with no fixture timeout of its own, so a launch is bounded only by
 * `DEFAULT_PLAYWRIGHT_LAUNCH_TIMEOUT` (3 * 60 * 1000 ms in playwright-core
 * 1.63). Under `workers: 1` a worker that cannot launch its browser fails its
 * test at that bound and Playwright starts a fresh worker for the NEXT test,
 * which pays it again: 45 desktop-webkit tests x 180 s is two and a quarter
 * hours of a gate reporting 45 "tree" failures for one dead browser. That is
 * exactly what happened after this machine's WindowServer crashed mid-release
 * on 2026-09-09 -- headless WebKit depends on the WindowServer/GPU XPC
 * service graph, and every `webkit.launch()` hung until a re-login -- and the
 * verdict was misread, twice, as a regression in the tree (`story show
 * SH-627`). A gate whose verdict depends on state it never checked is the
 * SH-306 shape; this is the check.
 *
 * Why a global setup rather than a Playwright setup project with
 * `dependencies`: `scripts/run-e2e.sh` derives its project loop from every
 * `name:` line in the config and gives each project its own daemon, seed and
 * fake-tmux state, so a setup project would be looped as a standalone project
 * and would need exclusions in the runner AND in the config's own contract
 * tests. Why per project rather than every engine the config names: on the
 * night in question Chromium stayed healthy after the crash and its 463/463
 * runs were real evidence about the tree; probing WebKit on a Chromium run
 * would have refused that evidence along with the broken engine's.
 *
 * Two things are deliberately NOT here. No `timeout:` is passed to `launch`,
 * so the probe's ceiling is Playwright's own launch default rather than a
 * number chosen here (SH-394: a deadline derives from the bound it disproves;
 * `tests/e2e_load_grace.rs` scans this directory for bare literals). And no
 * way to skip it, because a probe that can be switched off is a gate that
 * reads as run when it did not (SH-306).
 */

/**
 * The engine a project's tests will actually launch, resolved the way
 * Playwright's own fixtures resolve it (`browserName` fixture in
 * `playwright/lib/index.js`: `use.browserName`, else the
 * `defaultBrowserType` a `devices[...]` descriptor spreads in, else
 * chromium). Read from the resolved project rather than from a hand-kept
 * project-to-engine map, so a project added to the config is probed with no
 * edit here (SH-136).
 */
export function engineFor(project: FullProject): "chromium" | "firefox" | "webkit" {
  const use = project.use as {
    browserName?: "chromium" | "firefox" | "webkit";
    defaultBrowserType?: "chromium" | "firefox" | "webkit";
  };
  return use.browserName ?? use.defaultBrowserType ?? "chromium";
}

/** The project `scripts/run-e2e.sh` selected, by the name it exported beside `--project=`. */
function selectedProject(config: FullConfig): FullProject {
  const name = process.env.E2E_PROJECT;
  if (!name) {
    throw new Error(
      "launch-probe: E2E_PROJECT is not set -- run this suite through scripts/run-e2e.sh, " +
        "which exports the project it is about to run so the probe can launch that " +
        "project's own engine (SH-627). A probe that guessed the engine would be a " +
        "check of a browser the run may never use.",
    );
  }
  const project = config.projects.find((candidate) => candidate.name === name);
  if (!project) {
    throw new Error(
      `launch-probe: E2E_PROJECT names "${name}", which e2e/playwright.config.ts does not ` +
        `define (it defines: ${config.projects.map((candidate) => candidate.name).join(", ")}).`,
    );
  }
  return project;
}

export default async function launchProbe(config: FullConfig): Promise<void> {
  const project = selectedProject(config);
  const engine = engineFor(project);
  // Mirror the fixture's `_browserOptions`: the project's own launch options
  // (the untrusted-origin project carries `--host-resolver-rules`), its
  // headless and channel choices, and nothing else -- above all no timeout.
  const use = project.use as {
    launchOptions?: Record<string, unknown>;
    headless?: boolean;
    channel?: string;
  };
  const options: Record<string, unknown> = { handleSIGINT: false, ...use.launchOptions };
  if (use.headless !== undefined) options.headless = use.headless;
  if (use.channel !== undefined) options.channel = use.channel;

  const started = Date.now();
  try {
    const browser = await playwright[engine].launch(options);
    await browser.close();
  } catch (cause) {
    const message = cause instanceof Error ? cause.message : String(cause);
    throw new Error(
      `launch-probe: ${engine} could not launch for project ${project.name} ` +
        `(${Date.now() - started}ms). NO TEST RAN. This is the machine, not the tree: a ` +
        `browser that cannot start would otherwise fail every test in this project at ` +
        `Playwright's launch timeout, one worker at a time, and read as that many tree ` +
        `failures (SH-627: a WindowServer crash did exactly this to WebKit). If the browser ` +
        `is installed (make e2e-install), check the machine's WindowServer/GPU services -- ` +
        `a re-login or reboot restored WebKit last time -- then re-run. Playwright said: ` +
        message,
    );
  }
  // One line, always, so a probe nobody can see is never mistaken for a probe
  // that did not run -- the same posture as load-grace's own announcement.
  console.error(`launch-probe: ${engine} launched for project ${project.name} in ${Date.now() - started}ms`);
}

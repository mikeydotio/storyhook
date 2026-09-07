import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import type { Page } from "@playwright/test";
import { test, expect } from "./support";
import { requiredEnv } from "./support";

const STORY_BINARY = resolve("../target/debug/story");
const REPO_ROOT = resolve("..");
const NAMED_TOKEN = requiredEnv("DASHBOARD_NAMED_TOKEN");
const COOKIE_NAME = requiredEnv("DASHBOARD_COOKIE_NAME");

/** The stable evidence that an authenticated page finished its first catalog read. */
async function expectAuthenticatedHome(page: Page): Promise<void> {
  await expect(
    page.locator(".repo-card-name", { hasText: "Alpha Project" }),
  ).toBeVisible();
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
}

/**
 * SH-321's browser leg for SH-319: Chromium talks to the loopback socket under
 * a real non-trustworthy HTTP hostname, with no credential installed by the
 * fixture. The token reaches the browser only through the dashboard's own
 * modal exchange, and the resulting host-only cookie must carry every later
 * read across page, tab, and daemon lifetime boundaries.
 */
test("a named-token cookie survives reload, a new tab, and daemon restart on an untrusted origin", async ({
  page,
  context,
}) => {
  await page.goto("/");

  expect(
    await page.evaluate(() => ({
      hostname: window.location.hostname,
      secure: window.isSecureContext,
    })),
  ).toEqual({ hostname: "storyhook.e2e.test", secure: false });
  await expect(page.locator("#token-modal")).toHaveClass(/open/);

  const exchange = page.waitForResponse(
    (response) => new URL(response.url()).pathname === "/token" && response.status() === 204,
  );
  const catalog = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/repos" && response.status() === 200,
  );
  await page.locator("#token-input").fill(NAMED_TOKEN);
  await page.locator("#token-submit").click();
  await exchange;
  const catalogResponse = await catalog;
  await expectAuthenticatedHome(page);

  const catalogHeaders = await catalogResponse.request().allHeaders();
  expect(catalogHeaders["x-storyhook"]).toBe("1");
  expect(catalogHeaders["sec-fetch-site"]).toBeUndefined();

  const cookie = (await context.cookies()).find((candidate) => candidate.name === COOKIE_NAME);
  expect(cookie).toMatchObject({
    name: COOKIE_NAME,
    value: NAMED_TOKEN,
    domain: "storyhook.e2e.test",
    path: "/",
    httpOnly: true,
    secure: false,
    sameSite: "Strict",
  });
  expect(await page.evaluate(() => document.cookie)).not.toContain(COOKIE_NAME);

  // Chromium does not retry the EventSource whose credential-free first
  // attempt received 401. The required reload creates a fresh native stream
  // carrying the exchanged cookie, so this is the honest observation point
  // for SH-319's Referer fallback rather than a timer-based retry assumption.
  const reloadedEvents = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/events" && response.status() === 200,
  );
  await page.reload();
  await expectAuthenticatedHome(page);
  const eventsResponse = await reloadedEvents;
  const eventHeaders = await eventsResponse.request().allHeaders();
  expect(eventHeaders["x-storyhook"]).toBeUndefined();
  expect(eventHeaders["sec-fetch-site"]).toBeUndefined();
  expect(eventHeaders.referer).toBe(`${new URL(eventsResponse.url()).origin}/`);

  const secondPage = await context.newPage();
  await secondPage.goto("/");
  await expectAuthenticatedHome(secondPage);

  await Promise.all([page.close(), secondPage.close()]);
  const commandOptions = {
    cwd: REPO_ROOT,
    encoding: "utf8" as const,
    timeout: test.info().timeout,
  };
  const stopped = execFileSync(STORY_BINARY, ["daemon", "stop"], commandOptions);
  const started = execFileSync(STORY_BINARY, ["daemon", "start"], commandOptions);
  const stoppedPid = stopped.match(/PID (\d+)/)?.[1];
  const startedPid = started.match(/PID (\d+)/)?.[1];
  expect(stoppedPid, stopped).toBeTruthy();
  expect(startedPid, started).toBeTruthy();
  expect(startedPid).not.toBe(stoppedPid);

  const restartedPage = await context.newPage();
  await restartedPage.goto("/");
  await expectAuthenticatedHome(restartedPage);
});

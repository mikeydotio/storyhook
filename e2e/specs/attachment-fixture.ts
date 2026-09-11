import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { Page } from "@playwright/test";
import { expect, requiredEnv, storyBinary, test } from "./support";

/** Exercises real CLI storage, dashboard login, and a headerless image request. */
export async function expectCookieAttachment(page: Page): Promise<void> {
  const storyId = requiredEnv("DASHBOARD_ALPHA_STORY_ID");
  const path = test.info().outputPath("pixel.png");
  mkdirSync(dirname(path), { recursive: true });
  // A complete 1×1 PNG: the browser must decode it, not merely accept a signature.
  const bytes = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aC1sAAAAASUVORK5CYII=", "base64");
  writeFileSync(path, bytes);
  const command = (args: string[]): string => execFileSync(storyBinary(), args, {
    cwd: requiredEnv("DASHBOARD_ALPHA_CHECKOUT"),
    encoding: "utf8",
    timeout: test.info().timeout,
  });
  const added = JSON.parse(command(["attachment", "add", storyId, path, "--json"]));
  const attachments: Array<{ id: number }> = added.story.story.attachments;
  const attachmentId = attachments[attachments.length - 1].id;
  try {
    await page.goto("/");
    await expect(page.locator("#token-modal")).toHaveClass(/open/);
    await page.locator("#token-input").fill(requiredEnv("DASHBOARD_NAMED_TOKEN"));
    const exchange = page.waitForResponse((response) => new URL(response.url()).pathname === "/token" && response.status() === 204);
    const catalog = page.waitForResponse((response) => new URL(response.url()).pathname === "/api/repos" && response.status() === 200);
    await page.locator("#token-submit").click();
    await exchange;
    const repos: Array<{ id: string; name: string }> = await (await catalog).json();
    const repo = repos.find((candidate) => candidate.name === "Alpha Project");
    expect(repo).toBeDefined();
    const url = `/api/repos/${repo!.id}/story/${storyId}/attachments/${attachmentId}`;
    await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
    const imageResponse = page.waitForResponse((response) => new URL(response.url()).pathname === url);
    const dimensions = await page.evaluate(async (src) => {
      const image = document.createElement("img");
      image.src = src;
      document.body.append(image);
      await image.decode();
      return { width: image.naturalWidth, height: image.naturalHeight };
    }, url);
    expect(dimensions).toEqual({ width: 1, height: 1 });
    const response = await imageResponse;
    expect(response.status()).toBe(200);
    expect(await response.body()).toEqual(bytes);
    expect(response.headers()["cross-origin-resource-policy"]).toBe("same-origin");
    expect(response.headers()["cache-control"]).toBe("no-store");
    const headers = await response.request().allHeaders();
    expect(headers["x-storyhook"]).toBeUndefined();
    expect(headers["x-storyhook-token"]).toBeUndefined();
    expect(headers.cookie).toContain(`${requiredEnv("DASHBOARD_COOKIE_NAME")}=`);
    expect(headers.referer).toBe(`${new URL(requiredEnv("DASHBOARD_URL")).origin}/`);
    if (!(await page.evaluate(() => window.isSecureContext))) {
      expect(headers["sec-fetch-site"]).toBeUndefined();
    }
  } finally {
    command(["attachment", "remove", storyId, String(attachmentId)]);
  }
}

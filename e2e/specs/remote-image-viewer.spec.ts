import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import type { Page } from "@playwright/test";
import {
  test, expect, seedToken, openProject, createStory, cleanUpCreatedStories,
  requiredEnv, latch,
} from "./support";

cleanUpCreatedStories("Alpha Project");

const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aC1sAAAAASUVORK5CYII=",
  "base64",
);

/** Mutates only this harness's isolated store through production CLI paths. */
function command(args: string[]): string {
  return execFileSync(resolve("../target/debug/story"), args, {
    cwd: requiredEnv("DASHBOARD_ALPHA_CHECKOUT"), encoding: "utf8",
    timeout: test.info().timeout,
  });
}

function setDescription(id: string, description: string): void {
  command(["set", id, "--description", description]);
}

function addImage(id: string, name: string): void {
  const path = test.info().outputPath("stored.png");
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, PNG);
  command(["attachment", "add", id, path, "--name", name]);
}

async function openStory(page: Page, id: string): Promise<void> {
  await page.locator(`.card[data-id="${id}"]`).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
}

async function decoded(page: Page): Promise<void> {
  const image = page.locator("#attachment-image img");
  await expect(image).toBeVisible();
  await expect.poll(() => image.evaluate(
    (node: HTMLImageElement) => node.complete && node.naturalWidth > 0,
  )).toBe(true);
  await expect(page.locator("#attachment-status")).toHaveText("");
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("remote images require activation, omit referrer, and follow stored attachments", async ({ page }) => {
  const id = await createStory(page, "Consent-gated remote image");
  addImage(id, "stored.png");
  const source = "https://images.example.test/screens/My%20Diagram.PNG?size=2#private";

  let requests = 0;
  let referer: string | undefined;
  await page.route("https://images.example.test/**", async route => {
    requests++;
    referer = (await route.request().allHeaders()).referer;
    await route.fulfill({ status: 200, contentType: "image/png", body: PNG });
  });

  await openStory(page, id);
  setDescription(id, `Inspect ${source}`);
  const buttons = page.locator(".attachment-thumbnail");
  await expect(buttons).toHaveCount(2);
  await expect(buttons.nth(0)).toHaveAttribute("data-media-kind", "stored");
  const remote = buttons.nth(1);
  await expect(remote).toHaveAttribute("data-media-kind", "remote");
  await expect(remote.locator(".attachment-name")).toHaveText("My Diagram.PNG");
  await expect(remote.locator(".remote-image-host")).toHaveText("images.example.test");
  await expect(remote.locator("img")).not.toHaveAttribute("src", /.+/);
  expect(requests).toBe(0);

  await remote.click();
  await decoded(page);
  expect(requests).toBeGreaterThan(0);
  expect(referer).toBeUndefined();
  await expect(page.locator("#attachment-title")).toHaveText("My Diagram.PNG");
  await expect(remote.locator("img")).toHaveAttribute(
    "src", "https://images.example.test/screens/My%20Diagram.PNG?size=2",
  );
});

test("description grammar collects supported links once and excludes unsafe contexts", async ({ page }) => {
  const id = await createStory(page, "Remote image grammar");
  await openStory(page, id);
  setDescription(id, [
    "https://assets.example.test/one.png?raw=1#first",
    "<https://assets.example.test/two.JpG>",
    "[linked](https://assets.example.test/three.gif)",
    "![diagram](https://assets.example.test/four%20wide.webp)",
    "https://assets.example.test/one.png?raw=1#duplicate",
    "`https://assets.example.test/code.png`",
    "```",
    "https://assets.example.test/fenced.jpeg",
    "```",
    "http://assets.example.test/insecure.png",
    "https://assets.example.test/vector.svg",
    "https://user:secret@assets.example.test/credentialed.png",
  ].join("\n"));

  const remote = page.locator('[data-media-kind="remote"]');
  await expect(remote).toHaveCount(4);
  await expect(remote.locator(".attachment-name")).toHaveText([
    "one.png", "two.JpG", "three.gif", "four wide.webp",
  ]);
  for (const image of await remote.locator("img").all()) {
    await expect(image).not.toHaveAttribute("src", /.+/);
  }
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await openStory(page, id);
  await expect(page.locator(".description-view")).toContainText(
    "![diagram](https://assets.example.test/four%20wide.webp)",
  );
});

test("live URL replacement invalidates delayed loads and supports explicit retry", async ({ page }) => {
  const id = await createStory(page, "Remote image lifecycle");
  const oldUrl = "https://remote.example.test/old.png";
  const newUrl = "https://remote.example.test/new.png";

  const oldReceived = latch();
  const releaseOld = latch();
  await page.route(oldUrl, async route => {
    oldReceived.release();
    await releaseOld.held;
    await route.fulfill({ status: 200, contentType: "image/png", body: PNG });
  });
  let newAttempts = 0;
  await page.route(newUrl, async route => {
    newAttempts++;
    if (newAttempts === 1) {
      await route.fulfill({ status: 503, body: "not ready" });
    } else {
      await route.fulfill({ status: 200, contentType: "image/png", body: PNG });
    }
  });

  try {
    await openStory(page, id);
    setDescription(id, oldUrl);
    await page.locator('[data-media-kind="remote"]').click();
    await oldReceived.held;
    await expect(page.locator("#attachment-status")).toHaveText("Loading image…");

    setDescription(id, newUrl);
    await expect(page.locator("#attachment-modal")).not.toHaveClass(/open/);
    const replacement = page.locator('[data-media-kind="remote"]');
    await expect(replacement).toHaveAttribute("title", newUrl);
    releaseOld.release();
    await page.unroute(oldUrl, { behavior: "wait" });
    await expect(page.locator("#attachment-modal")).not.toHaveClass(/open/);

    await replacement.click();
    await expect(page.locator("#attachment-status")).toContainText("Could not load new.png");
    await page.locator("#attachment-close").click();
    await replacement.click();
    await decoded(page);

    setDescription(id, "No remote image remains.");
    await expect(page.locator("#attachment-modal")).not.toHaveClass(/open/);
    await expect(page.locator('[data-media-kind="remote"]')).toHaveCount(0);
    await expect(page.locator("#drawer")).toBeFocused();
  } finally {
    releaseOld.release();
    await page.unrouteAll({ behavior: "wait" });
  }
});

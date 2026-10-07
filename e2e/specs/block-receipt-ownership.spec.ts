import { isBoardPage } from "./board-network";
import { test, expect } from "./support";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  heldReadDeadlineMs,
  holdFetch,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/** Opens a real card and waits for its detail response to settle. */
async function openStory(page: Page, id: string): Promise<void> {
  const finished = page.waitForEvent("requestfinished", {
    predicate: (request) => request.method() === "GET"
      && new URL(request.url()).pathname.endsWith(`/story/${id}`),
  });
  await page.locator(`.card[data-id="${id}"]`).click();
  await finished;
  await expect(page.locator("#drawer-id")).toHaveText(id);
}

/** Expands the production comments section without assuming saved UI preferences. */
async function showComments(page: Page): Promise<void> {
  const toggle = page.locator("#drawer-body .section-toggle", { hasText: "Comments" });
  if (await toggle.getAttribute("aria-expanded") !== "true") await toggle.click();
}

cleanUpCreatedStories("Alpha Project");

test("block receipts stay with their story across navigation, late detail, and board refresh (SH-823)", async ({
  page,
  request,
}) => {
  await seedToken(page);
  await page.goto(`/?apiGetTimeoutMs=${heldReadDeadlineMs()}&boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
  const first = await createStory(page, "SH-823 receipt owner");
  const second = await createStory(page, "SH-823 unrelated receipt owner");
  const empty = await createStory(page, "SH-823 story without receipts");
  const slug = await projectSlug(request, "Alpha Project");
  const base = `/api/repos/${encodeURIComponent(slug)}/story/`;
  const headers = {
    "X-Storyhook": "1",
    "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
  };
  const firstReceipt = `AGENT BLOCK DELIVERY #107 — interrupt delivered — ${first}\n\n> Evidence mentions SH-768.`;
  const secondReceipt = `AGENT BLOCK DELIVERY #108 — resume unreached — ${second}\n\n> No registered session.`;
  // The worker is exercised by Rust tests; these receipt-shaped fixture comments
  // ask what the real UI does with data, including a diagnostic naming SH-768.
  for (const [id, text] of [[first, firstReceipt], [second, secondReceipt]]) {
    const response = await request.post(`${base}${id}/comment`, { headers, data: { text } });
    expect(response.ok(), await response.text()).toBe(true);
  }
  const comments = page.locator("#drawer-body .comments .comment-text");
  const assertOwner = async (id: string, receipt: string | null) => {
    await expect(page.locator("#drawer-id")).toHaveText(id);
    await showComments(page);
    await expect(comments).toHaveCount(receipt ? 1 : 0);
    if (receipt) await expect(comments).toContainText(receipt);
  };
  await openStory(page, first);
  await assertOwner(first, `#107 — interrupt delivered — ${first}`);
  await page.locator("#drawer-close").click();

  // Hold first's genuine detail response until second owns the drawer. The
  // production request guard must reject it without changing second's comments.
  const heldDetail = await holdFetch(page,
    (url) => url.pathname === `${base}${first}`, () => true);
  await page.locator(`.card[data-id="${first}"]`).click();
  await heldDetail.taken;
  await page.locator("#drawer-close").click();
  await openStory(page, second);
  await assertOwner(second, `#108 — resume unreached — ${second}`);
  await heldDetail.deliverCanceled();
  await assertOwner(second, `#108 — resume unreached — ${second}`);

  // A real write to another story causes SSE and a genuine board refresh while
  // second remains open. Delaying that reply makes the tested boundary explicit.
  const refreshedTitle = "SH-823 unrelated board refresh";
  const heldBoard = await holdFetch<{ stories: { story: { id: string; title: string } }[] }>(
    page,
    (url) => isBoardPage(url, slug, "todo"),
    (body) => body.stories.some((view) => view.story.id === first && view.story.title === refreshedTitle),
  );
  const changed = await request.patch(`${base}${first}`, { headers, data: { title: refreshedTitle } });
  expect(changed.ok(), await changed.text()).toBe(true);
  await heldBoard.taken;
  await heldBoard.deliver();
  await expect(page.locator(`.card[data-id="${first}"] .card-title`)).toHaveText(refreshedTitle);
  await assertOwner(second, `#108 — resume unreached — ${second}`);

  await page.locator("#drawer-close").click();
  await openStory(page, empty);
  await assertOwner(empty, null);
  await page.locator("#drawer-close").click();
  await openStory(page, first);
  await assertOwner(first, `#107 — interrupt delivered — ${first}`);

  for (const [id, expected] of [[first, [firstReceipt]], [second, [secondReceipt]], [empty, []]] as const) {
    const response = await request.get(`${base}${id}`, { headers });
    expect(response.ok()).toBe(true);
    const body = await response.json();
    expect((body.story.story.comments || []).map((comment: { text: string }) => comment.text)).toEqual(expected);
  }
});

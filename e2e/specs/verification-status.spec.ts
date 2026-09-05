import type { Page } from "@playwright/test";
import { test, expect } from "./support";
import {
  onAFrozenClock,
  openProject,
  projectSlug,
  seedToken,
} from "./support";

/**
 * SH-549's browser contract. The route clones real story views so the test
 * replaces only verifier status; the Rust API tests own the wire producer.
 */

const RUNNING_TITLE = "SH-549 active low priority";
const QUEUED_TITLE = "SH-549 queued high priority";
const STARTING_TITLE = "SH-549 active starting";
const MOVED_TITLE = "SH-549 moved out of verifying";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

type Verification =
  | { status: "queued"; wait_seconds: number; position: number }
  | {
      status: "running";
      elapsed_seconds: number;
      current_step?: { label: string; elapsed_seconds: number };
      tests?: { completed: number; total: number };
    };

async function injectVerificationCards(page: Page, slug: string): Promise<void> {
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    async (route) => {
      const response = await route.fetch();
      const data: { stories?: Array<Record<string, unknown>> } = await response.json();
      const template = (data.stories ?? [])[0];
      if (!template) throw new Error("verification status fixture has no story to clone");

      const cards: Array<{
        id: string;
        title: string;
        priority: string;
        state?: string;
        verification: Verification;
      }> = [
        {
          id: "SH-94901",
          title: RUNNING_TITLE,
          priority: "low",
          verification: {
            status: "running",
            elapsed_seconds: 724,
            current_step: { label: "rust-suite", elapsed_seconds: 182 },
            tests: { completed: 2234, total: 2250 },
          },
        },
        {
          id: "SH-94902",
          title: QUEUED_TITLE,
          priority: "high",
          verification: { status: "queued", wait_seconds: 3840, position: 1 },
        },
        {
          id: "SH-94903",
          title: STARTING_TITLE,
          priority: "medium",
          verification: { status: "running", elapsed_seconds: 3 },
        },
        {
          id: "SH-94904",
          title: MOVED_TITLE,
          priority: "medium",
          state: "todo",
          verification: { status: "queued", wait_seconds: 10, position: 2 },
        },
      ];

      for (const candidate of cards) {
        const clone = JSON.parse(JSON.stringify(template)) as {
          story: Record<string, unknown>;
          display_state?: string | null;
          is_ready?: boolean;
          is_blocked?: boolean;
          verification?: Verification;
        };
        clone.story.id = candidate.id;
        clone.story.title = candidate.title;
        clone.story.state = candidate.state ?? "verifying";
        clone.story.superstate = "OPEN";
        clone.story.priority = candidate.priority;
        clone.display_state = null;
        clone.is_ready = false;
        clone.is_blocked = false;
        clone.verification = candidate.verification;
        (data.stories ??= []).push(clone);
      }
      await route.fulfill({ response, json: data });
    },
  );
}

function card(page: Page, title: string) {
  return page.locator('.column[data-state="verifying"] .card', { hasText: title });
}

test("cards distinguish active ownership from priority-sorted waiting work", async ({
  page,
  request,
}) => {
  const slug = await projectSlug(request, "Alpha Project");
  await injectVerificationCards(page, slug);
  await openProject(page, "Alpha Project");

  const running = card(page, RUNNING_TITLE);
  const queued = card(page, QUEUED_TITLE);
  await expect(running.locator(".verification-chip")).toHaveText(
    "Verifying · 12m 4s total · rust suite 3m 2s · 2234/2250 tests (99.3%)",
  );
  await expect(queued.locator(".verification-chip")).toHaveText(
    "Queued · 1h 4m · position 1",
  );
  await expect(running).toHaveAttribute(
    "aria-label",
    new RegExp("Verifying · 12m 4s total.*2234/2250 tests \\(99\\.3%\\)"),
  );
  await expect(queued).toHaveAttribute("aria-label", /Queued · 1h 4m · position 1/);
  const moved = page.locator('.column[data-state="todo"] .card', { hasText: MOVED_TITLE });
  await expect(moved.locator(".verification-chip")).toHaveCount(0);
  await expect(moved).not.toHaveAttribute("aria-label", /Queued/);
});

test("starting is explicit and elapsed running values advance on the shared timer", async ({
  page,
  request,
}) => {
  const slug = await projectSlug(request, "Alpha Project");
  await injectVerificationCards(page, slug);

  await onAFrozenClock(page, async () => {
    await openProject(page, "Alpha Project");
    await expect(card(page, STARTING_TITLE).locator(".verification-chip")).toHaveText(
      "Verifying · starting…",
    );
    const chip = card(page, RUNNING_TITLE).locator(".verification-chip");
    await expect(chip).toContainText("12m 4s total · rust suite 3m 2s");
    await page.clock.runFor(1000);
    await expect(chip).toContainText("12m 5s total · rust suite 3m 3s");
    await expect(card(page, RUNNING_TITLE)).toHaveAttribute(
      "aria-label",
      /12m 5s total · rust suite 3m 3s/,
    );
  });
});

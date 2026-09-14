import type { Page } from "@playwright/test";
import { test, expect } from "./support";
import {
  onAFrozenClock,
  openProject,
  projectSlug,
  seedToken,
} from "./support";

/**
 * SH-549's browser contract, extended for SH-603 supersession. The route
 * clones real story views so the test replaces only verifier status; the Rust
 * API tests own the wire producer.
 */

const RUNNING_TITLE = "SH-549 active low priority";
const ACTIVITY_TITLE = "SH-589 active lock wait";
const QUEUED_TITLE = "SH-549 queued high priority";
const STARTING_TITLE = "SH-549 active starting";
const RESUBMITTED_TITLE = "SH-603 resubmitted generation";
const MOVED_TITLE = "SH-549 moved out of verifying";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

type Verification =
  | { status: "held"; blockers: string[] }
  | { status: "landingpending" }
  | { status: "queued"; wait_seconds: number; position: number }
  | {
      status: "superseding";
      generation: number;
      superseded_generation: number;
      wait_seconds: number;
      active_elapsed_seconds: number;
    }
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
        { id: "SH-94907", title: "SH-656 held", priority: "high", verification: { status: "held", blockers: ["SH-11", "SH-12"] } },
        { id: "SH-94908", title: "SH-656 landing pending", priority: "high", verification: { status: "landingpending" } },
        {
          id: "SH-94906",
          title: RESUBMITTED_TITLE,
          priority: "high",
          verification: {
            status: "superseding",
            generation: 123,
            superseded_generation: 120,
            wait_seconds: 30,
            active_elapsed_seconds: 420,
          },
        },
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
          id: "SH-94905",
          title: ACTIVITY_TITLE,
          priority: "low",
          verification: {
            status: "running",
            elapsed_seconds: 1204,
            current_step: { label: "waiting for gate lock", elapsed_seconds: 496 },
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
  const activity = card(page, ACTIVITY_TITLE);
  await expect(activity.locator(".verification-chip")).toHaveText(
    "Verifying · 20m 4s total · waiting for gate lock 8m 16s",
  );
  await expect(activity.locator(".verification-chip")).not.toContainText("tests");
  await expect(activity).toHaveAttribute(
    "aria-label",
    /Verifying · 20m 4s total · waiting for gate lock 8m 16s$/,
  );
  await expect(queued).toHaveAttribute("aria-label", /Queued · 1h 4m · position 1/);
  const resubmitted = card(page, RESUBMITTED_TITLE);
  await expect(resubmitted.locator(".verification-chip")).toHaveText(
    /^Resubmitted · generation 123 reserved · generation 120 still running · 7m \d+s elapsed · \d+s waiting$/,
  );
  await expect(resubmitted.locator(".verification-chip")).toHaveClass(
    /verification-chip-superseding/,
  );
  await expect(resubmitted).toHaveAttribute(
    "aria-label",
    /Resubmitted · generation 123 reserved · generation 120 still running · 7m \d+s elapsed · \d+s waiting$/,
  );
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


test("held dependencies and pending landings stay visible without queue positions", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  await injectVerificationCards(page, slug);
  await openProject(page, "Alpha Project");
  const held = card(page, "SH-656 held");
  await expect(held.locator(".verification-chip")).toHaveText("Verification held · open blockers: SH-11, SH-12");
  await expect(held).toHaveAttribute("aria-label", /Verification held · open blockers: SH-11, SH-12/);
  await expect(card(page, "SH-656 landing pending").locator(".verification-chip")).toHaveText("Landing pending · confirming merge outcome");
  await expect(card(page, QUEUED_TITLE).locator(".verification-chip")).toContainText("position 1");
});

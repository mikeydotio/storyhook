import { test, expect } from "./support";
import type { Locator, Page } from "@playwright/test";
import { cleanUpCreatedStories, deleteStory, openProject, seedToken } from "./support";

/**
 * Exercises SH-217's markdown renderer (`renderMarkdown` in
 * src/web_dashboard.html): a hand-written, dependency-free GFM-lite
 * subset that builds DOM nodes directly, never an HTML string -- the
 * dashboard's CSP is `script-src 'unsafe-inline'`, so a string-to-markup
 * sink would be a real vulnerability the moment untrusted story text
 * reached it (tests/web_test.rs pins the absence of one).
 *
 * The renderer has no exported seam (the whole dashboard script is one
 * IIFE with nothing on `window`), so this spec drives it through comment
 * bodies -- a read-only surface with no focus/edit machinery in the way,
 * which renders through the exact same `renderMarkdown()` call the
 * description's read view uses (see description-edit-mode.spec.ts for
 * that interaction). Reading `.innerHTML` in a *test* is fine here; only
 * *writing* one in production code is what the CSP posture forbids.
 *
 * Creates and deletes its own stories rather than touching the "Alpha
 * Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function createStory(page: Page, title: string): Promise<Locator> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await expect(card).toBeVisible();
  return card;
}

async function openStory(page: Page, card: Locator): Promise<void> {
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
}

async function closeDrawer(page: Page): Promise<void> {
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}

/** Posts one comment through the drawer's own comment form and waits for
 * it to render -- newest-first, so it lands as the FIRST `.comment-text`.
 *
 * The wait is load-bearing, not decoration: `.comment-text` at position 0
 * is trivially "visible" the instant ANY earlier comment exists, so a bare
 * `toBeVisible()` can pass against stale content posted before this call
 * even landed -- a real race, not a hypothetical one (it produced a
 * flaky 5s timeout against `**bold text**`'s own `<strong>` locator,
 * chasing a comment that was never going to acquire one). The textarea
 * only clears inside the success handler, after the drawer has already
 * been rebuilt with the new comment (buildCommentsSection's submit
 * handler: `.then(function(r) { textarea.value = ""; handleMutationSuccess(r); })`,
 * both synchronous within one callback) -- so waiting for it to empty is
 * a reliable proxy for "the new comment now exists in the DOM," unlike
 * waiting on the list's own visibility. */
async function postComment(page: Page, text: string): Promise<Locator> {
  const textarea = page.locator('textarea[placeholder="Add a comment…"]');
  await textarea.fill(text);
  await page.locator("#drawer-body .comment-add button").click();
  await expect(textarea).toHaveValue("");
  const rendered = page.locator(".comment-text").first();
  await expect(rendered).toBeVisible();
  return rendered;
}

test("every block and inline construct renders to the expected DOM", async ({ page }) => {
  const title = "SH-217 markdown — block and inline constructs";
  const card = await createStory(page, title);
  await openStory(page, card);

  const cases: Array<{ md: string; check: (c: Locator) => Promise<void> }> = [
    { md: "# Heading one", check: async (c) => expect(c.locator("h1")).toHaveText("Heading one") },
    { md: "###### Heading six", check: async (c) => expect(c.locator("h6")).toHaveText("Heading six") },
    { md: "## Trailing hashes ##", check: async (c) => expect(c.locator("h2")).toHaveText("Trailing hashes") },
    {
      md: "no space after hash: ##nope",
      check: async (c) => {
        await expect(c.locator("h1, h2")).toHaveCount(0);
      },
    },
    {
      md: "line one\nline two",
      check: async (c) => expect(c).toHaveText("line one line two"),
    },
    {
      md: "line one  \nline two",
      check: async (c) => {
        await expect(c.locator("br")).toHaveCount(1);
      },
    },
    {
      md: "para one\n\npara two",
      check: async (c) => expect(c.locator("p")).toHaveCount(2),
    },
    { md: "---", check: async (c) => expect(c.locator("hr")).toHaveCount(1) },
    { md: "* * *", check: async (c) => expect(c.locator("hr")).toHaveCount(1) },
    {
      md: "**bold text**",
      check: async (c) => expect(c.locator("strong")).toHaveText("bold text"),
    },
    { md: "*italic text*", check: async (c) => expect(c.locator("em")).toHaveText("italic text") },
    { md: "_also italic_", check: async (c) => expect(c.locator("em")).toHaveText("also italic") },
    { md: "~~gone~~", check: async (c) => expect(c.locator("del")).toHaveText("gone") },
    {
      md: "snake_case_name stays literal",
      check: async (c) => {
        await expect(c.locator("em")).toHaveCount(0);
        await expect(c).toContainText("snake_case_name");
      },
    },
    {
      md: "*a **nested bold** inside italic*",
      check: async (c) => {
        await expect(c.locator("em")).toHaveCount(1);
        await expect(c.locator("em strong")).toHaveText("nested bold");
      },
    },
    { md: "`inline code`", check: async (c) => expect(c.locator("code")).toHaveText("inline code") },
    {
      md: "`**not bold**`",
      check: async (c) => {
        await expect(c.locator("code")).toHaveText("**not bold**");
        await expect(c.locator("strong")).toHaveCount(0);
      },
    },
    {
      md: "```\nvar x = 1;\n```",
      check: async (c) => expect(c.locator("pre code")).toHaveText("var x = 1;"),
    },
    {
      md: "```\n**not bold**\n```",
      check: async (c) => {
        await expect(c.locator("pre code")).toHaveText("**not bold**");
        await expect(c.locator("strong")).toHaveCount(0);
      },
    },
    {
      md: "~~~\ntilde fence\n~~~",
      check: async (c) => expect(c.locator("pre code")).toHaveText("tilde fence"),
    },
    {
      md: "```\nunclosed fence line one\nunclosed fence line two",
      check: async (c) =>
        expect(c.locator("pre code")).toHaveText(
          "unclosed fence line one\nunclosed fence line two",
        ),
    },
    {
      // Not indented-code as the comment's ONLY content: the existing
      // submit handler does `textarea.value.trim()` (buildCommentsSection,
      // pre-dating this story), which strips leading whitespace from the
      // very start of the string -- an indented block starting a comment
      // would lose its indent before the renderer ever saw it. A leading
      // paragraph (unaffected by trim, since it isn't at a string edge)
      // sidesteps that and still exercises the construct.
      md: "before\n\n    indented code line",
      check: async (c) => expect(c.locator("pre code")).toHaveText("indented code line"),
    },
    {
      md: "> a quoted line",
      check: async (c) => expect(c.locator("blockquote")).toContainText("a quoted line"),
    },
    {
      md: "- one\n- two\n- three",
      check: async (c) => expect(c.locator("ul li")).toHaveCount(3),
    },
    {
      md: "1. first\n2. second",
      check: async (c) => {
        await expect(c.locator("ol li")).toHaveCount(2);
      },
    },
    {
      md: "3. starts at three\n4. continues",
      check: async (c) => expect(c.locator("ol")).toHaveAttribute("start", "3"),
    },
    {
      md: "- one\n\n- two",
      check: async (c) => expect(c.locator("ul > li > p")).toHaveCount(2),
    },
    {
      md: "- parent\n  - nested child",
      check: async (c) => expect(c.locator("ul li ul li")).toHaveText("nested child"),
    },
    {
      md: "- [ ] todo item\n- [x] done item",
      check: async (c) => {
        await expect(c.locator("li.task")).toHaveCount(2);
        await expect(c.locator("li.task input[type=checkbox]:checked")).toHaveCount(1);
      },
    },
    {
      md: "| a | b |\n| - | - |\n| 1 | 2 |",
      check: async (c) => {
        await expect(c.locator("table th")).toHaveCount(2);
        await expect(c.locator("table td")).toHaveCount(2);
      },
    },
    {
      md: "| left | center | right |\n|:--|:-:|--:|\n| 1 | 2 | 3 |",
      check: async (c) => {
        await expect(c.locator('table th[align="center"]')).toHaveCount(1);
        await expect(c.locator('table td[align="right"]')).toHaveText("3");
      },
    },
    {
      md: "\\*escaped asterisks\\*",
      check: async (c) => {
        await expect(c).toContainText("*escaped asterisks*");
        await expect(c.locator("em")).toHaveCount(0);
      },
    },
    {
      md: "a <b>bold</b> tag stays literal",
      check: async (c) => {
        await expect(c.locator("b")).toHaveCount(0);
        await expect(c).toContainText("<b>bold</b>");
      },
    },
    {
      md: "<script>alert(1)</script>",
      check: async (c) => {
        const scriptCount = await page.evaluate(
          () => document.querySelectorAll("script").length,
        );
        expect(scriptCount).toBe(1); // only the dashboard's own <script>
        await expect(c).toContainText("<script>alert(1)</script>");
      },
    },
    {
      md: "![alt text](https://example.com/x.png)",
      check: async (c) => {
        await expect(c.locator("img")).toHaveCount(0);
        await expect(c).toContainText("![alt text](https://example.com/x.png)");
      },
    },
  ];

  // Post and check each case in turn, always against `.comment-text` at
  // position 0 -- comments render newest-first, so the case just posted
  // is always the first one, checked before the next post pushes it down.
  // Interleaving (rather than batching all posts, then all checks) also
  // attributes a failure to the exact case that produced it. See
  // postComment()'s own doc comment for why the textarea-empty wait is
  // load-bearing here, not decoration.
  for (const { md, check } of cases) {
    const rendered = await postComment(page, md);
    await check(rendered);
  }

  await closeDrawer(page);
  await deleteStory(page, title);
});

test("unsafe link schemes render as text, never as a link", async ({ page }) => {
  const title = "SH-217 markdown — unsafe link schemes";
  const card = await createStory(page, title);
  await openStory(page, card);

  const unsafe = [
    "[click](javascript:alert(1))",
    "[click](JaVaScRiPt:alert(1))",
    "[data](data:text/html,evil)",
    "[vb](vbscript:msgbox(1))",
    "[relative](/relative/path)",
    "[protocol-relative](//evil.example.com)",
  ];

  for (const md of unsafe) {
    const rendered = await postComment(page, md);
    await expect(rendered.locator("a")).toHaveCount(0);
  }

  await closeDrawer(page);
  await deleteStory(page, title);
});

test("rendered links open in a new tab and drop the referrer", async ({ page }) => {
  const title = "SH-217 markdown — safe links";
  const card = await createStory(page, title);
  await openStory(page, card);

  const cases = [
    "[https link](https://example.com/page)",
    "[mailto link](mailto:person@example.com)",
    "see <https://example.com/auto> here",
    "visit https://example.com/bare directly",
  ];

  for (const md of cases) {
    const rendered = await postComment(page, md);
    const link = rendered.locator("a").first();
    await expect(link).toHaveAttribute("target", "_blank");
    await expect(link).toHaveAttribute("rel", "noopener noreferrer");
    await expect(link).toHaveAttribute("href", /^(https:|mailto:)/);
  }

  await closeDrawer(page);
  await deleteStory(page, title);
});

test("story ids inside rendered markdown become links, and stay literal inside code", async ({
  page,
}) => {
  const source = "SH-217 markdown — story id linkification source";
  const target = "SH-217 markdown — story id linkification target";
  const sourceCard = await createStory(page, source);
  const targetCard = await createStory(page, target);
  const targetId = (await targetCard.getAttribute("data-id"))!;

  await openStory(page, sourceCard);
  const sourceId = (await page.locator("#drawer-id").textContent())!;
  const unknownId = `${sourceId.split("-")[0]}-999999`;

  const linkedCases = [
    `plain paragraph mentioning ${targetId} here`,
    `**bold mentioning ${targetId}**`,
    `- a list item naming ${targetId}`,
    `| id |\n| - |\n| ${targetId} |`,
  ];
  for (const md of linkedCases) {
    const rendered = await postComment(page, md);
    await expect(rendered.locator(".rel-id")).toHaveText(targetId);
  }

  const literalCases: Array<{ md: string; expectId: string }> = [
    { md: `inline code stays literal: \`${targetId}\``, expectId: targetId },
    { md: `fenced code stays literal:\n\`\`\`\n${targetId}\n\`\`\``, expectId: targetId },
    { md: `self-reference stays plain: ${sourceId}`, expectId: sourceId },
    { md: `unresolvable id stays plain: ${unknownId}`, expectId: unknownId },
  ];
  for (const { md, expectId } of literalCases) {
    const rendered = await postComment(page, md);
    await expect(rendered.locator(".rel-id")).toHaveCount(0);
    await expect(rendered).toContainText(expectId);
  }

  await closeDrawer(page);
  await deleteStory(page, source);
  await deleteStory(page, target);
});

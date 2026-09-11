import { test, expect } from "./support";
import { openProject, seedToken } from "./support";

/**
 * SH-622's fence, executed. `support.ts` exports an `expect` whose
 * `toHaveText`/`toContainText` refuse an assertion that rides an
 * `aria-hidden` glyph -- text that `textContent` carries and the accessible
 * name excludes. SH-620 put a decorative emoji inside every control that
 * has one, and four specs asserting a control's own words through
 * `toHaveText` read the decoration too. This spec is the only thing that
 * proves the door does what its doc says; `tests/e2e_text_assertion_door.rs`
 * proves only that every spec walks through it.
 *
 * The subject throughout is the static project selector,
 * `#projsel-btn` (`src/web_dashboard.html`): a `.projsel-label` span holding
 * the words, then an `aria-hidden` caret span holding "🔽". On Home the
 * label reads "All projects", so the button's `textContent` is the two,
 * with the markup's own whitespace around each, and its accessible name is
 * "All projects".
 *
 * Every refusal here is captured with `rejects.toThrow`, which is NOT one of
 * the two matchers the door shadows, so the harness judging the door is not
 * itself the thing under test.
 */

const SELECTOR = "#projsel-btn";
const CARET = "🔽";
const LABEL = "All projects";

/** What the door tells a spec that asserted decoration. Its wording is the
 * contract a reader acts on, so the spec pins the load-bearing words: the
 * hidden text it found, and the remedy. */
const REFUSAL = /aria-hidden/;
const REMEDY = /toHaveAccessibleName/;
/** The two verdicts a refusal reports about Playwright's own comparison, so
 * a reader knows whether the spec had encoded the glyph or tripped on it. */
const WOULD_HAVE_PASSED = /would have PASSED/;
const HAD_ALSO_FAILED = /had also failed/;

/** A deadline for the delegation-fidelity cases. Short, because the point is
 * to measure the door's own overhead against it -- see `DELEGATION_CEILING_MS`. */
const FIDELITY_TIMEOUT_MS = 2_000;

/** A `.not` delegated in the wrong direction (polling until the text
 * MATCHES, as the documented pass-flip idiom would) spends the whole
 * `FIDELITY_TIMEOUT_MS` before it can pass. A correctly delegated one returns
 * as soon as the first poll disagrees. Half the deadline separates the two
 * with room for a loaded machine, and derives from the deadline it disproves
 * rather than stating an opinion about how fast a poll should look (SH-394). */
const DELEGATION_CEILING_MS = FIDELITY_TIMEOUT_MS / 2;

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await expect(page.locator(SELECTOR)).toBeVisible();
});

test("toHaveText is refused on a control carrying a hidden glyph, even when the text would match", async ({
  page,
}) => {
  // The base assertion would PASS -- this is exactly the glyph-bearing
  // text textContent holds (a pattern, because Playwright does not
  // whitespace-normalise the received text against a RegExp, and the markup
  // puts newlines around both spans). Refusing it anyway is what separates
  // the door from a relabelled failure: a passing assertion has encoded
  // decoration -- and the refusal says so in as many words.
  const attempt = expect(page.locator(SELECTOR)).toHaveText(
    new RegExp(`^\\s*${LABEL}\\s*${CARET}\\s*$`),
  );
  await expect(attempt).rejects.toThrow(REFUSAL);
  await expect(attempt).rejects.toThrow(REMEDY);
  await expect(attempt).rejects.toThrow(CARET);
  await expect(attempt).rejects.toThrow(WOULD_HAVE_PASSED);
});

test("toHaveText is refused on the same control when the text would not match", async ({
  page,
}) => {
  // SH-622's own symptom: `toHaveText("Columns (1)")` against
  // "Columns (1)🔽". The reader gets the door's diagnosis, not a bare
  // "expected/received" that hides why the two differ.
  const attempt = expect(page.locator(SELECTOR)).toHaveText(LABEL, {
    timeout: FIDELITY_TIMEOUT_MS,
  });
  await expect(attempt).rejects.toThrow(REFUSAL);
  await expect(attempt).rejects.toThrow(REMEDY);
  await expect(attempt).rejects.toThrow(HAD_ALSO_FAILED);
});

test("not.toHaveText is refused on a control carrying a hidden glyph", async ({
  page,
}) => {
  const attempt = expect(page.locator(SELECTOR)).not.toHaveText("anything else", {
    timeout: FIDELITY_TIMEOUT_MS,
  });
  await expect(attempt).rejects.toThrow(REFUSAL);
});

test("the label element, the glyph element, and the accessible name are all legitimate subjects", async ({
  page,
}) => {
  // The words, asserted on the element that holds only the words.
  await expect(page.locator("#projsel-label")).toHaveText(LABEL);
  // The glyph, asserted on the glyph -- "only a spec whose SUBJECT is the
  // glyph asserts text". It has no hidden descendants of its own.
  await expect(page.locator(`${SELECTOR} .emoji-icon`)).toHaveText(CARET);
  // The remedy the refusal names has to be real.
  await expect(page.locator(SELECTOR)).toHaveAccessibleName(LABEL);
});

test("toContainText is refused only when the expectation names hidden text", async ({
  page,
}) => {
  // A substring claim that never mentions the glyph does not ride it: this
  // is the idiom sixteen existing `#projsel-btn` sites already use.
  await expect(page.locator(SELECTOR)).toContainText(LABEL);
  await expect(page.locator(SELECTOR)).toContainText(/All projects/);

  await expect(
    expect(page.locator(SELECTOR)).toContainText(CARET),
  ).rejects.toThrow(REFUSAL);
  await expect(
    expect(page.locator(SELECTOR)).toContainText(new RegExp(CARET)),
  ).rejects.toThrow(REFUSAL);
  await expect(
    expect(page.locator(SELECTOR)).toContainText(LABEL + CARET, {
      timeout: FIDELITY_TIMEOUT_MS,
    }),
  ).rejects.toThrow(REFUSAL);
});

test("a glyph-free subject reaches Playwright's own matcher unchanged", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  const count = page.locator("#filter-count");
  await expect(count).toHaveText(/^\d+ \/ \d+$/);
  await expect(count).toContainText("/");

  // A failure is Playwright's own failure, message and all -- the door
  // neither rewrites nor swallows it.
  const failure = expect(count).toHaveText("nope", {
    timeout: FIDELITY_TIMEOUT_MS,
  });
  await expect(failure).rejects.toThrow(/toHaveText/);
  await expect(failure).rejects.toThrow(/nope/);
  await expect(failure).rejects.not.toThrow(REFUSAL);

  // A custom message travels with it.
  await expect(
    expect(count, "the custom message survives the door").toHaveText("nope", {
      timeout: FIDELITY_TIMEOUT_MS,
    }),
  ).rejects.toThrow(/the custom message survives the door/);
});

test("not.toHaveText is delegated in the negative direction, not flipped afterwards", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  const count = page.locator("#filter-count");
  const started = Date.now();
  await expect(count).not.toHaveText("nope", { timeout: FIDELITY_TIMEOUT_MS });
  const elapsed = Date.now() - started;
  expect(
    elapsed,
    `a \`.not\` that passed only after ${elapsed}ms spent its deadline polling for a match it never wanted`,
  ).toBeLessThan(DELEGATION_CEILING_MS);

  // And the negative direction still fails when the text IS there.
  const shouldFail = expect(count).not.toHaveText(/\d+ \/ \d+/, {
    timeout: FIDELITY_TIMEOUT_MS,
  });
  await expect(shouldFail).rejects.toThrow(/toHaveText/);
  await expect(shouldFail).rejects.not.toThrow(REFUSAL);
});

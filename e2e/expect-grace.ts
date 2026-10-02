import type { Expect, TestInfo } from "@playwright/test";
import { BASE_EXPECT_TIMEOUT_MS, gracedPatience } from "./load-grace";

/**
 * Refreshes the default at assertion construction (SH-813). Explicit matcher
 * and poll options still win inside Playwright. Derived instances retain
 * dynamic sampling unless configure explicitly supplies a timeout; undefined
 * restores the dynamic default. No assertion is restarted after a failure.
 */
export function withAssertionGrace<Matchers>(
  target: Expect<Matchers>,
  patience: () => number = gracedPatience,
  report: (budgetMs: number) => void = () => {},
): Expect<Matchers> {
  function wrap<M>(source: Expect<M>, fixed = false): Expect<M> {
    const sampled = () => {
      if (fixed) return source;
      const budget = patience();
      report(budget);
      return source.configure({ timeout: budget });
    };
    // Proxy the callable, not its matchers: Playwright retains types, chaining,
    // asymmetric helpers, errors and the independent toPass timeout policy.
    return new Proxy(source, {
      apply(_target, _thisArg, args) {
        return Reflect.apply(sampled(), undefined, args);
      },
      get(_target, key, receiver) {
        if (key === "soft") return wrap(source.soft, fixed);
        if (key === "poll") return (...args: Parameters<typeof source.poll>) => sampled().poll(...args);
        if (key === "configure") {
          return (options: Parameters<typeof source.configure>[0]) => wrap(
            source.configure(options),
            "timeout" in options ? options.timeout !== undefined : fixed,
          );
        }
        if (key === "extend") {
          return (...args: Parameters<typeof source.extend>) => wrap(source.extend(...args), fixed);
        }
        return Reflect.get(source, key, receiver);
      },
    });
  }
  return wrap(target);
}

type AnnotationOwner = Pick<TestInfo, "annotations">;
const reportedBudgets = new WeakMap<AnnotationOwner, Set<number>>();

/** Reports each non-idle default budget once per test. */
export function reportAssertionGrace(
  owner: AnnotationOwner,
  budgetMs: number,
  write: (line: string) => void = (line) => process.stderr.write(line),
): void {
  if (budgetMs <= BASE_EXPECT_TIMEOUT_MS) return;
  let seen = reportedBudgets.get(owner);
  if (!seen) {
    seen = new Set();
    reportedBudgets.set(owner, seen);
  }
  if (seen.has(budgetMs)) return;
  const line = `load-grace: assertion default=${budgetMs}ms (base=${BASE_EXPECT_TIMEOUT_MS}ms; explicit options override)`;
  write(`${line}\n`);
  owner.annotations.push({ type: "load-grace", description: line });
  seen.add(budgetMs);
}

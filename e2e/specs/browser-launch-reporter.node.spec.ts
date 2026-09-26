import { execFile } from "node:child_process";
import { resolve } from "node:path";
import { promisify } from "node:util";
import { expect, test } from "./support";

test("browser launch failure interrupts the project while ordinary failures continue", async () => {
  const result = await promisify(execFile)("python3", [
    resolve(__dirname, "../../scripts/test-browser-launch-reporter.py"),
  ]);
  expect(result.stderr).toContain("OK");
});

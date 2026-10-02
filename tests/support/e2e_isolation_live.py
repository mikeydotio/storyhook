"""Exercise the production runner with a deliberately order-dependent fixture.

Run explicitly after building and installing the locked e2e toolchain. This
starts real isolated daemons and real Playwright; the ordinary Rust gate runs
the lightweight contracts instead. No existing spec or product behavior is mocked.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def main():
    """Prove the predecessor masks a failure, then prove isolation exposes it twice."""
    with tempfile.TemporaryDirectory(prefix="isolation-proof-", dir=ROOT / "e2e/specs") as specs, \
            tempfile.TemporaryDirectory(prefix="isolation-proof-results-", dir="/tmp") as output:
        directory = Path(specs)
        prefix = directory.name
        common = '''import { test, expect } from "@playwright/test";
import { existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";
const checkout = process.env.DASHBOARD_ALPHA_CHECKOUT!;
const marker = () => join(checkout, "isolation-proof-marker");
'''
        (directory / "a.node.spec.ts").write_text(common + '''
test("predecessor changes its seeded fixture", async ({ request }) => {
  expect((await request.get(process.env.DASHBOARD_URL!)).status()).toBe(200);
  writeFileSync(marker(), "predecessor");
});
''')
        (directory / "b.node.spec.ts").write_text(common + '''
test("order-dependent assertion", async ({ request }) => {
  expect((await request.get(process.env.DASHBOARD_URL!)).status()).toBe(200);
  console.log("ISOLATION_FIXTURE=" + checkout);
  expect(existsSync(marker())).toBe(true);
});
''')
        selection = Path(output) / "both.list"
        selection.write_text(f"[node] › {prefix}/a.node.spec.ts\n[node] › {prefix}/b.node.spec.ts\n")
        environment = dict(os.environ, STORYHOOK_E2E_JOBS="2")
        for mode, args, expected in (
                ("shared", ["--test-list=" + str(selection)], 0),
                ("isolated", ["--isolate-files", prefix], 1)):
            environment["STORYHOOK_E2E_RESULTS_DIR"] = str(Path(output) / mode)
            with (Path(output) / (mode + ".log")).open("w") as log:
                result = subprocess.run(["bash", "scripts/run-e2e.sh", "--project=node", *args],
                                        cwd=ROOT, env=environment, stdout=log, stderr=log)
            if result.returncode != expected:
                raise AssertionError((Path(output) / (mode + ".log")).read_text())
        root = Path(output) / "isolated"
        if not (root / "isolation.json").is_file():
            raise AssertionError((Path(output) / "isolated.log").read_text())
        report = json.loads((root / "isolation.json").read_text())
        assert report["selected_files"] == 2 and report["selected_tests"] == 2, report
        assert len(report["failures"]) == 1, report
        failure = report["failures"][0]
        assert failure["file"] == f"{prefix}/b.node.spec.ts", failure
        assert failure["outcome"] == "failed again", failure
        initial = (root / "logs" / (failure["slice"] + ".log")).read_text()
        rerun = (root / "reruns/logs" / (failure["slice"] + ".log")).read_text()
        fixtures = [next(line.split("ISOLATION_FIXTURE=", 1)[1] for line in text.splitlines()
                         if "ISOLATION_FIXTURE=" in line) for text in (initial, rerun)]
        assert fixtures[0] != fixtures[1], fixtures
        print("PASS: predecessor masks the failure; isolation exposes exactly one file; "
              "the serial repeat uses a fresh fixture and preserves the failure")


if __name__ == "__main__":
    main()

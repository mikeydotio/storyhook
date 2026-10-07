"""SH-870: causal-return metadata refresh never starts a gate or mutates Git."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from load_grace import contention, patience

SCRIPT = Path(__file__).resolve().parents[1] / "verify-pr.sh"


class AttributionHead(unittest.TestCase):
    """Exercise the production shell entry with only its GitHub endpoint stubbed."""

    def test_metadata_only_and_endpoint_failure(self):
        """The exact read is forwarded; errors remain errors and no gate owner appears."""
        with tempfile.TemporaryDirectory(prefix="sh870-head-", dir="/tmp") as directory:
            root = Path(directory).resolve()
            env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null",
                       GIT_CONFIG_NOSYSTEM="1")
            limit = patience(30, contention())
            subprocess.run(["git", "init", "--quiet", "--template=", "-b", "fixture"],
                           cwd=root, env=env, check=True, timeout=limit)
            endpoint = root / "story-endpoint"
            endpoint.write_text("""#!/bin/bash
[ "$1" = github ] && [ "$2" = exec ] || exit 91
shift 2
[ "$1" = --checkout ] && [ "$2" = "$PWD" ] || exit 92
shift 2
[ "$1" = --authority ] && [ "$2" = "$PWD" ] || exit 93
shift 2
[ "$1" = -- ] || exit 94
shift
[ "$#" -eq 5 ] && [ "$1" = pr ] && [ "$2" = view ] || exit 95
[ "$3" = https://github.com/acme/widgets/pull/1 ] || exit 96
[ "$4" = --json ] && [ "$5" = state,isDraft,isCrossRepository,headRefOid ] || exit 97
if [ "$SH870_ENDPOINT_FAIL" = 1 ]; then printf '%s\\n' 'metadata unavailable' >&2; exit 17; fi
printf '%s\\n' '{"state":"OPEN","isDraft":false,"isCrossRepository":false,"headRefOid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}'
""")
            endpoint.chmod(0o700)
            env.update(STORY_BIN=str(endpoint), STORYHOOK_GITHUB_AUTHORITY=str(root))
            env.pop("STORYHOOK_GITHUB_EXPECTED", None)
            before = sorted(str(path.relative_to(root)) for path in (root / ".git").rglob("*"))
            for fail in (False, True):
                env["SH870_ENDPOINT_FAIL"] = "1" if fail else "0"
                result = subprocess.run(["bash", str(SCRIPT), "--diagnosis-head",
                                         "https://github.com/acme/widgets/pull/1"], cwd=root, env=env,
                                        text=True, capture_output=True, timeout=limit)
                self.assertEqual(result.returncode, 17 if fail else 0, result.stderr)
                if fail:
                    self.assertIn("metadata unavailable", result.stderr)
                    self.assertEqual(result.stdout, "")
                else:
                    self.assertEqual(json.loads(result.stdout)["headRefOid"], "a" * 40)
                self.assertEqual(before, sorted(str(path.relative_to(root)) for path in (root / ".git").rglob("*")))


if __name__ == "__main__":
    unittest.main()

"""Policy activation requires retained calibration, never repository overrides."""

import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
import contextlib
import io

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from host_admission.activation import load_policy
from host_admission.policy import Refusal
from test_host_admission import policy_value


class ActivationTests(unittest.TestCase):
    def test_status_is_disabled_without_creating_a_namespace(self):
        from host_admission.command import main
        root = self.root / "absent"
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            result = main(["status"], root=root)
        self.assertEqual(result, 0)
        self.assertFalse(json.loads(output.getvalue())["enabled"])
        self.assertFalse(root.exists())

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir="/tmp", prefix="ha-policy-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def write(self, name, value):
        path = self.root / name
        data = json.dumps(value, sort_keys=True).encode()
        path.write_bytes(data); path.chmod(0o600)
        return hashlib.sha256(data).hexdigest()

    def test_absent_and_fixture_policies_cannot_activate(self):
        with self.assertRaisesRegex(Refusal, "calibration"):
            load_policy(self.root, "fixture-host")
        self.write("policy.json", policy_value())
        with self.assertRaisesRegex(Refusal, "calibration"):
            load_policy(self.root, "fixture-host")

    def test_measured_policy_requires_matching_retained_bytes_and_parameters(self):
        p = policy_value(); p["calibration"] = "measured"
        parameters = {k: v for k, v in p.items() if k not in ("calibration", "measurements")}
        evidence = dict(version=1, host="fixture-host", parameters=parameters,
                        sources=["SH-801", "SH-867"], result="calibrated", observations=[dict(sample="retained")])
        digest = self.write("temporary.json", evidence)
        (self.root / "temporary.json").rename(self.root / f"calibration-{digest}.json")
        p["measurements"] = ["sha256:" + digest]
        self.write("policy.json", p)
        self.assertEqual(load_policy(self.root, "fixture-host").value, p)
        p["capacity"]["cpu"] += 1
        self.write("policy.json", p)
        with self.assertRaisesRegex(Refusal, "parameters"):
            load_policy(self.root, "fixture-host")
        (self.root / f"calibration-{digest}.json").write_text("{}")
        with self.assertRaisesRegex(Refusal, "digest"):
            load_policy(self.root, "fixture-host")

    def test_symlink_policy_is_refused(self):
        self.write("other.json", policy_value())
        (self.root / "policy.json").symlink_to(self.root / "other.json")
        with self.assertRaises(Refusal):
            load_policy(self.root, "fixture-host")


if __name__ == "__main__":
    unittest.main()

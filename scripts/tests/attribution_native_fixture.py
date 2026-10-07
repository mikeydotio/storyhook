"""Only fixture policy and endpoint differ from the production diagnostic driver."""
import functools
import importlib.util
import json
from pathlib import Path
import sys

from host_admit_fixture import AuthorityCase, MIB, fixture_policy


def main():
    """Own a real broker until stdin closes, or run the real embedded driver."""
    if sys.argv[1] == "serve":
        case = AuthorityCase()
        case.policy_changes = {"workloads": dict(fixture_policy()["workloads"],
                               **{"causal-rust": dict(cpu=4000, memory=400*MIB)})}
        try:
            case.setUp()
            path = Path(sys.argv[2])
            path.with_suffix(".pending").write_text(json.dumps(dict(root=str(case.root), policy=case.value)))
            path.with_suffix(".pending").replace(path)
            sys.stdin.read()
        finally:
            case.doCleanups()
        return
    config = json.loads(Path(sys.argv[1]).read_text())
    driver, request = sys.argv[2:]
    for name in list(sys.modules):
        if name == "host_admission" or name.startswith("host_admission."):
            del sys.modules[name]
    sys.path.insert(0, str(Path(driver).parent))
    from host_admission.policy import Policy
    spec = importlib.util.spec_from_file_location("native_driver", driver)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.diagnosis.run = functools.partial(module.diagnosis.run,
        root=Path(config["root"]), policy_loader=lambda _root, host: Policy(config["policy"], host, fixture=True))
    sys.argv = [driver, request]
    module.main()


if __name__ == "__main__":
    main()

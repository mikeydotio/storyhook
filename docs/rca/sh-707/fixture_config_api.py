"""Exercise native plugin-toggle RPCs only inside a newly created test fixture."""

import json
from pathlib import Path
import queue
import subprocess
import threading


def toggle(env, output, plugin_id, enabled, reject_stale=False):
    """Use the native UI's config/value/write API; never touch hook trust."""
    output = Path(output).resolve()
    # This helper has no live CLI entrypoint and accepts only rehearsal-owned homes.
    if (Path(env["HOME"]) != output / "home"
            or Path(env["CODEX_HOME"]) != output / "codex-home"
            or not (output / "commands.json").is_file()):
        raise ValueError("config API requires an initialized isolated rehearsal")
    if plugin_id not in ("greenlight@agentics", "greenlight@personal", "greenlight-sh707-age54@personal"):
        raise ValueError("fixture toggle only supports the reviewed Greenlight identities")
    records, incoming = [], queue.Queue()
    stderr_path = output / f"toggle-{plugin_id}-{enabled}.stderr"
    with stderr_path.open("x") as stderr:
        process = subprocess.Popen(["codex", "app-server", "--stdio"], env=env,
            cwd=output, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True)

        def consume():
            for line in process.stdout:
                incoming.put(line)

        reader = threading.Thread(target=consume, daemon=True)
        reader.start()

        def request(method, params):
            request_id = len(records) + 1
            payload = {"id": request_id, "method": method, "params": params}
            process.stdin.write(json.dumps(payload) + "\n")
            process.stdin.flush()
            while True:
                try:
                    response = json.loads(incoming.get(timeout=20))
                except queue.Empty as error:
                    raise ValueError(f"fixture RPC {method} timed out; see {stderr_path}") from error
                if response.get("id") == request_id:
                    records.append({"request": payload, "response": response})
                    return response

        try:
            initialized = request("initialize", {"clientInfo": {"name": "sh707-fixture", "version": "1"}})
            if "error" in initialized:
                raise ValueError(f"fixture initialize failed: {initialized}")
            process.stdin.write('{"method":"initialized"}\n')
            process.stdin.flush()
            read = request("config/read", {"includeLayers": True})
            if "error" in read:
                raise ValueError(f"fixture config read failed: {read}")
            layer = next(item for item in read["result"]["layers"] if item["name"]["type"] == "user")
            params = {"keyPath": f"plugins.{plugin_id}", "value": {"enabled": enabled},
                      "mergeStrategy": "upsert", "expectedVersion": layer["version"]}
            if reject_stale:
                before = (output / "codex-home/config.toml").read_bytes()
                refused = request("config/value/write", {**params, "expectedVersion": "sha256:" + "0" * 64})
                conflict = refused.get("error", {}).get("data", {}).get("config_write_error_code")
                if conflict != "configVersionConflict" or (output / "codex-home/config.toml").read_bytes() != before:
                    raise ValueError("stale-version write was not refused without mutation")
            written = request("config/value/write", params)
            if "error" in written:
                raise ValueError(f"fixture toggle failed: {written}")
            return records
        finally:
            (output / f"toggle-{plugin_id}-{enabled}.json").write_text(json.dumps(records, indent=2) + "\n")
            process.stdin.close()
            process.terminate()
            process.wait(timeout=10)
            reader.join(timeout=2)
            process.stdout.close()

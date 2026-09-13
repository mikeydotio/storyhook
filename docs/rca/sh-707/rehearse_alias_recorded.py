"""Rehearse a separate development identity while preserving every old cache."""

from pathlib import Path
import json
import os
import subprocess
import sys
import tomllib

worktree = Path('/Volumes/Code/mikeyward/storyhook/.codex/worktrees/SH-707')
sys.path.insert(0, str(worktree / 'docs/rca/sh-707'))
from rehearse_development import rehearse
from stage_alias import stage_alias
from packaging_evidence import inventory
from installed_contract import exercise
from fixture_config_api import toggle

old = Path('/Users/mikey/Enderchest/storyhook/sh707-development-20260913')
new = Path('/Users/mikey/Enderchest/storyhook/sh707-development-20260913-age54-isolated')
root = Path('/private/tmp/sh707-alias-rehearsal-age54')
repo = Path('/Volumes/Code/mikeyward/agentics/.codex/worktrees/AGE-104')
rehearse(repo, old, root, preserve_caches=True)
env = {'PATH': os.environ['PATH'], 'HOME': str(root / 'home'),
       'CODEX_HOME': str(root / 'codex-home'), 'TMPDIR': '/tmp', 'LC_ALL': 'C',
       'PYTHONDONTWRITEBYTECODE': '1'}
config = root / 'codex-home/config.toml'
before = tomllib.loads(config.read_text())
cache = root / 'codex-home/plugins/cache'
cache_before = inventory(cache)
old_source = inventory(root / 'home/plugins/greenlight')
receipt = json.loads((new / 'provenance.json').read_text())
staged = stage_alias(new, root / 'home', receipt['artifact_sha256'])
assert tomllib.loads(config.read_text()) == before
assert inventory(root / 'home/plugins/greenlight') == old_source
assert inventory(cache) == cache_before
result = subprocess.run(['codex', 'plugin', 'add', staged['plugin_id'], '--json'],
    env=env, cwd=root, capture_output=True, text=True, timeout=90)
(root / 'alias-install.json').write_text(result.stdout)
(root / 'alias-install.stderr').write_text(result.stderr)
assert result.returncode == 0, result.stderr
installed = json.loads(result.stdout)
assert installed['pluginId'] == staged['plugin_id']
assert installed['name'] == receipt['plugin_name']
assert installed['version'] == receipt['metadata_delta']['version']
installed_path = Path(installed['installedPath'])
assert inventory(installed_path) == receipt['metadata_delta']['files']
expected = json.loads(json.dumps(before))
expected['plugins'][staged['plugin_id']] = {'enabled': True}
assert tomllib.loads(config.read_text()) == expected
cache_after = inventory(cache)
assert all(cache_after.get(path) == value for path, value in cache_before.items())
prefix = str(installed_path.relative_to(cache)) + '/'
assert all(path.startswith(prefix) for path in set(cache_after) - set(cache_before))
contracts = exercise(installed_path, repaired=True)
suite = subprocess.run(['bats', str(installed_path / 'tests/greenlight-policy.bats'),
    str(installed_path / 'tests/greenlight-redirection.bats')], env=env, cwd=root,
    capture_output=True, text=True, timeout=180)
(root / 'alias-explorer-contracts.log').write_text(suite.stdout + suite.stderr)
assert suite.returncode == 0, suite.stdout + suite.stderr
assert inventory(installed_path) == receipt['metadata_delta']['files']
toggles = toggle(env, root, staged['plugin_id'], False, reject_stale=True)
expected['plugins'][staged['plugin_id']]['enabled'] = False
assert tomllib.loads(config.read_text()) == expected
assert inventory(cache) == cache_after
report = {'kind': 'separate SH-707 development identity rehearsal',
    'source_commit': receipt['source']['commit'], 'plugin_tree': receipt['source']['plugin_tree'],
    'plugin_id': staged['plugin_id'], 'version': installed['version'],
    'artifact_sha256': receipt['artifact_sha256'], 'staging': staged,
    'all_previous_cache_files_preserved': len(cache_before),
    'old_staged_source_preserved': True, 'existing_config_and_synthetic_trust_preserved': True,
    'installer_enables_new_candidate': True, 'rollback_disabled_new_candidate': True,
    'compatibility_contracts_passed': len(contracts), 'explorer_contracts': suite.stdout,
    'toggle_rpc_transcript': toggles, 'native_host_validated': False,
    'release_certified': False, 'live_changes_performed': False}
(root / 'alias-receipt.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps({key: value for key, value in report.items()
                  if key not in ('staging', 'toggle_rpc_transcript')}, indent=2), flush=True)

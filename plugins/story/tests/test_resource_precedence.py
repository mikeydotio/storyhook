"""A broken caller server cannot preempt the lease selected by native inventory."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
PLUGIN = Path(__file__).resolve().parents[1]


class ResourcePrecedenceTests(unittest.TestCase):
    """Both explicit and discovered marker leases own their server selection."""

    def test_native_lease_selection_precedes_caller_ownership_probe(self):
        with tempfile.TemporaryDirectory(prefix='sh825-precedence-', dir='/tmp') as root:
            root = Path(root).resolve()
            caller = str(root / 'caller')
            owner = str(root / 'owner')
            import sys
            sys.path.insert(0, str(PLUGIN / 'lib'))
            from tmux_target import activation_path
            env = dict(os.environ, HOME=str(root), TMUX=caller + ',0,0', STORY_PLUGIN_ROOT=str(PLUGIN))
            env.pop('XDG_STATE_HOME', None)
            path = activation_path(caller, env)
            path.parent.mkdir(parents=True)
            path.write_text('{invalid activation')
            path.chmod(0o600)
            report = root / 'report.json'
            report.write_text(json.dumps(dict(resources=dict(status='resolved',story_id='SH-1',repository=str(root),
                             worktree=str(root / 'SH-1'),branch='worktree-SH-1',window_name='SH-1',socket_path=owner,
                             provider='codex',pane=dict(pane_id='%8')))))
            script = '''source "$STORY_PLUGIN_ROOT/lib/resources.sh"
refuse() { printf '%s\\n' "$*" >&2; exit 1; }
refuse_with() { refuse "$@"; }
story_cli() { cat "$REPORT"; }
load_story_resources SH-1 "$LEASE"
[ "$RESOURCE_SOCKET" = "$OWNER" ]
'''
            for lease in ('', json.dumps(dict(tmux=dict(socket_path=owner)))):
                with self.subTest(explicit=bool(lease)):
                    result = subprocess.run(['bash','-c',script], env=dict(env, REPORT=str(report),OWNER=owner,LEASE=lease),
                                            capture_output=True,text=True,timeout=15)
                    self.assertEqual(result.returncode,0,result.stderr)


if __name__ == '__main__': unittest.main()

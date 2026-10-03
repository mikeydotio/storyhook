"""Real RV-10 ownership through StoryHook's shipping launch paths (SH-825)."""

import io
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import shlex
import time
import fcntl
import socket
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

sys.dont_write_bytecode = True
PLUGIN = Path(__file__).resolve().parents[1]
ROOT = PLUGIN.parents[1]
sys.path.insert(0, str(ROOT / "scripts/tests"))
import load_grace  # noqa: E402

REVISION = "7f77ee8997a9e984479a910c14735852b63b5c83"


def view_program():
    """Compose the same shipping sources as the daemon's VIEW_PROGRAM."""
    sys.path.insert(0, str(PLUGIN / 'lib'))
    from view_program import program
    return program(ROOT)


class TransportTests(unittest.TestCase):
    """Each case owns all processes and sockets it can stop or replace."""

    @classmethod
    def setUpClass(cls):
        """Export a pinned provider without installation or network dependencies."""
        cls.patience = load_grace.patience(45, load_grace.contention())
        cls.provider_temp = tempfile.TemporaryDirectory(prefix="rv825-", dir="/tmp")
        cls.addClassCleanup(cls.provider_temp.cleanup)
        cls.provider = Path(cls.provider_temp.name).resolve()
        common = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
                                cwd=ROOT, capture_output=True, text=True, check=True, timeout=cls.patience)
        checkout = os.environ.get("STORY_TEST_REVIVIFY_REPO") or str(Path(common.stdout.strip()).parent.parent / "tmux-revivify")
        exported = subprocess.run(["git", "-C", checkout, "archive", REVISION],
                                  capture_output=True, timeout=cls.patience)
        if exported.returncode:
            raise AssertionError("RV-10 test prerequisite unavailable: set STORY_TEST_REVIVIFY_REPO to a repository "
                                 f"containing {REVISION}: {exported.stderr.decode()}")
        with tarfile.open(fileobj=io.BytesIO(exported.stdout)) as archive:
            # The reviewed Git tree contains only repository-relative paths.
            for member in archive.getmembers():
                if member.name.startswith("/") or ".." in Path(member.name).parts or member.issym() or member.islnk():
                    raise AssertionError(f"unsafe provider archive entry: {member.name}")
            if hasattr(tarfile, "data_filter"):
                archive.extractall(cls.provider, filter="data")
            else:
                archive.extractall(cls.provider)
        cls.cli = str(cls.provider / "bin/revivify")

    def setUp(self):
        """Start and adopt a bare isolated server; no user config is loaded."""
        self.temp = tempfile.TemporaryDirectory(prefix="sh825-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.env = {"HOME": str(self.root), "PATH": os.path.dirname(sys.executable) + os.pathsep + os.environ["PATH"],
                    "TMUX_TMPDIR": str(self.root), "PYTHONDONTWRITEBYTECODE": "1", "LANG": "en_US.UTF-8",
                    "STORYHOOK_VERIFIER_MIRROR": "1"}
        socket_dir = self.root / ("tmux-" + str(os.getuid()))
        socket_dir.mkdir(mode=0o700)
        self.socket = str(socket_dir / "default")
        self.tmux = shutil.which("tmux")
        self.assertIsNotNone(self.tmux)
        self.command([self.tmux, "-S", self.socket, "-f", "/dev/null", "new-session", "-d", "-s", "original", "/bin/sleep", "600"])
        self.addCleanup(self.stop)
        self.pid = int(self.command([self.tmux, "-N", "-S", self.socket, "display-message", "-p", "#{pid}"]).stdout)
        self.record = json.loads(self.command([self.cli, "server", "adopt", "--socket", self.socket,
                                              "--state-dir", str(self.root / "custom-snapshots"), "--json"]).stdout)
        self.endpoint = self.record["endpoint"]
        self.inode = os.stat(self.endpoint).st_ino
        (self.root / "logs").mkdir()
        self.reader = self.root / "reader"
        self.reader.write_text("#!/bin/sh\nexec /bin/sleep 600\n")
        self.reader.chmod(0o700)

    def command(self, argv, check=True, **kwargs):
        """Run one bounded fixture command with complete diagnostics."""
        result = subprocess.run(argv, env=kwargs.pop("env", self.env), capture_output=True,
                                text=True, timeout=self.patience, **kwargs)
        if check:
            self.assertEqual(result.returncode, 0, (argv[:3], result.stdout, result.stderr))
        return result

    def stop(self):
        """Release a stopped fixture before killing only its owned servers."""
        if hasattr(self, "pid"):
            try:
                os.kill(self.pid, signal.SIGCONT)
            except ProcessLookupError:
                pass
        sys.path.insert(0, str(PLUGIN / 'lib'))
        from process_identity import process_identity
        observed = []
        for endpoint in set((self.socket, getattr(self, "endpoint", self.socket))):
            rows = self.command([self.tmux, '-N', '-S', endpoint, 'list-panes', '-a', '-F', '#{pane_pid}'], check=False)
            for pid in rows.stdout.splitlines():
                try:
                    observed.append(process_identity(int(pid)))
                except ProcessLookupError:
                    pass
            self.command([self.tmux, "-N", "-S", endpoint, "kill-server"], check=False)
        # Login shells write history at exit. Socket disappearance alone is not
        # quiescence and can race TemporaryDirectory's recursive removal.
        deadline = time.monotonic() + self.patience
        for old in observed:
            while True:
                try:
                    if process_identity(old['pid']) != old:
                        break
                except ProcessLookupError:
                    break
                self.assertLess(time.monotonic(), deadline, 'owned fixture pane did not exit')
                time.sleep(.01)

    def invoke(self, path):
        """Execute the actual composed reader or provider-launch boundary."""
        if path == "view":
            return self.command([sys.executable, "-B", "-c", view_program(), "fixture",
                                 str(self.root / "logs"), str(self.reader)], check=False)
        if path == 'dispatch':
            cli = self.root / 'story-fixture'
            cli.write_text('#!/bin/sh\ncase "$*" in\n'
                           '*"project show"*) echo \'{"result":"ok","project":{"slug":"fixture","checkout":""}}\' ;;\n'
                           '*"engine reset-check"*) echo \'{"result":"ok"}\' ;;\n'
                           '*"show SH-1"*) echo \'{"result":"ok","story":{"story":{"id":"SH-1","state":"todo","story_type":"story"}}}\' ;;\n'
                           '*) echo "unexpected fixture story query: $*" >&2; exit 1 ;;\nesac\n')
            cli.chmod(0o700)
            return self.command(['bash', str(PLUGIN / 'bin/story.sh'), 'dispatch', 'SH-1'], check=False,
                                env=dict(self.env, STORY_BIN=str(cli), STORY_TARGET_SESSION='fixture',
                                         STORY_CREATE_SESSION='1', TMUX=self.socket + ',0,0'))
        return self.command([sys.executable, "-B", str(PLUGIN / "lib/tmux-launch.py"),
                             "new-session", "-d", "-s", "fixture", "/bin/sleep", "600"], check=False,
                            env=dict(self.env, TMUX=self.socket + ",0,0"))

    def shell_resources(self, body, **environment):
        """Exercise the shipping Bash boundary with real tmux and RV-10."""
        script = 'set -euo pipefail; source "$STORY_PLUGIN_ROOT/lib/resources.sh"; ' + body
        return self.command(['bash', '-c', script], check=False,
                            env=dict(self.env, STORY_PLUGIN_ROOT=str(PLUGIN), TMUX=self.socket + ',0,0', **environment))

    def test_bash_target_uses_private_owner_and_recognizes_caller_alias(self):
        pane = self.command([self.tmux, '-N', '-S', self.endpoint, 'display-message', '-p', '#{pane_id}']).stdout.strip()
        os.unlink(self.socket)
        self.command([self.tmux, '-S', self.socket, '-f', '/dev/null', 'new-session', '-d', '-s', 'foreign', '/bin/sleep', '600'])
        result = self.shell_resources('prepare_tmux_target ensure; tmux list-sessions -F "#{session_name}"; '
                                      'resource_is_self "$TMUX_PANE"; resource_socket_for_pane "$TMUX_PANE"', TMUX_PANE=pane)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ['original', self.endpoint])

    def test_bash_exact_session_does_not_reuse_a_prefix(self):
        result = self.shell_resources('prepare_tmux_target ensure; ensure_tmux_session orig "$HOME"; '
                                      'echo "$SESSION_CREATED"; tmux list-sessions -F "#{session_name}"')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(result.stdout.splitlines()), {'true', 'orig', 'original'})

    def test_bash_cleanup_target_publishes_checked_origin_and_omits_legacy_evidence(self):
        result = self.shell_resources('resource_cleanup_target /tmp/legacy.sock; prepare_tmux_target ensure; '
                                      'resource_cleanup_target "$RESOURCE_SOCKET"')
        self.assertEqual(result.returncode, 0, result.stderr)
        old, protected = map(json.loads, result.stdout.splitlines())
        self.assertEqual(old, {'socket_path': '/tmp/legacy.sock'})
        self.assertEqual(protected, {'socket_path': self.endpoint, 'revivify': {
            'logical_socket': self.socket, 'origin_generation': self.record['generation']}})

    def test_bash_cannot_override_a_captured_private_selector(self):
        for selector in ('-S "$RESOURCE_LOGICAL_SOCKET"', '-u -S "$RESOURCE_LOGICAL_SOCKET"', '-L other'):
            with self.subTest(selector=selector):
                result = self.shell_resources('prepare_tmux_target ensure; tmux ' + selector + ' list-sessions')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('protected tmux', result.stderr)

    def test_outer_dispatch_discovers_before_terminal_or_checkout_preflight(self):
        os.unlink(self.endpoint)
        result = self.invoke('dispatch')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('revivify', result.stdout + result.stderr)
        self.assertNotIn('has no checkout', result.stdout)
        self.assertFalse(os.path.exists(self.endpoint))

    def test_bash_concurrent_session_creators_accept_only_the_exact_winner(self):
        bin_dir = self.root / 'barrier-bin'
        bin_dir.mkdir()
        barrier = self.root / 'barrier'
        barrier.mkdir()
        wrapper = bin_dir / 'tmux'
        wrapper.write_text('#!' + sys.executable + '\nimport os,sys,time\nfrom pathlib import Path\n'
                           f'barrier=Path({str(barrier)!r})\n'
                           'if "new-session" in sys.argv:\n'
                           ' (barrier / str(os.getpid())).touch()\n'
                           f' deadline=time.monotonic()+{self.patience!r}\n'
                           ' while len(list(barrier.iterdir())) < 2:\n'
                           '  if time.monotonic() >= deadline: raise RuntimeError("creation barrier timed out")\n'
                           '  time.sleep(.01)\n'
                           f'os.execv({self.tmux!r}, [{self.tmux!r}, *sys.argv[1:]])\n')
        wrapper.chmod(0o700)
        def create(_):
            return self.shell_resources('prepare_tmux_target ensure; ensure_tmux_session concurrent "$HOME"; echo "$SESSION_CREATED"',
                                        PATH=str(bin_dir) + os.pathsep + self.env['PATH'])
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(create, range(2)))
        self.assertEqual([r.returncode for r in results], [0, 0], [(r.stdout, r.stderr) for r in results])
        self.assertEqual({r.stdout.strip() for r in results}, {'true', 'false'})
        panes = self.command([self.tmux, '-N', '-S', self.endpoint, 'list-panes', '-s', '-t', '=concurrent', '-F', '#{pane_id}'])
        self.assertEqual(len(panes.stdout.splitlines()), 1)

    def test_displaced_public_socket_is_ignored_by_both_shipping_paths(self):
        os.unlink(self.socket)
        self.command([self.tmux, "-S", self.socket, "-f", "/dev/null", "new-session", "-d", "-s", "foreign", "/bin/sleep", "600"])
        for path in ("view", "launch"):
            with self.subTest(path=path):
                result = self.invoke(path)
                if path == "view":
                    self.assertEqual(result.returncode, 0, result.stderr)
                else:
                    # The view already allocated this exact session on the owner.
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("duplicate session", result.stderr)
        private = self.command([self.tmux, "-N", "-S", self.endpoint, "list-sessions", "-F", "#{session_name}"]).stdout
        public = self.command([self.tmux, "-N", "-S", self.socket, "list-sessions", "-F", "#{session_name}"]).stdout
        self.assertEqual(set(private.splitlines()), {"original", "fixture"})
        self.assertEqual(public.strip(), "foreign")
        self.assertEqual(os.stat(self.endpoint).st_ino, self.inode)

    def test_lifecycle_inventory_uses_real_rv10_and_retains_private_binding(self):
        sys.path.insert(0, str(PLUGIN / 'lib'))
        import agent_identity
        import tmux_client
        os.unlink(self.socket)
        self.command([self.tmux, '-S', self.socket, '-f', '/dev/null', 'new-session', '-d', '-s', 'foreign', '/bin/sleep', '600'])
        with patch.dict(os.environ, self.env, clear=True), tmux_client.operation():
            panes = agent_identity.panes(self.socket)
            self.assertEqual(len(panes), 1)
            pane = next(iter(panes.values()))
            self.assertEqual(pane['socket'], self.endpoint)
            self.assertEqual(agent_identity.pane_at(pane['pane'], pane['socket']), pane)
        inspected = self.command([sys.executable, '-B', str(PLUGIN / 'lib/tmux-env.py'), 'retained'],
                                 env=dict(self.env, TMUX=self.endpoint + ',0,0'))
        self.assertEqual(inspected.returncode, 0)
        self.assertEqual(os.stat(self.endpoint).st_ino, self.inode)
        self.assertEqual(self.command([self.tmux, '-N', '-S', self.socket, 'list-sessions', '-F', '#{session_name}']).stdout.strip(), 'foreign')

    def restore_current(self, config="/dev/null"):
        """Save and restore the owned server through the real RV-10 protocol."""
        program = '''
import json, os, sys, time
sys.path.insert(0, sys.argv[1])
from revivify.ownership import Ownership
from revivify.saving import save
from revivify.capture.snapshotter import Snapshotter
from revivify.store.blobs import BlobStore
from revivify.config import Settings
from revivify.agents.registry import build_detector
from pathlib import Path
from revivify.managed import ensure_server
from revivify.process_identity import process_state
owner = Ownership(sys.argv[2], os.environ)
old = owner.read()
client = owner.client(old, sys.argv[3])
paths = owner.paths(old)
saved = save(paths, Snapshotter(client, BlobStore(paths.blobs), Settings(), agents=build_detector(paths, os.environ, Path(os.environ['HOME']))), 'storyhook-test')
client.run('kill-server')
deadline = time.monotonic() + float(sys.argv[5])
while process_state(old['identity']) != 'exited':
    if time.monotonic() > deadline:
        raise RuntimeError('fixture server did not exit')
    time.sleep(0.01)
new = ensure_server(owner, sys.argv[3], sys.argv[4], old['state_dir'], config_file=sys.argv[6])
print(json.dumps(dict(target=new, snapshot=saved.snapshot_id)))
'''
        result = json.loads(self.command([sys.executable, '-B', '-c', program, str(self.provider),
                                         self.socket, self.tmux, self.cli, str(self.patience), config]).stdout)
        self.endpoint = result['target']['endpoint']
        self.pid = result['target']['identity']['pid']
        return result

    def test_restore_evidence_matches_real_provider_snapshot_and_receipt(self):
        result = self.restore_current()
        sys.path.insert(0, str(PLUGIN / 'lib'))
        import tmux_client
        from tmux_target import restore_evidence
        with patch.dict(os.environ, self.env, clear=True), tmux_client.operation():
            target = tmux_client.client(self.socket).target
            evidence = restore_evidence(target, self.env)
        self.assertEqual(evidence['snapshot_id'], result['snapshot'])
        self.assertEqual(evidence['source_generation'], self.record['generation'])
        self.assertEqual(evidence['source_endpoint'], self.record['endpoint'])
        self.assertEqual(len(evidence['panes']), 1)
        uuid, saved = next(iter(evidence['panes'].items()))
        current_uuid = self.command([self.tmux, '-N', '-S', self.endpoint, 'show-options',
                                     '-p', '-v', '-t', saved['pane_id'], '@revivify-uuid']).stdout.strip()
        self.assertEqual(uuid, current_uuid)
        from restored_dispatch import restored_launch
        command = self.command([self.tmux, '-N', '-S', self.endpoint, 'display-message', '-p',
                                '-t', saved['pane_id'], '#{pane_start_command}']).stdout.strip()
        restored_launch(target, uuid, command)

    def restored_provider(self, provider, live=False):
        """Actual restore, shell replay, provider argv, Git marker and publication."""
        repo = self.root / 'repo'
        repo.mkdir()
        self.command(['git', 'init', '-b', 'main', str(repo)])
        self.command(['git', '-C', str(repo), '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                      'commit', '--allow-empty', '-m', 'fixture'])
        worktree = self.root / 'SH-1'
        self.command(['git', '-C', str(repo), 'worktree', 'add', '-b', 'worktree-SH-1', str(worktree)])
        common = str(repo / '.git')
        private = self.command(['git', '-C', str(worktree), 'rev-parse', '--absolute-git-dir']).stdout.strip()
        binary = self.root / 'bin'
        binary.mkdir()
        source = self.root / 'claude.c'
        source.write_text(r'''#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <limits.h>
#include <string.h>
#include <sys/file.h>
int main(int argc, char **argv) {
  char path[PATH_MAX]; snprintf(path, sizeof(path), "%s/provider-calls", getenv("HOME"));
  if(strstr(argv[0],"codex") && argc>2) {
    char lockpath[PATH_MAX]; snprintf(lockpath,sizeof(lockpath),"%s/.codex/thread-writer-locks/%s.lock",getenv("HOME"),argv[2]);
    FILE *lock=fopen(lockpath,"a"); if(!lock || flock(fileno(lock),LOCK_EX)) return 3;
  }
  FILE *f=fopen(path,"a"); if(!f) return 2;
  fprintf(f,"%d",getpid()); for(int i=1;i<argc;i++) fprintf(f," %s",argv[i]); fprintf(f,"\n"); fclose(f);
  while(1) pause();
}
''')
        self.command(['cc', '-Wall', '-Werror', str(source), '-o', str(binary / provider)])
        self.env['PATH'] = str(binary) + os.pathsep + self.env['PATH']
        session = '2dd387a4-715d-4010-8fb0-937c1246405a'
        transcripts = self.root / ('.claude/projects/fixture' if provider == 'claude' else '.codex/sessions')
        transcripts.mkdir(parents=True)
        transcript = transcripts / (session + '.jsonl')
        if provider == 'claude':
            transcript.write_text(json.dumps(dict(type='assistant', sessionId=session, isSidechain=False,
                                  cwd=str(worktree), message=dict(stop_reason='end_turn'))) + '\n')
        else:
            transcript.write_text(json.dumps(dict(type='session_meta', payload=dict(id=session,cwd=str(worktree),source='cli'))) + '\n'
                                  + json.dumps(dict(type='event_msg',payload=dict(type='task_complete',turn_id='turn'))) + '\n')
            (self.root / '.codex/thread-writer-locks').mkdir()
            database = sqlite3.connect(str(self.root / '.codex/state_5.sqlite'))
            try:
                database.execute('CREATE TABLE threads (id TEXT, cwd TEXT, name TEXT, thread_source TEXT, created_at_ms INTEGER, updated_at_ms INTEGER)')
                database.execute('INSERT INTO threads VALUES (?,?,?, ?,0,0)', (session,str(worktree),'fixture','user'))
                database.commit()
            finally:
                database.close()
        resume_args = ['--resume', session] if provider == 'claude' else ['resume', session]
        (worktree / '.claude').mkdir()
        (worktree / '.claude/dispatch-sentinel.json').write_text(json.dumps(dict(
            story_id='SH-1', session_id=session, transcript_path=str(transcript))))
        lease = dict(version=1, project_slug='fixture', story_id='SH-1', repository_path=str(repo),
                     worktree_path=str(worktree), branch='worktree-SH-1',
                     tmux=dict(socket_path=self.endpoint, revivify=dict(logical_socket=self.socket,
                               origin_generation=self.record['generation'])))
        marker = Path(private) / 'storyhook-cleanup-lease-v1.json'
        marker.write_text(json.dumps(lease))
        def tmux(*args):
            return self.command([self.tmux, '-N', '-S', self.endpoint, *args]).stdout.strip()
        tmux('set-environment', '-g', 'PATH', self.env['PATH'])
        discarded = tmux('new-window', '-d', '-P', '-F', '#{pane_id}', '/bin/sleep', '600')
        tmux('kill-pane', '-t', discarded)
        pane = tmux('new-window', '-d', '-P', '-F', '#{pane_id}', '-n', 'SH-1', '-c', str(worktree),
                    str(binary / provider), *resume_args)
        sys.path.insert(0, str(PLUGIN / 'lib'))
        import agent_identity, continuation_runtime, restoration, tmux_client
        old_cwd = os.getcwd()
        try:
            os.chdir(worktree)
            with patch.dict(os.environ, dict(self.env, TMUX=self.endpoint + ',0,0'), clear=True), tmux_client.operation():
                observed = agent_identity.pane_at(pane, self.endpoint)
                identity = agent_identity.observe(dict(project='fixture', story='SH-1', common=common), observed, provider)
                tmux('set-option', '-p', '-t', pane, agent_identity.OPTION, json.dumps(identity))
                tmux('set-option', '-w', '-t', pane, '@storyhook-agent', provider)
                metadata = continuation_runtime.register(dict(socket=self.endpoint, pane=pane, provider=provider,
                           story_id='SH-1', cwd=str(worktree), model='default', effort='', speed='', autonomy_mode='auto'))['capture']
            if live:
                # These are the pre-adoption public bindings of the same native
                # process. The fixture already completed RV-10's live adoption.
                lease['tmux'] = dict(socket_path=self.socket)
                identity['socket'] = self.socket
                metadata['socket'] = self.socket
                marker.write_text(json.dumps(lease))
                tmux('set-option', '-p', '-t', pane, agent_identity.OPTION, json.dumps(identity))
                tmux('set-option', '-w', '-t', pane, continuation_runtime.OPTION, json.dumps(metadata))
            (self.root / '.zshrc').write_text('export PATH=' + shlex.quote(self.env['PATH']) + '\nsource '
                                          + shlex.quote(str(self.provider / 'shell/revivify.zsh')) + '\n')
            config = self.root / 'tmux.conf'
            config.write_text('set -g default-shell /bin/zsh\n')
            if not live:
                self.restore_current(str(config))
            deadline = time.monotonic() + self.patience
            while not (self.root / 'provider-calls').exists() or len((self.root / 'provider-calls').read_text().splitlines()) < (1 if live else 2):
                if time.monotonic() >= deadline:
                    snapshots = list((self.root / 'custom-snapshots').glob('generations/*/snapshots/*storyhook-test.json'))
                    self.fail('actual RV-10 provider replay did not start: ' + tmux('capture-pane', '-p', '-t', 'SH-1') + repr([p.read_text() for p in snapshots]))
                time.sleep(.05)
            lockpath = Path(common) / 'storyhook/workspace-locks/SH-1.lock'
            lockpath.parent.mkdir(parents=True)
            with lockpath.open('w') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                env = dict(self.env, STORY_WORKSPACE_LOCK_FD=str(lock.fileno()))
                with patch.dict(os.environ, env, clear=True), tmux_client.operation():
                    value = dict(lease=lease, window='SH-1')
                    proposal = restoration.propose(value)
                    if live:
                        self.assertEqual(proposal['identity']['process'], identity['process'])
                    else:
                        self.assertNotEqual(proposal['identity']['process'], identity['process'])
                        self.assertNotEqual(proposal['pane'], pane)
                    self.assertEqual(proposal['metadata']['session_id'], session)
                    if not live:
                        self.assertEqual(proposal['identity']['restored']['session_id'], session)
                    restoration.publish(value, proposal)
                    restoration.publish(value, proposal)
                    agent_identity.validate(proposal['identity'])
                    self.assertEqual(continuation_runtime.owner(proposal['metadata'] | {'lease':proposal['lease']}), 'present')
                    self.assertEqual(json.loads(marker.read_text()), proposal['lease'])
                    self.assertEqual(proposal['lease']['tmux'].get('revivify'), lease['tmux'].get('revivify'))
                    self.assertEqual(proposal['metadata']['autonomy_mode'], metadata['autonomy_mode'])
            calls = (self.root / 'provider-calls').read_text().splitlines()
            self.assertEqual(len(calls), 1 if live else 2, calls)
            self.assertTrue(all(' '.join(resume_args) in call for call in calls), calls)
        finally:
            os.chdir(old_cwd)

    def test_restored_claude_rebinds_real_process_and_retains_conversation(self):
        self.restored_provider('claude')

    def test_restored_codex_rebinds_real_process_and_retains_conversation(self):
        self.restored_provider('codex')

    def test_live_adoption_preserves_process_and_rebinds_public_alias(self):
        self.restored_provider('claude', live=True)

    def test_view_readopts_a_healthy_reader_after_real_restore(self):
        sys.path.insert(0, str(PLUGIN / 'lib'))
        from process_observation import observe_process
        source = self.root / 'reader.c'
        source.write_text('#include <unistd.h>\nint main(void) { while(1) pause(); }\n')
        self.command(['cc', '-Wall', '-Werror', str(source), '-o', str(self.reader)])
        self.assertEqual(self.invoke('view').returncode, 0)
        def tmux(*args):
            return self.command([self.tmux, '-N', '-S', self.endpoint, *args]).stdout.strip()
        before = tmux('display-message', '-p', '-t', 'fixture:verification', '#{pane_id}:#{pane_pid}')
        proof = json.loads(tmux('show-options', '-p', '-v', '-t', 'fixture:verification', '@storyhook-reader-proof-v1'))
        self.assertEqual(proof['process']['process']['executable'], str(self.reader))
        config = self.root / 'reader-tmux.conf'
        reader_command = shlex.join([str(self.reader), 'daemon', 'logs', '--directory', str(self.root / 'logs'), '--follow'])
        config.write_text('set -g default-shell /bin/zsh\nset -g default-command ' + shlex.quote(reader_command) + '\n')
        self.restore_current(str(config))
        restored = tmux('display-message', '-p', '-t', 'fixture:verification', '#{pane_id}:#{pane_pid}')
        self.assertNotEqual(before, restored)
        # RV-10 readiness publishes the replay shell before its default command
        # has necessarily exec'd. This case requires an actually healthy reader;
        # the sibling case exercises a shell that never starts one.
        deadline = time.monotonic() + self.patience
        while True:
            current = observe_process(int(restored.split(':')[1]))
            if current['process']['executable'] == str(self.reader) and current['argv'] == shlex.split(reader_command):
                break
            self.assertLess(time.monotonic(), deadline, repr(current))
            time.sleep(.05)
        result = self.invoke('view')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(tmux('display-message', '-p', '-t', 'fixture:verification', '#{pane_id}:#{pane_pid}'), restored,
                         repr(dict(source=proof['process'], restored=current)))
        self.assertEqual(tmux('show-options', '-w', '-v', '-t', 'fixture:verification', '@storyhook-reader'), restored)
        self.assertEqual(self.invoke('view').returncode, 0)
        self.assertEqual(tmux('display-message', '-p', '-t', 'fixture:verification', '#{pane_id}:#{pane_pid}'), restored)

    def test_view_replaces_stale_replay_shell_once(self):
        self.assertEqual(self.invoke('view').returncode, 0)
        self.restore_current()
        first = self.invoke('view')
        self.assertEqual(first.returncode, 0, first.stderr)
        before = self.command([self.tmux, '-N', '-S', self.endpoint, 'display-message', '-p',
                               '-t', 'fixture:verification', '#{pane_id}:#{pane_pid}']).stdout
        second = self.invoke('view')
        self.assertEqual(second.returncode, 0, second.stderr)
        after = self.command([self.tmux, '-N', '-S', self.endpoint, 'display-message', '-p',
                              '-t', 'fixture:verification', '#{pane_id}:#{pane_pid}']).stdout
        self.assertEqual(before, after)

    def test_saturated_private_listener_preserves_work_and_generation(self):
        peers = []
        os.kill(self.pid, signal.SIGSTOP)
        try:
            refused = False
            for _ in range(256):
                peer = socket.socket(socket.AF_UNIX)
                peers.append(peer)
                peer.settimeout(0.01)  # A non-answer is the intentional listener saturation proof.
                try:
                    peer.connect(self.endpoint)
                except OSError:
                    refused = True
                    break
            self.assertTrue(refused, "fixture did not saturate the real listener")
            for path in ("view", "launch", "dispatch"):
                with self.subTest(path=path):
                    result = self.invoke(path)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("revivify", result.stderr + result.stdout)
                    self.assertEqual(os.stat(self.endpoint).st_ino, self.inode)
                    self.assertEqual(os.stat(self.socket).st_ino, self.inode)
        finally:
            for peer in peers:
                peer.close()
            os.kill(self.pid, signal.SIGCONT)
        report = json.loads(self.command([self.cli, "server", "inspect", "--socket", self.socket, "--json"]).stdout)
        self.assertEqual(report["generation"], self.record["generation"])
        self.assertEqual(report["identity"], self.record["identity"])
        self.assertEqual(self.command([self.tmux, "-N", "-S", self.endpoint, "list-sessions", "-F", "#{session_name}"]).stdout.strip(), "original")

    def test_missing_private_endpoint_is_not_recreated(self):
        os.unlink(self.endpoint)
        for path in ("view", "launch"):
            result = self.invoke(path)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("revivify", result.stderr)
            self.assertFalse(os.path.exists(self.endpoint))
        self.assertEqual(self.command([self.tmux, "-N", "-S", self.socket, "list-sessions", "-F", "#{session_name}"]).stdout.strip(), "original")

    def test_endpoint_disappearing_after_ensure_cannot_start_a_replacement(self):
        sys.path.insert(0, str(PLUGIN / "lib"))
        spec = importlib.util.spec_from_file_location("fixture_tmux_launch", PLUGIN / "lib/tmux-launch.py")
        launcher = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(launcher)
        run = subprocess.run
        allocations = []

        def lose_endpoint(argv, **kwargs):
            if argv[0] == "tmux" and "new-session" in argv:
                allocations.append(argv)
                os.unlink(self.endpoint)
            return run(argv, **kwargs)

        with patch.dict(os.environ, dict(self.env, TMUX=self.socket + ",0,0"), clear=True), \
                patch.object(sys, "argv", ["tmux-launch.py", "new-session", "-d", "-s", "fixture"]), \
                patch.object(launcher.subprocess, "run", side_effect=lose_endpoint):
            with launcher.probe_budget.operation():
                self.assertNotEqual(launcher.main(), 0)
        self.assertEqual(len(allocations), 1)
        self.assertEqual(allocations[0][1:4], ["-N", "-S", self.endpoint])
        self.assertFalse(os.path.exists(self.endpoint))
        self.assertEqual(self.command([self.tmux, "-N", "-S", self.socket, "list-sessions", "-F", "#{session_name}"]).stdout.strip(), "original")

    def test_concurrent_views_reuse_one_generation_and_one_reader(self):
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(self.invoke, ["view", "view"]))
        for result in results:
            self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(self.command([self.cli, "server", "inspect", "--socket", self.socket, "--json"]).stdout)
        self.assertEqual(report["generation"], self.record["generation"])
        panes = self.command([self.tmux, "-N", "-S", self.endpoint, "list-panes", "-s", "-t", "=fixture", "-F", "#{pane_id}"]).stdout.splitlines()
        self.assertEqual(len(panes), 1)


if __name__ == "__main__":
    unittest.main()

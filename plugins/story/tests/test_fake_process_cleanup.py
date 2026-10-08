"""Regressions for leaked plugin fake panes and their state-writing descendants.

Run through test-fake-process-cleanup.sh for the existing isolated home and
binary lease. Child shells source the real lib.sh but share that home: they
never start/stop a daemon or invoke a provider, real tmux or Git. Their separate
process ledgers and scopes, fake panes, pipes and files are wholly test-owned.
No waitid API is required; Apple Python 3.9 and Homebrew Python are supported.
"""
import errno
import json
import os
from pathlib import Path
import selectors
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True
TESTS = Path(__file__).resolve().parent
sys.path.insert(0, str(TESTS.parents[2] / "scripts"))
sys.path.insert(0, str(TESTS.parents[2] / "scripts/tests"))
from host_admission import native
import load_grace


def allowance():
    return load_grace.patience(30, load_grace.contention())


def alive(row):
    try:
        current = native.identity(row["pid"], native.boot_identity())
    except ProcessLookupError:
        return False
    if current != row:
        raise FixtureAborted("native identity changed; no guessed cleanup")
    return True


class FixtureAborted(KeyboardInterrupt):
    """Uncertain custody stops the suite and retains its exact diagnostic roots."""


DRIVER = r'''
source "$1" || exit 97
_TMP_REPOS+=("$FIXTURE_ROOT/owned")
printf '%s\n%s\n' "$FAKE_TMUX_PROCESS_LEDGER" "$FAKE_TMUX_PROCESS_SCOPE" > "$FIXTURE_ROOT/owner"
printf '__ready__\n'
while IFS= read -r fixture_command; do
  eval "$fixture_command"
  printf '__done__:%s\n' "$?"
done
'''


class Fixture:
    def __init__(self):
        if not os.environ.get("STORYHOOK_TEST_HOME"):
            raise RuntimeError("run via test-fake-process-cleanup.sh: isolated home/lease required")
        self.root = Path(tempfile.mkdtemp(prefix="story-test-custody.", dir="/tmp"))
        self.state = self.root / "owned/state"
        self.state.mkdir(parents=True)
        self.records = {}
        env = {k: v for k, v in os.environ.items() if not k.startswith("FAKE_TMUX_")}
        env.update(FIXTURE_ROOT=str(self.root), FAKE_TMUX_STATE=str(self.state),
                   FAKE_TMUX_PANE_LIFETIME="900", FAKE_TMUX_CLEANUP_SECONDS=str(allowance()))
        self.log = (self.root / "stderr").open("w+")
        self.child = subprocess.Popen(["bash", "-c", DRIVER, "fixture", str(TESTS / "lib.sh")],
                                      env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=self.log, text=True, bufsize=1)
        self.closed = False
        self.ledger = None
        self.scope = None
        try:
            self.read("__ready__")
            ledger, self.scope = (self.root / "owner").read_text().splitlines()
            self.ledger = Path(ledger)
        except BaseException:
            self.child.stdin.close()
            self._wait()
            raise FixtureAborted("fixture startup refused; retained " + str(self.root)) from None

    def read(self, expected):
        deadline = time.monotonic() + allowance()
        while True:
            with selectors.DefaultSelector() as selector:
                selector.register(self.child.stdout, selectors.EVENT_READ)
                if not selector.select(max(0, deadline - time.monotonic())):
                    raise AssertionError("fixture reply timed out; " + str(self.root))
            line = self.child.stdout.readline().rstrip("\n")
            if line.startswith(expected):
                return line
            if not line:
                raise AssertionError("fixture exited before reply; " + str(self.root))

    def command(self, command, expected=0):
        self.child.stdin.write(command + "\n")
        self.child.stdin.flush()
        reply = self.read("__done__:")
        self.refresh()
        if reply != "__done__:" + str(expected):
            raise AssertionError(reply + " from " + command + "; " + str(self.root))

    def refresh(self):
        if self.ledger and self.ledger.exists():
            for path in self.ledger.glob("*.process.json"):
                row = json.loads(path.read_text())
                self.records[path.name] = row

    def launch(self, child=False, delayed=False, state=None):
        prefix = "FAKE_TMUX_PANE_CHILD=1 " if child else ""
        if delayed:
            prefix += "FAKE_TMUX_SENTINEL_DELAY_SECS=900 "
        if state is not None:
            prefix += "FAKE_TMUX_STATE=" + shlex.quote(str(state)) + " "
        provider = "claude" if delayed else "codex"
        self.command(prefix + '"$TESTS_DIR/fakes/tmux" new-window -t fixture -n fixture '
                     '-c "$FAKE_TMUX_STATE" ' + provider + ' >/dev/null')
        return [row for row in self.records.values() if not row["settled"]]

    def finish(self, status=0):
        self.refresh()
        self.child.stdin.write("exit " + str(status) + "\n")
        self.child.stdin.flush()
        self.child.stdin.close()
        self._wait()
        if self.child.returncode != status:
            raise AssertionError("cleanup changed exit status; " + str(self.root))
        if (self.root / "owned").exists() or self.ledger.exists():
            raise AssertionError("successful cleanup retained owned roots; " + str(self.root))

    def _wait(self):
        try:
            self.child.wait(timeout=allowance() + 5)
        except subprocess.TimeoutExpired:
            raise FixtureAborted("owner did not settle; retained " + str(self.root))
        self.closed = True
        self.child.stdout.close()
        self.log.close()

    def dispose(self):
        if not self.closed:
            self.refresh()
            self.child.stdin.close()  # EOF asks the original owner to run its real EXIT trap.
            self._wait()
        if self.ledger is None or self.ledger.exists():
            raise FixtureAborted("unsettled ledger; retained " + str(self.root))
        if any(alive(row["native"]) for row in self.records.values()):
            raise FixtureAborted("live registered writer; retained " + str(self.root))
        shutil.rmtree(self.root)


class CleanupTests(unittest.TestCase):
    def setUp(self):
        self.fixture = Fixture()
        self.addCleanup(self.fixture.dispose)

    def test_success_exit_settles_all_overridden_states(self):
        f = self.fixture
        rows = f.launch()
        second = f.root / "owned/second"
        second.mkdir()
        rows += f.launch(state=second)
        self.assertEqual(len({r["native"]["pid"] for r in rows}), 2)
        self.assertTrue(all(alive(row["native"]) for row in rows))
        f.finish()
        self.assertTrue(all(not alive(row["native"]) for row in rows))

    def test_failure_exit_preserves_status_and_settles_child(self):
        f = self.fixture
        rows = f.launch(child=True)
        self.assertEqual({r["role"] for r in rows}, {"pane-child", "child"})
        self.assertTrue(all(alive(r["native"]) for r in rows))
        f.finish(23)
        self.assertTrue(all(not alive(r["native"]) for r in rows))

    def test_overwritten_pid_and_generations_cannot_signal_unrelated_process(self):
        f = self.fixture
        unrelated = subprocess.Popen(["sleep", "900"], stdin=subprocess.DEVNULL,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            pin = native.identity(unrelated.pid, native.boot_identity())
            previous = f.launch(child=True)
            (f.state / "pane_pid").write_text(str(unrelated.pid))
            current = f.launch(child=True)
            self.assertTrue(all(not alive(r["native"]) for r in previous))
            self.assertTrue(alive(pin))
            (f.state / "pane_pid").write_text(str(unrelated.pid))
            f.command('"$TESTS_DIR/fakes/tmux" kill-window -t @1')
            self.assertTrue(all(not alive(r["native"]) for r in current))
            self.assertTrue(alive(pin))
            f.finish()
            self.assertTrue(alive(pin))
        finally:
            # Only our unreaped direct sleep child; no descendants or group sweep.
            if unrelated.poll() is None:
                unrelated.terminate()
            unrelated.wait(timeout=allowance())

    def test_altered_launch_token_refuses_and_retains_all_roots(self):
        f = self.fixture
        [row] = f.launch()
        path = next(f.ledger.glob("*.process.json"))
        original = path.read_text()
        altered = json.loads(original)
        altered["native"]["start"] += ":altered"
        path.write_text(json.dumps(altered))
        try:
            f.command('( _cleanup )', expected=1)
            self.assertTrue(alive(row["native"]))
            self.assertTrue(f.state.is_dir())
            self.assertTrue(f.ledger.is_dir())
            self.assertIn("different native incarnation", (f.root / "stderr").read_text())
        finally:
            path.write_text(original)
        f.finish()

    def test_delayed_publisher_is_registered_and_settled_before_state_deletion(self):
        f = self.fixture
        rows = f.launch(delayed=True)
        self.assertEqual({r["role"] for r in rows}, {"pane", "publisher"})
        self.assertTrue(all(alive(r["native"]) for r in rows))
        self.assertFalse((f.state / "claude-hook-output.json").exists())
        f.finish()
        self.assertTrue(all(not alive(r["native"]) for r in rows))

    def test_nested_scope_preserves_outer_pane_and_settles_own_child_and_publisher(self):
        f = self.fixture
        [outer] = f.launch()
        nested = f.root / "owned/nested"
        nested.mkdir()
        script = r'''
source "$1" || exit 98
_TMP_REPOS+=("$FAKE_TMUX_STATE")
FAKE_TMUX_PANE_CHILD=1 FAKE_TMUX_SENTINEL_DELAY_SECS=900 \
  "$TESTS_DIR/fakes/tmux" new-window -t nested -n nested -c "$FAKE_TMUX_STATE" claude >/dev/null || exit 99
cp "$FAKE_TMUX_PROCESS_LEDGER"/*.process.json "$FIXTURE_ROOT/receipts/"
'''
        (f.root / "receipts").mkdir()
        f.command("FAKE_TMUX_STATE=" + shlex.quote(str(nested)) + " bash -c " +
                  shlex.quote(script) + " nested " + shlex.quote(str(TESTS / "lib.sh")))
        rows = [json.loads(p.read_text()) for p in (f.root / "receipts").glob("*.json")]
        own = [r for r in rows if r["scope"] != f.scope]
        self.assertEqual({r["role"] for r in own}, {"pane-child", "child", "publisher"})
        self.assertTrue(alive(outer["native"]))
        self.assertTrue(all(not alive(r["native"]) for r in own))
        self.assertFalse(nested.exists())
        self.assertTrue(f.ledger.exists())
        f.finish()


    def test_outer_cleanup_refuses_while_nested_scope_owner_is_live(self):
        f = self.fixture
        nested = f.root / "owned/nested-live"
        nested.mkdir()
        script = r'''
source "$1" || exit 98
_TMP_REPOS+=("$FAKE_TMUX_STATE")
printf '%s' "$FAKE_TMUX_PROCESS_SCOPE" > "$FIXTURE_ROOT/nested-scope.tmp"
mv "$FIXTURE_ROOT/nested-scope.tmp" "$FIXTURE_ROOT/nested-scope"
IFS= read -r release < "$FIXTURE_ROOT/nested-release"
'''
        os.mkfifo(f.root / 'nested-release')
        try:
            f.command("FAKE_TMUX_STATE=" + shlex.quote(str(nested)) + " bash -c " +
                      shlex.quote(script) + " nested " + shlex.quote(str(TESTS / "lib.sh")) +
                      ' </dev/null >"$FIXTURE_ROOT/nested-output" 2>&1 & nested_fixture_pid=$!')
            self.wait_until(lambda: (f.root / 'nested-scope').exists())
            scope = (f.root / 'nested-scope').read_text()
            owner = json.loads((f.ledger / (scope + '.scope.json')).read_text())['owner']
            f.command('( _cleanup )', expected=1)
            self.assertTrue(alive(owner))
            self.assertTrue(f.state.exists())
            self.assertTrue(nested.exists())
            self.assertIn('nested fixture scope is still active', (f.root / 'stderr').read_text())
        finally:
            # The owner remains our shell's exact direct child. A FIFO release
            # asks it to run its real EXIT cleanup; no PID receives a signal.
            self.wait_until(lambda: (f.root / 'nested-scope').exists())
            descriptor = None
            deadline = time.monotonic() + allowance()
            while descriptor is None:
                try:
                    descriptor = os.open(f.root / 'nested-release', os.O_WRONLY | os.O_NONBLOCK)
                except OSError as error:
                    if error.errno != errno.ENXIO or time.monotonic() >= deadline:
                        raise FixtureAborted('nested release refused; retained ' + str(f.root)) from error
                    time.sleep(0.01)
            try:
                os.write(descriptor, b'release\n')
            finally:
                os.close(descriptor)
        f.command('wait "$nested_fixture_pid"')
        self.assertFalse(nested.exists())
        f.finish()

    def test_copied_valid_foreign_launch_refuses_before_any_signal(self):
        f = self.fixture
        other = Fixture()
        try:
            [ours] = f.launch()
            [theirs] = other.launch()
            source = next(other.ledger.glob('*.process.json'))
            copied = f.ledger / source.name
            copied.write_bytes(source.read_bytes())
            try:
                f.command('( _cleanup )', expected=1)
                self.assertTrue(alive(ours['native']))
                self.assertTrue(alive(theirs['native']))
                self.assertTrue(f.state.exists())
                self.assertIn('different fixture ledger', (f.root / 'stderr').read_text())
            finally:
                copied.unlink()
            f.finish()
            self.assertTrue(alive(theirs['native']))
            other.finish()
        finally:
            other.dispose()

    def test_replaced_custody_path_cannot_hide_the_held_original_writer(self):
        f = self.fixture
        [row] = f.launch()
        # The live pane holds the original shared inode. Each replacement is
        # refused before any TERM, even though its new inode can be locked.
        for path in (f.ledger / 'lock', f.ledger / 'writers',
                     f.ledger / (f.scope + '.scope-writers'), f.ledger / row['writer']):
            saved = path.with_name(path.name + '.original')
            path.rename(saved)
            path.touch()
            try:
                f.command('( _cleanup )', expected=1)
                self.assertTrue(alive(row['native']))
                self.assertTrue(f.state.exists())
                self.assertTrue(saved.exists())
                self.assertIn('custody file identity changed', (f.root / 'stderr').read_text())
            finally:
                path.unlink()
                saved.rename(path)
        f.finish()

    def start_publisher(self, script):
        f = self.fixture
        f.command('( exec 7<"$FAKE_TMUX_PROCESS_LEDGER/$FAKE_TMUX_PROCESS_SCOPE.scope-writers"; '
                  'exec 8<"$FAKE_TMUX_PROCESS_LEDGER/writers"; '
                  'python3 -B "$TESTS_DIR/fake-process-owner.py" writer '
                  '"$FAKE_TMUX_PROCESS_LEDGER" "$FAKE_TMUX_PROCESS_SCOPE" 8 7 && '
                  'python3 -B "$TESTS_DIR/fake-process-owner.py" start '
                  '"$FAKE_TMUX_PROCESS_LEDGER" "$FAKE_TMUX_STATE" publisher 0 fixture ' +
                  shlex.quote(str(script)) + ' "$FAKE_TMUX_STATE" >/dev/null )')

    def wait_until(self, predicate):
        deadline = time.monotonic() + allowance()
        while not predicate():
            if time.monotonic() >= deadline:
                raise AssertionError("fixture witness did not arrive: " + str(self.fixture.root))
            time.sleep(0.01)

    def test_exited_publisher_with_live_unregistered_writer_refuses_root_deletion(self):
        f = self.fixture
        # A controlled late writer inherits the actual publisher's custody.
        # It exits cooperatively on its private release file; nobody signals an
        # unregistered PID, and no fixture reaches real hook/provider code.
        writer = f.root / "writer.py"
        writer.write_text("""
import json, os, sys, time
from pathlib import Path
sys.path.insert(0, sys.argv[2])
from host_admission import native
root = Path(sys.argv[1])
boot = native.boot_identity()
row = native.process(os.getpid(), boot)
(root / 'writer-ready.tmp').write_text(json.dumps(row))
(root / 'writer-ready.tmp').replace(root / 'writer-ready.json')
while not (root / 'release-writer').exists():
    time.sleep(0.01)
""")
        launcher = f.root / "publisher.sh"
        launcher.write_text('#!/usr/bin/env bash\nexec ' + shlex.quote(sys.executable) +
                            ' -B -c ' + shlex.quote("""
import os, subprocess, sys, time
from pathlib import Path
root = Path(sys.argv[1])
child = subprocess.Popen([sys.executable, '-B', str(root / 'writer.py'), str(root), sys.argv[2]], close_fds=False)
deadline = time.monotonic() + float(sys.argv[3])
while not (root / 'writer-ready.json').exists():
    if time.monotonic() >= deadline:
        raise SystemExit(91)
    time.sleep(0.01)
""") + ' ' + shlex.quote(str(f.root)) + ' ' +
                            shlex.quote(str(TESTS.parents[2] / 'scripts')) + ' ' + str(allowance()))
        try:
            self.start_publisher(launcher)
            self.wait_until(lambda: (f.root / 'writer-ready.json').exists())
            row = json.loads((f.root / 'writer-ready.json').read_text())
            pin = {key: row[key] for key in ('pid', 'start', 'boot')}
            [publisher] = [r for r in f.records.values() if r['role'] == 'publisher']
            self.assertEqual(row['parent'], publisher['native']['pid'])
            self.wait_until(lambda: not alive(publisher['native']))
            self.assertTrue(alive(pin))
            f.command('( FAKE_TMUX_CLEANUP_SECONDS=0.1; export FAKE_TMUX_CLEANUP_SECONDS; _cleanup )', expected=1)
            self.assertTrue(f.state.exists())
            self.assertTrue(f.ledger.exists())
            self.assertTrue(alive(pin))
            self.assertIn('writer still borrows its state', (f.root / 'stderr').read_text())
        finally:
            (f.root / 'release-writer').touch()
        self.wait_until(lambda: not alive(pin))
        f.finish()

    def test_interrupted_registration_reaps_the_unreleased_direct_child(self):
        f = self.fixture
        probe = f.root / 'interrupted.py'
        probe.write_text("""
import importlib.util, os, sys, time
from pathlib import Path
from unittest.mock import patch
spec = importlib.util.spec_from_file_location('fixture_owner', sys.argv[1])
owner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(owner)
root, state = Path(sys.argv[2]), Path(sys.argv[3])
children = []
real_popen, real_atomic = owner.subprocess.Popen, owner.atomic
def popen(*args, **kwargs):
    child = real_popen(*args, **kwargs)
    children.append(child)
    return child
def interrupted(path, value):
    if path.name.endswith('.process.json'):
        raise owner.Refusal('controlled interruption before publication')
    return real_atomic(path, value)
deadline = time.monotonic() + float(os.environ['FAKE_TMUX_CLEANUP_SECONDS'])
with patch.object(owner.subprocess, 'Popen', popen), patch.object(owner, 'atomic', interrupted):
    try:
        owner.start(root, os.environ['FAKE_TMUX_PROCESS_SCOPE'], state, 'pane-child', '900', 'fixture', [], deadline)
    except owner.Refusal as error:
        assert 'controlled interruption' in str(error)
    else:
        raise AssertionError('missing interrupted publication refusal')
assert len(children) == 1 and children[0].returncode is not None
assert not list(root.glob('*.process.json'))
for path in root.glob('*.writer'):
    with owner.exclusive(path, owner.file_pin(path), deadline):
        pass
""")
        f.command('( exec 7<"$FAKE_TMUX_PROCESS_LEDGER/$FAKE_TMUX_PROCESS_SCOPE.scope-writers"; '
                  'exec 8<"$FAKE_TMUX_PROCESS_LEDGER/writers"; '
                  'python3 -B "$TESTS_DIR/fake-process-owner.py" writer '
                  '"$FAKE_TMUX_PROCESS_LEDGER" "$FAKE_TMUX_PROCESS_SCOPE" 8 7 && '
                  'python3 -B ' + shlex.quote(str(probe)) +
                  ' "$TESTS_DIR/fake-process-owner.py" "$FAKE_TMUX_PROCESS_LEDGER" "$FAKE_TMUX_STATE" )')
        self.assertFalse(list(f.ledger.glob('*.process.json')))
        f.finish()

if __name__ == "__main__":
    unittest.main()

"""Exercise the shipping notify door, real tmux and real gate; fake only the provider."""
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import time

repo, scratch = map(Path, sys.argv[1:3])
project_slug = sys.argv[3]
tmux = shutil.which("tmux")
assert tmux, "tmux is required; this regression must not skip"
socket = scratch / "tmux.sock"
bindir = scratch / "bin"
bindir.mkdir()
wrapper = bindir / "tmux"
wrapper.write_text(f"#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do case \"$1\" in -u) shift ;; -S) shift 2 ;; *) break ;; esac; done\nif [ -f {shlex.quote(str(scratch / 'refuse-native'))} ] && [ \"$1\" = send-keys ]; then echo 'native delivery refused by fixture' >&2; exit 42; fi\nexec {shlex.quote(tmux)} -u -S {shlex.quote(str(socket))} \"$@\"\n")
wrapper.chmod(0o755)
env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}",
           STORYHOOK_LOCK_DIR=str(scratch / "locks"),
           STORY_READY_PROCESS_PATTERN="[Pp]ython", STORY_PASTE_SETTLE_DELAY="0")
env.pop("STORYHOOK_MACHINE_LOCKS", None)
env.pop("STORY_AGENT", None)
gate = repo / "scripts/machine-lock.sh"
helper = repo / "plugins/story/bin/story.sh"
provider = scratch / "provider.py"
# The provider draws a composer the way the real ones do (SH-780): the glyph and
# its pad (Claude: U+276F and NBSP; Codex: U+203A and a space), then what was
# typed, cleared by the submit key. story.sh notify reads that row before it
# types and before it submits. Bracketed paste stays off: this provider reads
# every ESC byte as the native Escape key. Its gate's output goes to a log, as
# a real agent captures a tool's output instead of printing it into its
# composer row (where it would read as a draft).
provider.write_text('''import os,sys,subprocess,signal,time,tty
from pathlib import Path
tty.setraw(sys.stdin.fileno())
root=Path(sys.argv[1]); mode=sys.argv[2]
glyph='\\u276f\\u00a0' if sys.argv[4]=='claude' else '\\u203a '
message=b''
def draw(): os.write(1,b'\\r\\x1b[2K'+glyph.encode()+message)
os.write(1,b'\\r\\n'); draw()
gate_log=open(root/'gate.log','ab')
gate=subprocess.Popen(["bash",sys.argv[3],"gate","--","python3","-c", "import os,signal,time; from pathlib import Path; Path('writer').write_text(str(os.getpid())); signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(90)"], cwd=root, stdout=gate_log, stderr=gate_log)
(root/'holder').write_text(str(gate.pid))
(root/'ready').write_text(str(os.getpid()))
while True:
 c=os.read(sys.stdin.fileno(),1)
 if c==b'\\x1b':
  (root/'native').write_text('Escape')
  if mode=='abrupt':
   try: os.kill(gate.pid,signal.SIGKILL)
   except ProcessLookupError: pass
 elif c in (b'\\t',b'\\r'):
  (root/'submitted').write_bytes(message)
  message=b''
  draw()
 else:
  message+=c
  draw()
''')

def run(*args, **kwargs):
    return subprocess.run(args, env=env, cwd=scratch, capture_output=True,
                          text=True, timeout=25, **kwargs)

def wait_for(predicate, why, seconds=8):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(.03)
    raise AssertionError(why)

def alive(pid):
    result = run("ps", "-o", "stat=", "-p", str(pid))
    return bool(result.stdout.strip()) and not result.stdout.lstrip().startswith("Z")

waiter = unrelated = None
owned = {}
try:
    unrelated = subprocess.Popen(["bash", str(gate), "gate-unrelated", "--", "sleep", "90"],
                                 env=env, cwd=scratch, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                 start_new_session=True)
    for mode, identity in (("normal", "codex"), ("abrupt", "claude"), ("failed", "codex")):
        for name in ("ready", "holder", "writer", "native", "submitted"):
            (scratch / name).unlink(missing_ok=True)
        launch = shlex.join([sys.executable, str(provider), str(scratch), mode, str(gate), identity])
        started = run("tmux", "new-session", "-d", "-s", "test", "-n", "SH-1", launch)
        assert started.returncode == 0, started.stderr
        run("tmux", "set-window-option", "-t", "test:SH-1", "automatic-rename", "off", check=True)
        run("tmux", "set-window-option", "-t", "test:SH-1", "@storyhook-agent", identity, check=True)
        wait_for(lambda: (scratch / "writer").exists(), "provider gate did not start")
        writer = int((scratch / "writer").read_text())
        agent = int((scratch / "ready").read_text())
        holder = int((scratch / "holder").read_text())
        owned = {pid: run("ps", "-o", "lstart=", "-p", str(pid)).stdout.strip() for pid in (writer, holder)}
        # Unmarked panes refuse before any native key or cleanup side effect.
        run("tmux", "set-window-option", "-u", "-t", "test:SH-1", "@storyhook-agent", check=True)
        refused = json.loads(run("bash", str(helper), "notify", "SH-1", "--interrupt").stdout)
        assert refused.get("reason") == "pane-provider-unknown", refused
        assert alive(writer) and not (scratch / "native").exists()
        run("tmux", "set-window-option", "-t", "test:SH-1", "@storyhook-agent", identity, check=True)
        # A managed launch publishes an exact process incarnation after its
        # readiness signal. A window tag alone cannot authorize Python as a
        # provider; exercise the same production registration door as dispatch.
        identity_helper = repo / "plugins/story/lib/agent_identity.py"
        captured = json.loads(run(sys.executable, str(identity_helper), "capture", str(agent), check=True).stdout)
        pane = run("tmux", "display-message", "-p", "-t", "test:SH-1", "#{pane_id}", check=True).stdout.strip()
        registered = json.loads(run(sys.executable, str(identity_helper), "register", project_slug,
                                    "SH-1", "SH-1", str(scratch), pane, str(agent), identity,
                                    captured["identity"]["start"], check=True).stdout)
        assert registered.get("ok"), registered
        probe = ("import subprocess,sys; "
                 f"s=subprocess.run(['ps','-o','stat=','-p','{writer}'],capture_output=True,text=True).stdout.strip(); "
                 "sys.exit(71 if s and not s.startswith('Z') else 0)")
        waiter = subprocess.Popen(["bash", str(gate), "--max-wait", "15", "gate", "--", "python3", "-c", probe],
                                  env=env, cwd=scratch, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if mode == "failed":
            (scratch / "refuse-native").touch()
        result = run("bash", str(helper), "notify", "SH-1", "--interrupt")
        answer = json.loads(result.stdout)
        if mode == "failed":
            assert answer.get("reason") == "interruption-failed", answer
            assert alive(writer) and alive(agent), "a refused key must preserve the session and tools"
            assert not (scratch / "native").exists() and not (scratch / "submitted").exists()
            evidence = list((scratch / "locks").glob("gate.*.lock/interrupt/processes.json"))
            assert evidence and str(writer) in json.loads(evidence[0].read_text())["processes"]
            _, diagnostic = waiter.communicate(timeout=20)
            assert waiter.returncode != 0 and "interruption cleanup incomplete" in diagnostic, diagnostic
            break
        assert answer.get("ok"), (answer, result.stderr)
        assert answer.get("target"), "interrupt acknowledgement must bind the session"
        wait_for(lambda: (scratch / "native").exists(), "native Escape was not delivered")
        assert not (scratch / "submitted").exists(), "block must send no prompt or submit key"
        assert not alive(writer), "gate child survived acknowledged interruption"
        assert not alive(holder), "gate owner survived acknowledged interruption"
        assert alive(agent), "provider session was killed"
        _, diagnostic = waiter.communicate(timeout=20)
        assert waiter.returncode == 0, "another holder entered over surviving writers: " + diagnostic
        assert unrelated.poll() is None, "an unrelated gate holder was stopped"
        next_holder = run("bash", str(gate), "--max-wait", "2", "gate", "--", "true")
        assert next_holder.returncode == 0, next_holder.stderr
        prompt = "Your story experienced a temporary block, which has been lifted. The environment and dev branch may have changed. Please reread your story, its comments, and its relationships to understand the changes, and adjust your work accordingly. If the change is significant, resetting & rebasing the worktree and restarting the story may be appropriate."
        resume = json.loads(run("bash", str(helper), "notify", "SH-1", prompt,
                                "--expected-target", answer["target"]).stdout)
        assert resume.get("ok"), resume
        wait_for(lambda: (scratch / "submitted").exists(), "resume was not submitted")
        assert (scratch / "submitted").read_text() == prompt, "resume prompt changed"
        # A replaced pane must not receive a resume intended for its predecessor.
        replacement = shlex.join([sys.executable, "-c", "import time; time.sleep(90)"])
        run("tmux", "respawn-pane", "-k", "-t", "test:SH-1", replacement, check=True)
        wait_for(lambda: "python" in run("tmux", "display-message", "-p", "-t", "test:SH-1", "#{pane_current_command}").stdout.lower(), "replacement provider did not start")
        stale = json.loads(run("bash", str(helper), "notify", "SH-1", "resume",
                              "--expected-target", answer["target"]).stdout)
        assert stale.get("reason") == "pane-changed" and not stale.get("ok"), stale
        run("tmux", "kill-server", check=True)
        wait_for(lambda: run("tmux", "has-session", "-t", "test").returncode != 0, "test tmux server did not close")
finally:
    for child in (waiter, unrelated):
        if child is not None and child.poll() is None:
            child.terminate()
            child.wait(timeout=10)
    # Exact fixture-owned identities only, including writers from red/mutant runs.
    for pid, started in owned.items():
        if started and run("ps", "-o", "lstart=", "-p", str(pid)).stdout.strip() == started:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    run("tmux", "kill-server")

"""Prove that a real watchdog timer cannot outlive inherited workspace authority."""

import fcntl
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def processes(patience):
    """Read parentage and execution state without guessing a timer PID."""
    output = subprocess.check_output(
        ['ps', '-axo', 'pid=,ppid=,stat=,comm='], text=True, timeout=patience)
    result = {}
    for line in output.splitlines():
        fields = line.split(None, 3)
        if len(fields) == 4:
            result[int(fields[0])] = (int(fields[1]), fields[2], fields[3])
    return result


def stop_owned_timer(wrapper, patience):
    """Freeze an actual grandchild sleep while its parent watchdog owns it."""
    deadline = time.monotonic() + patience
    while (remaining := deadline - time.monotonic()) > 0:
        table = processes(remaining)
        for pid, (parent, state, executable) in table.items():
            owner = table.get(parent)
            if (owner and owner[0] == wrapper.pid and 'Z' not in state
                    and Path(executable).name == 'sleep'):
                try:
                    os.kill(pid, signal.SIGSTOP)
                except ProcessLookupError:
                    continue
                confirmed = False
                try:
                    remaining = deadline - time.monotonic()
                    if remaining > 0:
                        stopped = processes(remaining).get(pid)
                        confirmed = bool(stopped and 'T' in stopped[1])
                        if confirmed:
                            return pid
                finally:
                    # If observation fails, the caller never receives this PID.
                    # Do not strand its timer in STOP while cleanup waits for it.
                    if not confirmed:
                        try:
                            os.kill(pid, signal.SIGCONT)
                        except ProcessLookupError:
                            pass
        assert wrapper.poll() is None, 'wrapper exited before its timer was observed'
        time.sleep(.01)
    raise AssertionError(f'wrapper {wrapper.pid}: real watchdog timer not observed within {patience}s')


def exercise(script, root, mode, patience, stopped_hold):
    """Keep a real inherited flock held until normal or signalled cleanup ends."""
    root.mkdir()
    environment = dict(os.environ)
    for key in ('STORYHOOK_MACHINE_LOCKS', 'STORYHOOK_GATE_PROGRESS',
                'STORYHOOK_ACTIVITY_LOG_DIR', 'STORYHOOK_GATE_PROGRESS_ACTIVITY_PATH'):
        environment.pop(key, None)
    environment['STORYHOOK_LOCK_DIR'] = str(root / 'locks')
    workspace = root / 'workspace.lock'
    timer = None
    with workspace.open('a+') as owner, (root / 'output').open('w+') as output:
        fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
        # The shell uses descriptors 3 and 9 itself. An inherited workspace
        # can have any other descriptor; choose one outside those reservations.
        inherited = fcntl.fcntl(owner, fcntl.F_DUPFD, 20)
        try:
            wrapper = subprocess.Popen(
                ['bash', str(script), '--max-idle', str(math.ceil(2 * patience + stopped_hold)),
                 'fixture', '--',
                 'bash', '-c', 'IFS= read -r release'],
                cwd=root, env=environment, stdin=subprocess.PIPE,
                stdout=output, stderr=output, pass_fds=(inherited,))
        finally:
            os.close(inherited)
        owner.close()
        try:
            timer = stop_owned_timer(wrapper, patience)
            if mode == 'normal':
                wrapper.stdin.write(b'release\n')
                wrapper.stdin.flush()
            else:
                wrapper.send_signal(signal.SIGTERM)
            try:
                status = wrapper.wait(timeout=stopped_hold)
            except subprocess.TimeoutExpired:
                pass
            else:
                raise AssertionError(
                    f'{mode}: wrapper exited {status} over stopped watchdog timer {timer}')
            with workspace.open('a+') as competitor:
                try:
                    fcntl.flock(competitor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    pass
                else:
                    raise AssertionError(f'{mode}: live watchdog lost workspace authority')
            os.kill(timer, signal.SIGCONT)
            status = wrapper.wait(timeout=patience)
            assert status == (0 if mode == 'normal' else -signal.SIGTERM), (mode, status)
            with workspace.open('a+') as competitor:
                fcntl.flock(competitor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            remaining = processes(patience).get(timer)
            assert not remaining or 'Z' in remaining[1], (mode, timer, remaining)
            timer = None
            print(f'{mode}: timer reaped before wrapper exit; workspace immediately available')
        finally:
            if timer is not None:
                try:
                    os.kill(timer, signal.SIGCONT)
                except ProcessLookupError:
                    pass
                deadline = time.monotonic() + patience
                while True:
                    allowance = deadline - time.monotonic()
                    assert allowance > 0, 'owned timer survived regression cleanup'
                    remaining = processes(allowance).get(timer)
                    if not remaining or 'Z' in remaining[1]:
                        break
                    time.sleep(.02)
            if wrapper.poll() is None:
                wrapper.terminate()
                wrapper.wait(timeout=patience)
            wrapper.stdin.close()


if __name__ == '__main__':
    script, scratch = map(Path, sys.argv[1:3])
    patience, stopped_hold = map(float, sys.argv[3:5])
    for mode in ('normal', 'signal'):
        exercise(script, scratch / mode, mode, patience, stopped_hold)

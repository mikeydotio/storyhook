"""Pin approval watchers to one endpoint and kernel process incarnation."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys

sys.dont_write_bytecode = True

import agent_identity
import probe_budget
from process_identity import process_identity
import tmux_client

OPTION = '@storyhook-plan-watch-v1'


def capture(socket, pane, pid):
    """Capture the dispatcher's ready pane; never follow a stale private binding."""
    client = tmux_client.client(socket)
    client.require_binding(socket)
    process = process_identity(int(pid))
    return dict(socket=client.target['endpoint'], pane=pane, process=process)


def command(binding, args):
    """Recheck the native incarnation immediately before a pinned tmux request."""
    if '-t' not in args or args[args.index('-t') + 1] != binding['pane']:
        raise RuntimeError('approval watcher target differs from captured pane')
    if process_identity(binding['process']['pid']) != binding['process']:
        raise RuntimeError('approval watcher process was replaced')
    if 'identity' in binding:
        from restoration import validate_provider
        validate_provider(binding['identity'])
    if args and args[0] == 'send-keys' and binding.get('story'):
        # Labels may change after rearm. Recheck policy at the input boundary.
        raw = agent_identity.run(os.environ.get('STORY_BIN', 'story'), 'show', binding['story'], '--json')
        result = json.loads(raw)['story']
        story = result.get('story', result)
        if story.get('state') != 'in-progress' or any(label in story.get('labels', []) for label in ('no-auto', 'human-only')):
            raise RuntimeError('current story policy forbids automatic plan approval')
    client = tmux_client.client(binding['socket'])
    client.require_binding(binding['socket'])
    return agent_identity.run(*client.arguments(['-u', '-S', binding['socket'], *args], binding=True, socket=binding['socket']))


def schedule(identity, metadata):
    """Rearm existing autonomous metadata; native callers first check story policy."""
    if metadata is None or metadata.get('autonomy_mode') not in ('auto', 'full-auto'):
        return False
    binding = dict(socket=identity['socket'], pane=identity['pane'], process=identity['process'])
    if 'restored' in identity:
        binding['identity'] = identity
    binding['common'] = identity['common']
    binding['story'] = identity['story']
    encoded = json.dumps(binding, sort_keys=True, separators=(',', ':'))
    current = command(binding, ['show-options', '-w', '-qv', '-t', binding['pane'], OPTION])
    if current:
        status = json.loads(current)
        if status.get('binding') == binding and status.get('complete') is True:
            return False
    argv = [sys.executable, str(Path(__file__).resolve()), 'watch', encoded,
            str(Path(__file__).resolve().parents[1] / 'hooks/full-auto.sh'), metadata['provider'], '0']
    auto = 'STORYHOOK_FULL_AUTO' if metadata['autonomy_mode'] == 'full-auto' else 'STORYHOOK_AUTO'
    shell = shlex.join(['env', auto + '=' + identity['story'], *argv])
    command(binding, ['run-shell', '-b', '-t', binding['pane'], shell])
    return True


def watch(binding, hook, provider, limit):
    """One live watcher per physical binding, with a durable completion marker."""
    encoded = json.dumps(binding, sort_keys=True, separators=(',', ':'))
    key = hashlib.sha256(encoded.encode()).hexdigest()
    directory = Path(binding['common']) / 'storyhook/approval-watchers'
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(str(directory / key), os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
        with tmux_client.operation():
            current = command(binding, ['show-options', '-w', '-qv', '-t', binding['pane'], OPTION])
            if current and json.loads(current) == dict(binding=binding, complete=True):
                return
        env = dict(os.environ, STORY_APPROVAL_BINDING=encoded, TMUX=binding['socket'] + ',0,0')
        result = subprocess.run(['bash', hook, '--approve-' + provider + '-plan', binding['pane'],
                                 str(binding['process']['pid']), limit], env=env, check=False)
        if result.returncode:
            raise RuntimeError('approval watcher failed: ' + str(result.returncode))



def main():
    """The long-lived wrapper never extends an individual terminal probe budget."""
    try:
        operation, *args = sys.argv[1:]
        if operation == 'watch':
            watch(json.loads(args[0]), *args[1:])
        else:
            with tmux_client.operation():
                if operation == 'capture':
                    print(json.dumps(capture(*args), sort_keys=True))
                elif operation == 'complete':
                    binding = json.loads(os.environ['STORY_APPROVAL_BINDING'])
                    command(binding, ['set-option', '-w', '-t', binding['pane'], OPTION,
                                      json.dumps(dict(binding=binding, complete=True), sort_keys=True)])
                elif operation == 'tmux':
                    binding = json.loads(os.environ['STORY_APPROVAL_BINDING'])
                    print(command(binding, args))
                else:
                    raise RuntimeError('unknown approval binding operation')
        return 0
    except (OSError, ValueError, KeyError, RuntimeError, agent_identity.IdentityError, subprocess.TimeoutExpired) as error:
        print('approval watcher: ' + str(error), file=sys.stderr)
        return 1


if __name__ == '__main__': sys.exit(main())

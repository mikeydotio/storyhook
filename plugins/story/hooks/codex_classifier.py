"""Bounded, capability-free Luna classification through the installed Codex CLI."""

import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading

ROOT = Path(__file__).resolve().parent
MODEL = 'gpt-5.6-luna'
# This version's outgoing request is covered by the opt-in provider probe. A new
# runtime must be measured before it can acquire the authority to approve plans.
SUPPORTED_VERSION = 'codex-cli 0.154.0'
MAX_OUTPUT = 1024 * 1024
DISABLED_FEATURES = (
    'hooks', 'shell_tool', 'unified_exec', 'multi_agent', 'apps', 'browser_use',
    'computer_use', 'image_generation', 'goals', 'memories', 'plugins',
    'view_image', 'sleep_tool', 'skill_search', 'tool_suggest',
)


def run_process(argv, *, timeout, env=None, cwd=None, text=''):
    """Run an owned process group with a deadline and bounded returned output."""
    def terminated(signum, frame):
        """Unwind owned child cleanup when the provider terminates its hook."""
        raise RuntimeError('hook terminated by provider')

    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=stdout,
                                 stderr=stderr, env=env, cwd=cwd, start_new_session=True)
        previous = None
        try:
            if threading.current_thread() is threading.main_thread():
                previous = signal.signal(signal.SIGTERM, terminated)
            child.communicate(text.encode(), timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError(f'{Path(argv[0]).name} exceeded {timeout}s deadline') from exc
        finally:
            # Covers timeout, parent interruption, and descendants retained after
            # the original process exits. Only this invocation owns this group.
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait()
            if previous is not None:
                signal.signal(signal.SIGTERM, previous)
        if stdout.tell() > MAX_OUTPUT or stderr.tell() > MAX_OUTPUT:
            raise RuntimeError(f'{Path(argv[0]).name} exceeded output limit')
        stdout.seek(0)
        stderr.seek(0)
        out, err = stdout.read().decode(), stderr.read().decode()
        if child.returncode:
            raise RuntimeError(f'{Path(argv[0]).name} exited {child.returncode}: {err[-1000:]}')
        return out


def classifier_environment(env):
    """Keep login discovery and transport settings without parent lane identity."""
    allowed = ('HOME', 'PATH', 'CODEX_HOME', 'TMPDIR', 'LANG', 'LC_ALL',
               'HTTPS_PROXY', 'HTTP_PROXY', 'ALL_PROXY', 'NO_PROXY',
               'SSL_CERT_FILE', 'SSL_CERT_DIR')
    return {key: env[key] for key in allowed if key in env}


def classifier_command(workdir):
    """Build the exact probed no-tools configuration, preserving the login home."""
    command = ['codex', 'exec', '--ignore-user-config', '--ephemeral',
               '--skip-git-repo-check', '--strict-config', '--json', '-C', str(workdir),
               '-s', 'read-only', '-m', MODEL,
               '--output-schema', str(ROOT / 'plan-classification.schema.json')]
    config = {
        'approval_policy': 'never', 'project_doc_max_bytes': 0,
        'web_search': 'disabled', 'agents.enabled': False,
        'skills.include_instructions': False,
        'tools.experimental_request_user_input.enabled': False,
        'tools.update_plan.enabled': False,
        'model_reasoning_effort': 'low',
        'model_catalog_json': str(ROOT / 'luna-classifier.json'),
        'model_instructions_file': str(ROOT / 'plan-classifier.md'),
    }
    for key, value in config.items():
        command += ['-c', key + '=' + json.dumps(value)]
    for feature in DISABLED_FEATURES:
        command += ['--disable', feature]
    return command + ['-']


def parse_response(output):
    """Accept a completed text-only turn; never accept errors or tool events."""
    completed = False
    answer = None
    for line in output.splitlines():
        event = json.loads(line)
        if not isinstance(event, dict):
            raise RuntimeError('Luna emitted a non-object event')
        kind = event.get('type')
        if kind == 'turn.completed':
            completed = True
        elif kind in ('thread.started', 'turn.started'):
            continue
        elif kind in ('item.started', 'item.updated', 'item.completed'):
            item = event.get('item', {})
            if not isinstance(item, dict):
                raise RuntimeError('Luna emitted a non-object item')
            if item.get('type') not in ('agent_message', 'reasoning'):
                raise RuntimeError('Luna emitted an error or unexpected tool event')
            if kind == 'item.completed' and item['type'] == 'agent_message':
                answer = json.loads(item['text'])
        else:
            raise RuntimeError(f'Luna emitted unexpected event {kind!r}')
    if not completed or answer is None:
        raise RuntimeError('Luna returned no completed classification')
    return answer


def classify(message):
    """Classify one message using existing Codex authentication and no tools."""
    env = classifier_environment(os.environ)
    version = run_process(['codex', '--version'], timeout=2, env=env).strip()
    if version != SUPPORTED_VERSION:
        raise RuntimeError(f'unverified classifier runtime {version!r}; expected {SUPPORTED_VERSION}')
    # A new non-project cwd and project_doc_max_bytes=0 exclude repository policy.
    # Codex still loads global user AGENTS independently; we preserve that policy.
    with tempfile.TemporaryDirectory(prefix='storyhook-classifier-', dir='/tmp') as workdir:
        output = run_process(classifier_command(workdir), timeout=20, env=env,
                             cwd=workdir, text=json.dumps({'assistant_message': message}))
    return parse_response(output)

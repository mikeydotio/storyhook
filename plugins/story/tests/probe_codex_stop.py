"""Opt-in, loopback-only Codex 0.154.0 wire/Stop probe; never contacts a model API.

Run through probe-codex-stop.sh so every story operation uses an isolated store.
Model responses are data fixtures; the CLI, classifier, hooks, and tracker are real.
"""
import http.server
import json
import os
from pathlib import Path
import shutil
import queue
import subprocess
import sys
import tempfile
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'hooks'))
import codex_classifier as classifier

PLAN = ('Implementation plan:\n1. Post this approved plan verbatim as a story comment.\n'
        '2. Fix the parser and add its regression test.\n3. Run impacted tests and commit.\n'
        'Reply Approved to start implementation.')
ANSWER = {'decision': 'approve_plan', 'evidence': 'Reply Approved to start implementation.'}


def run_plan_turn(command, repo, env):
    """Exercise a genuine Plan-mode turn through the installed app-server API."""
    flags = []
    for index, arg in enumerate(command):
        if arg in ('-c', '--enable', '--disable'):
            flags += command[index:index + 2]
    with tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen([command[0], 'app-server', '--stdio'] + flags,
                                   env=env, cwd=repo, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=stderr, text=True)
        incoming = queue.Queue()

        def read_events():
            """Queue actual provider messages without blocking the probe deadline."""
            for line in process.stdout:
                incoming.put(json.loads(line))

        reader = threading.Thread(target=read_events, daemon=True)
        reader.start()
        events = []
        deadline = time.monotonic() + 45

        def send(method, params, request_id=None):
            """Send one provider protocol request."""
            message = {'method': method, 'params': params}
            if request_id is not None:
                message['id'] = request_id
            process.stdin.write(json.dumps(message) + '\n')
            process.stdin.flush()

        def until(predicate):
            """Wait for a protocol result with one overall deadline."""
            while True:
                event = incoming.get(timeout=max(0.01, deadline - time.monotonic()))
                events.append(event)
                assert 'error' not in event, event
                if predicate(event):
                    return event

        try:
            send('initialize', {'clientInfo': {'name': 'sh676-probe', 'version': '1'},
                                'capabilities': {'experimentalApi': True}}, 1)
            until(lambda event: event.get('id') == 1)
            send('initialized', {})
            send('thread/start', {'cwd': repo, 'model': classifier.MODEL,
                                 'sandbox': 'read-only', 'approvalPolicy': 'never',
                                 'config': {'bypass_hook_trust': True}}, 2)
            started = until(lambda event: event.get('id') == 2)
            send('turn/start', {'threadId': started['result']['thread']['id'],
                               'input': [{'type': 'text', 'text': 'Present the implementation plan and wait for approval.'}],
                               'collaborationMode': {'mode': 'plan', 'settings': {
                                   'model': classifier.MODEL, 'reasoning_effort': 'low',
                                   'developer_instructions': None}}}, 3)
            until(lambda event: event.get('method') == 'turn/completed')
            return json.dumps(events)
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            reader.join(timeout=2)
            stderr.seek(0)
            if process.returncode not in (0, -15):
                print(stderr.read().decode(), file=sys.stderr)


def run_probe(repo, story_id, mode='default', structured=False):
    """Drive the installed provider against a model endpoint owned by this test."""
    real_codex = shutil.which('codex')
    assert real_codex, 'codex is required for the opt-in provider probe'
    requests = []

    class Model(http.server.BaseHTTPRequestHandler):
        """Supply deterministic model data and capture actual wire capabilities."""

        def log_message(self, *args):
            """Suppress request logging; assertions report failures."""

        def do_POST(self):
            """Return a plan, its classification, or a final continuation receipt."""
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append(request)
            is_classifier = any(i.get('role') == 'user' and any(c.get('text', '').startswith('{"assistant_message":') for c in i.get('content', [])) for i in request['input'])
            if is_classifier:
                text = json.dumps(ANSWER)
            elif any('Do not implement or write files while in Plan mode' in str(item) for item in request['input']):
                text = '<proposed_plan>\n' + PLAN + '\n</proposed_plan>'
            elif any('approved automatically' in str(item) for item in request['input']):
                text = 'CONTINUED_AFTER_AUTOMATIC_APPROVAL'
            else:
                text = (json.dumps({'type': 'storyhook.implementation-plan', 'version': 1,
                                    'story_id': story_id, 'plan': PLAN}) if structured else PLAN)
            msg = {'id': 'msg_probe', 'type': 'message', 'role': 'assistant',
                   'content': [{'type': 'output_text', 'text': text}]}
            events = [
                {'type': 'response.created', 'response': {'id': 'resp_probe'}},
                {'type': 'response.output_item.added', 'output_index': 0, 'item': msg},
                {'type': 'response.output_item.done', 'output_index': 0, 'item': msg},
                {'type': 'response.completed', 'response': {'id': 'resp_probe', 'status': 'completed',
                 'output': [msg], 'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}}},
            ]
            data = ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n'
                           for event in events).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    with tempfile.TemporaryDirectory(prefix='sh676-provider-', dir='/tmp') as tmp:
        root = Path(tmp)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            flags = ['-c', 'model_provider="sh676_probe"', '-c', 'model_providers.sh676_probe.name="probe"',
                     '-c', f'model_providers.sh676_probe.base_url="http://127.0.0.1:{server.server_port}/v1"',
                     '-c', 'model_providers.sh676_probe.wire_api="responses"',
                     '-c', 'model_providers.sh676_probe.requires_openai_auth=false',
                     '--disable', 'enable_request_compression']
            # Substitute only the model endpoint for the real child Codex command.
            wrapper = root / 'codex'
            wrapper.write_text('#!' + sys.executable + '\nimport os,sys\n'
                               + f'args=[{real_codex!r}]+sys.argv[1:]\n'
                               + f'args += {flags!r} if "exec" in sys.argv else []\n'
                               + 'os.execv(args[0],args)\n')
            wrapper.chmod(0o700)
            env = os.environ | {'PATH': str(root) + os.pathsep + os.environ['PATH'],
                                'CODEX_HOME': str(root / 'codex-home'),
                                'STORYHOOK_AUTO': story_id}
            Path(env['CODEX_HOME']).mkdir()
            # Plant project policy that would contaminate classification if it leaked.
            Path(repo, 'AGENTS.md').write_text('PROJECT_POLICY_MUST_NOT_REACH_CLASSIFIER\n')
            command = classifier.classifier_command(repo)
            command[0] = real_codex
            command.remove('--ephemeral')
            index = command.index('hooks')
            del command[index - 1:index + 1]
            index = command.index('--output-schema')
            del command[index:index + 2]
            hook = 'bash "' + str(classifier.ROOT / 'codex-stop.sh') + '"'
            command = command[:-1] + flags + ['--dangerously-bypass-hook-trust', '--enable', 'hooks',
                '-c', 'hooks.Stop=[{hooks=[{type="command",command=' + json.dumps(hook) + ',timeout=50}]}]',
                'Present the implementation plan and wait for approval.']
            result = (run_plan_turn(command, repo, env) if mode == 'plan'
                      else classifier.run_process(command, timeout=45, env=env, cwd=repo))
            expected = '<proposed_plan>' if mode == 'plan' else 'CONTINUED_AFTER_AUTOMATIC_APPROVAL'
            assert expected in result, result + '\nJOURNALS: ' + repr([(str(p),p.read_text()) for p in Path(env['CODEX_HOME']).rglob('*.storyhook-plan-approval')])
            classified = [r for r in requests if any(i.get('role') == 'user' and any(c.get('text', '').startswith('{"assistant_message":') for c in i.get('content', [])) for i in r['input'])]
            assert len(classified) == (0 if structured else 1), len(classified)
            for request in classified:
                assert not request.get('tools'), request.get('tools')
                assert all(not i.get('tools') for i in request['input'] if i.get('type') == 'additional_tools')
                assert 'PROJECT_POLICY_MUST_NOT_REACH_CLASSIFIER' not in json.dumps(request)
                assert 'skills_instructions' not in json.dumps(request)
            journals = list(Path(env['CODEX_HOME']).rglob('*.storyhook-plan-approval'))
            assert len(journals) == 1, journals
            saved = json.loads(journals[0].read_text())
            assert saved['story_id'] == story_id, saved
            assert saved['mode'] == mode, saved
            assert len(requests) == (2 if structured else 3), len(requests)
            assert saved['source'] == ('structured' if structured else 'prose'), saved
            print(f'PASS ({mode}, structured={structured}): production Stop and native continuation; '
                  f'{len(requests)} requests, {len(classified)} classifier calls')
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == '__main__':
    run_probe(sys.argv[1], sys.argv[2])
    run_probe(sys.argv[1], sys.argv[2], 'plan')
    run_probe(sys.argv[1], sys.argv[2], structured=True)
    run_probe(sys.argv[1], sys.argv[2], 'plan', structured=True)

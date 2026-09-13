"""Opt-in SH-711 Codex compaction probe with isolated model HTTP and hook logs.

Run: python3 plugins/story/tests/probe_context_compaction.py
Native Stop: python3 plugins/story/tests/probe_context_compaction.py --native-stop
No operator sessions, StoryHook store, credentials, or trust bypass are used.
"""

import http.server
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time


def run_probe(native_stop=False):
    """Measure compaction and provider hook identities in an owned app server."""
    executable = shutil.which('codex')
    if not executable:
        raise RuntimeError('installed Codex is required')
    with tempfile.TemporaryDirectory(prefix='sh711-compact-probe-', dir='/tmp') as directory:
        root = Path(directory)
        hook_events = root / 'hook-events.jsonl'
        wire_requests = []
        release_initial = threading.Event()
        process_events = root / 'story-processes.jsonl'
        hook_states = root / 'hook-states.jsonl'
        current_mode = {'mode': 'default'}
        fake_story = root / 'fake_story.py'
        fake_story.write_text('import json,sys\n'
                              + 'assert sys.argv[1:]==["continuation","ack","SH-1","request-1"]\n'
                              + 'print(json.dumps({"result":"ok","status":"acknowledged"}))\n')
        hook = root / 'record_hook.py'
        hook.write_text('import json,sys\n'
                        + 'payload=json.load(sys.stdin)\n'
                        + f'with open({str(hook_events)!r}, "a") as out:\n'
                        + ' out.write(json.dumps(payload)+"\\n")\n'
                        + 'print("{}")\n')
        if native_stop:
            hooks = Path(__file__).resolve().parents[1] / 'hooks'
            hook.write_text('import json,sys,os\nfrom pathlib import Path\n'
                            + f'sys.path.insert(0,{str(hooks)!r})\n'
                            + 'import codex_stop,codex_classifier\n'
                            + 'payload=json.load(sys.stdin)\n'
                            + f'with open({str(hook_events)!r},"a") as out: out.write(json.dumps(payload)+"\\n")\n'
                            + 'transcript=Path(payload["transcript_path"]).read_text()\n'
                            + f'with open({str(hook_states)!r},"a") as out: out.write(json.dumps({{"event":payload["hook_event_name"],"message":payload.get("last_assistant_message"),"correction_in_transcript":"SH711_OPERATOR_CORRECTION" in transcript}})+"\\n")\n'
                            + 'def external(argv,**kwargs):\n'
                            + f' with open({str(process_events)!r},"a") as out: out.write(json.dumps({{"argv":argv,"text":kwargs.get("text")}})+"\\n")\n'
                            + ' if "continuation" in argv: return json.dumps({"result":"ok","native_feedback":True,"continuation":{"id":"request-1","story_id":"SH-1","status":"awaiting-ack","phase":"native-continuation"}})\n'
                            + ' if "session-eligibility" in argv: return json.dumps({"result":"ok","session_eligibility":{"schema_version":1,"story_id":"SH-1","eligible":True,"reason":"eligible"}})\n'
                            + ' if argv[:2]==["codex","--version"]: return codex_classifier.SUPPORTED_VERSION\n'
                            + ' if argv[:2]==["codex","exec"]: return json.dumps({"type":"item.completed","item":{"type":"agent_message","text":json.dumps({"decision":"other","evidence":""})}})+"\\n"+json.dumps({"type":"turn.completed"})\n'
                            + ' raise RuntimeError("unexpected external command "+repr(argv))\n'
                            + 'codex_stop.run_process=external\n'
                            + 'codex_classifier.run_process=external\n'
                            + 'print(json.dumps(codex_stop.handle(payload,os.environ) if payload["hook_event_name"]=="Stop" else {}))\n')

        class Model(http.server.BaseHTTPRequestHandler):
            """Replace model responses only; execute provider behavior unchanged."""

            def log_message(self, *args):
                """Keep output focused on measured protocol evidence."""

            def do_POST(self):
                """Serve a deterministic text response or remote compact response."""
                request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                wire_requests.append({'path': self.path, 'body': request})
                text = json.dumps({
                    'type': 'storyhook.session-handoff', 'version': 1,
                    'story_id': 'SH-1', 'kind': 'context',
                    'evidence': {'context': 'Context limit reached in the isolated probe.',
                                 'outstanding_work': 'Continue the approved isolated probe.'},
                })
                user_texts = [content.get('text', '') for item in request.get('input', [])
                              if item.get('role') == 'user'
                              for content in item.get('content', [])]
                if user_texts and user_texts[-1] == 'SH711_CONTINUE_AFTER_COMPACTION':
                    text = 'SH711_RECEIVING_SESSION_ACKNOWLEDGED'
                elif user_texts and user_texts[-1] == 'SH711_OPERATOR_CORRECTION':
                    text = 'SH711_CORRECTION_ACKNOWLEDGED'
                message = {'id': 'msg_probe', 'type': 'message', 'role': 'assistant',
                           'content': [{'type': 'output_text', 'text': text}]}
                if native_stop:
                    if user_texts[-1:] == ['Emit the context handoff.']:
                        if not release_initial.wait(10):
                            raise RuntimeError('fixture queue was not populated')
                    has_native_feedback = any('StoryHook recorded context handoff' in value
                                              for value in user_texts)
                    ack_outputs = [item for item in request.get('input', [])
                                   if item.get('type') == 'function_call_output'
                                   and item.get('call_id') == 'call_ack']
                    if user_texts[-1:] == ['SH711_OPERATOR_CORRECTION']:
                        message['content'][0]['text'] = 'SH711_CORRECTION_ACKNOWLEDGED'
                    elif ack_outputs:
                        assert 'acknowledged' in json.dumps(ack_outputs), ack_outputs
                        message['content'][0]['text'] = 'SH711_NATIVE_RECEIVING_SESSION_ACKNOWLEDGED'
                    elif has_native_feedback and current_mode['mode'] == 'plan':
                        message['content'][0]['text'] = 'SH711_PLAN_CONTINUES_READ_ONLY_REQUEST_PENDING'
                    elif has_native_feedback:
                        message = {'type': 'function_call', 'id': 'fc_ack', 'call_id': 'call_ack',
                                   'name': 'exec_command', 'arguments': json.dumps({
                                       'cmd': sys.executable + ' ' + str(fake_story)
                                              + ' continuation ack SH-1 request-1',
                                       'max_output_tokens': 200})}
                if self.path.endswith('/compact'):
                    data = json.dumps({'id': 'cmp_probe', 'object': 'response.compaction',
                                       'output': [message]}).encode()
                    content_type = 'application/json'
                else:
                    events = [
                        {'type': 'response.created', 'response': {'id': 'resp_probe'}},
                        {'type': 'response.output_item.added', 'output_index': 0, 'item': message},
                        {'type': 'response.output_item.done', 'output_index': 0, 'item': message},
                        {'type': 'response.completed', 'response': {
                            'id': 'resp_probe', 'status': 'completed', 'output': [message],
                            'usage': {'input_tokens': 10, 'output_tokens': 10, 'total_tokens': 20}}},
                    ]
                    data = ''.join('event: ' + event['type'] + '\ndata: '
                                   + json.dumps(event) + '\n\n' for event in events).encode()
                    content_type = 'text/event-stream'
                self.send_response(200)
                self.send_header('Content-Type', content_type)
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
        server_thread = threading.Thread(target=server.serve_forever, daemon=True)
        server_thread.start()
        provider_home = root / 'provider-home'
        provider_home.mkdir()
        env = {key: value for key, value in os.environ.items()
               if key in ('PATH', 'LANG', 'LC_ALL', 'TMPDIR')}
        env['HOME'] = str(root)
        env['CODEX_HOME'] = str(provider_home)
        if native_stop:
            env['STORYHOOK_AUTO'] = 'SH-1'
        settings = {
            'model': 'gpt-5.6-luna', 'model_provider': 'sh711_probe',
            'model_providers.sh711_probe.name': 'SH-711 isolated probe',
            'model_providers.sh711_probe.base_url': f'http://127.0.0.1:{server.server_port}/v1',
            'model_providers.sh711_probe.wire_api': 'responses',
            'model_providers.sh711_probe.requires_openai_auth': False,
            'approval_policy': 'never', 'sandbox_mode': 'read-only',
            'project_doc_max_bytes': 0,
        }
        command = [executable, 'app-server', '--stdio', '--enable', 'hooks',
                   '--disable', 'enable_request_compression']
        for key, value in settings.items():
            command += ['-c', key + '=' + json.dumps(value)]
        for event in ('Stop', 'PreCompact', 'PostCompact', 'SessionStart'):
            command += ['-c', 'hooks.' + event + '=[{hooks=[{type="command",command='
                        + json.dumps(sys.executable + ' ' + str(hook)) + ',timeout=5}]}]']
        messages = []
        incoming = queue.Queue()
        with tempfile.TemporaryFile() as errors:
            process = subprocess.Popen(command, cwd=root, env=env, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=errors, text=True)

            def read_events():
                """Read actual app-server events independently of request deadlines."""
                for line in process.stdout:
                    incoming.put(json.loads(line))

            reader = threading.Thread(target=read_events, daemon=True)
            reader.start()

            def send(method, params, identifier=None):
                """Send one protocol request to this probe's provider only."""
                payload = {'method': method, 'params': params}
                if identifier is not None:
                    payload['id'] = identifier
                process.stdin.write(json.dumps(payload) + '\n')
                process.stdin.flush()

            def until(predicate):
                """Observe bounded protocol completion without timer-only guesses."""
                deadline = time.monotonic() + 25
                while True:
                    item = incoming.get(timeout=max(0.01, deadline - time.monotonic()))
                    messages.append(item)
                    if 'error' in item:
                        raise RuntimeError(json.dumps(item))
                    if predicate(item):
                        return item

            try:
                send('initialize', {'clientInfo': {'name': 'sh711-probe', 'version': '1'},
                                    'capabilities': {'experimentalApi': True}}, 1)
                until(lambda item: item.get('id') == 1)
                send('initialized', {})
                send('hooks/list', {'cwds': [str(root)]}, 2)
                hook_inventory = until(lambda item: item.get('id') == 2)
                print(json.dumps({'hook_inventory': hook_inventory['result']}))
                reviewed = hook_inventory['result']['data'][0]['hooks']
                expected_command = sys.executable + ' ' + str(hook)
                assert len(reviewed) == 4 and all(
                    entry['command'] == expected_command
                    and entry['source'] == 'sessionFlags'
                    and entry['trustStatus'] == 'untrusted'
                    for entry in reviewed), reviewed
                # Enroll the exact reviewed fixture hashes through native config;
                # altered definitions remain untrusted under the provider policy.
                send('config/value/write', {
                    'keyPath': 'hooks.state', 'mergeStrategy': 'replace',
                    'filePath': str(provider_home.resolve() / 'config.toml'),
                    'value': {entry['key']: {'trusted_hash': entry['currentHash']}
                              for entry in reviewed},
                }, 3)
                until(lambda item: item.get('id') == 3)
                send('hooks/list', {'cwds': [str(root)]}, 4)
                trusted = until(lambda item: item.get('id') == 4)
                print(json.dumps({'trusted_hook_inventory': trusted['result']}))
                assert all(entry['trustStatus'] == 'trusted'
                           for entry in trusted['result']['data'][0]['hooks']), trusted
                for index, mode in enumerate(('default', 'plan')):
                    release_initial.clear()
                    current_mode['mode'] = mode
                    base = 10 + index * 20
                    send('thread/start', {'cwd': str(root), 'model': 'gpt-5.6-luna',
                                          'sandbox': 'read-only', 'approvalPolicy': 'never'}, base)
                    started = until(lambda item: item.get('id') == base)
                    thread_id = started['result']['thread']['id']
                    send('turn/start', {'threadId': thread_id,
                                       'input': [{'type': 'text', 'text': 'Emit the context handoff.'}],
                                       'collaborationMode': {'mode': mode, 'settings': {
                                           'model': 'gpt-5.6-luna', 'reasoning_effort': 'low',
                                           'developer_instructions': None}}}, base + 1)
                    if native_stop:
                        until(lambda item: item.get('id') == base + 1)
                        send('thread/queue/add', {
                            'threadId': thread_id, 'clientUserMessageId': f'correction-{mode}',
                            'input': [{'type': 'text', 'text': 'SH711_OPERATOR_CORRECTION'}],
                        }, base + 4)
                        until(lambda item: item.get('id') == base + 4)
                        release_initial.set()
                        receipt_marker = ('SH711_PLAN_CONTINUES_READ_ONLY_REQUEST_PENDING'
                                          if mode == 'plan' else 'SH711_NATIVE_RECEIVING_SESSION_ACKNOWLEDGED')
                        received = until(lambda item: item.get('method') == 'turn/completed'
                                         and item.get('params', {}).get('threadId') == thread_id
                                         and receipt_marker in json.dumps(item))
                        assert received['params']['turn']['status'] == 'completed', received
                        acknowledged_commands = [item for item in messages
                            if item.get('method') == 'item/completed'
                            and item.get('params', {}).get('threadId') == thread_id
                            and item['params'].get('item', {}).get('type') == 'commandExecution']
                        assert len(acknowledged_commands) == (0 if mode == 'plan' else 1), acknowledged_commands
                        if mode == 'default':
                            command_result = acknowledged_commands[0]['params']['item']
                            assert command_result['exitCode'] == 0, command_result
                            assert 'acknowledged' in command_result['aggregatedOutput'], command_result
                        send('thread/queue/list', {'threadId': thread_id}, base + 5)
                        remaining = until(lambda item: item.get('id') == base + 5)
                        assert ('SH711_OPERATOR_CORRECTION' in json.dumps(wire_requests[-1]['body'])
                                or 'SH711_OPERATOR_CORRECTION' in json.dumps(remaining)), remaining
                        def correction_finished(item):
                            """Bind queued correction completion to this exact root thread."""
                            return (item.get('method') == 'turn/completed'
                                    and item.get('params', {}).get('threadId') == thread_id
                                    and 'SH711_CORRECTION_ACKNOWLEDGED' in json.dumps(item))
                        correction = next((item for item in messages if correction_finished(item)), None)
                        if correction is None:
                            correction = until(correction_finished)
                        assert correction['params']['turn']['status'] == 'completed', correction
                        send('thread/read', {'threadId': thread_id}, base + 3)
                        observed = until(lambda item: item.get('id') == base + 3)
                        transcript = Path(observed['result']['thread']['path'])
                        contexts = [json.loads(line)['payload'] for line in transcript.read_text().splitlines()
                                    if json.loads(line)['type'] == 'turn_context']
                        assert contexts, transcript.read_text()
                        assert all(context['collaboration_mode']['mode'] == mode for context in contexts), contexts
                        assert all(context['sandbox_policy']['type'] == 'read-only' for context in contexts), contexts
                        print(json.dumps({'mode': mode, 'native_receiving_turn': received,
                                          'remaining_queue': remaining['result'],
                                          'queued_correction_completed': correction}))
                        continue
                    until(lambda item: item.get('method') == 'turn/completed')
                    send('thread/queue/add', {
                        'threadId': thread_id, 'clientUserMessageId': f'correction-{mode}',
                        'input': [{'type': 'text', 'text': 'SH711_OPERATOR_CORRECTION'}],
                    }, base + 4)
                    added = until(lambda item: item.get('id') == base + 4)
                    queued_correction = added['result']['queuedSubmission']
                    # The provider automatically starts queued input when idle.
                    # Compacting immediately would interrupt this correction.
                    correction_turn = until(lambda item: item.get('method') == 'turn/completed')
                    assert correction_turn['params']['turn']['status'] == 'completed', correction_turn
                    assert 'SH711_CORRECTION_ACKNOWLEDGED' in json.dumps(correction_turn), correction_turn
                    send('thread/compact/start', {'threadId': thread_id}, base + 2)
                    until(lambda item: item.get('id') == base + 2)
                    compact_turn = until(lambda item: item.get('method') == 'turn/completed')
                    assert compact_turn['params']['turn']['status'] == 'completed', compact_turn
                    send('thread/read', {'threadId': thread_id, 'includeTurns': False}, base + 3)
                    observed = until(lambda item: item.get('id') == base + 3)
                    send('thread/queue/list', {'threadId': thread_id}, base + 5)
                    preserved = until(lambda item: item.get('id') == base + 5)
                    assert preserved['result']['data'] == [], preserved
                    send('turn/start', {'threadId': thread_id, 'input': [{
                        'type': 'text', 'text': 'SH711_CONTINUE_AFTER_COMPACTION'}]}, base + 7)
                    until(lambda item: item.get('id') == base + 7)
                    receiving_turn = until(lambda item: item.get('method') == 'turn/completed')
                    assert receiving_turn['params']['turn']['status'] == 'completed', receiving_turn
                    assert 'SH711_RECEIVING_SESSION_ACKNOWLEDGED' in json.dumps(receiving_turn), receiving_turn
                    transcript_path = Path(observed['result']['thread']['path'])
                    transcript_events = [json.loads(line) for line in transcript_path.read_text().splitlines()]
                    last_context = next(event['payload'] for event in reversed(transcript_events)
                                        if event['type'] == 'turn_context')
                    assert last_context['collaboration_mode']['mode'] == mode, last_context
                    assert last_context['sandbox_policy']['type'] == 'read-only', last_context
                    assert 'SH711_OPERATOR_CORRECTION' in json.dumps(wire_requests[-1]['body']), wire_requests[-1]
                    print(json.dumps({'mode': mode, 'thread_id': thread_id,
                                      'thread_after_compaction': observed['result'],
                                      'preserved_correction': preserved['result'],
                                      'queued_correction': queued_correction,
                                      'correction_turn': correction_turn,
                                      'receiving_turn': receiving_turn,
                                      'transcript_event_shapes': [{
                                          'type': event.get('type'),
                                          'payload_keys': list(event.get('payload', {})),
                                          'payload_type': event.get('payload', {}).get('type'),
                                          'turn_id': event.get('payload', {}).get('turn_id'),
                                      } for event in transcript_events]}))
                recorded = ([json.loads(line) for line in hook_events.read_text().splitlines()]
                            if hook_events.exists() else [])
                compactions = [item for item in messages if item.get('method') == 'item/completed'
                               and item.get('params', {}).get('item', {}).get('type') == 'contextCompaction']
                print(json.dumps({'hook_events': recorded, 'compaction_items': compactions,
                                  'model_paths': [request['path'] for request in wire_requests]}))
                if native_stop:
                    calls = [json.loads(line) for line in process_events.read_text().splitlines()]
                    requests = [call for call in calls if 'continuation' in call['argv']]
                    assert len(requests) == 2, requests
                    assert len({json.loads(call['text'])['origin']['session_id'] for call in requests}) == 2, requests
                    assert not any(path.read_text() for path in provider_home.rglob('*.storyhook-plan-approval')), calls
                    assert not compactions, compactions
                    observations = [json.loads(line) for line in hook_states.read_text().splitlines()]
                    initial_stops = [state for state in observations
                                     if state['event'] == 'Stop'
                                     and 'storyhook.session-handoff' in (state['message'] or '')]
                    assert len(initial_stops) == 2, initial_stops
                    assert all(not state['correction_in_transcript'] for state in initial_stops), initial_stops
                    print(json.dumps({'native_story_requests': requests, 'hook_state_observations': observations}))
                    return
                if not any(item.get('hook_event_name') == 'PostCompact' for item in recorded):
                    raise RuntimeError('PostCompact was not emitted; inspect hook trust diagnostics below')
                assert len(compactions) == 2, compactions
                for compaction in compactions:
                    identity = compaction['params']
                    session_hooks = [event for event in recorded
                                     if event['session_id'] == identity['threadId']]
                    assert [event['hook_event_name'] for event in session_hooks] == [
                        'SessionStart', 'Stop', 'Stop', 'PreCompact', 'PostCompact',
                        'SessionStart', 'Stop'], session_hooks
                    assert session_hooks[3]['turn_id'] == identity['turnId'], session_hooks
                    assert session_hooks[4]['turn_id'] == identity['turnId'], session_hooks
                    assert session_hooks[-1]['last_assistant_message'] == 'SH711_RECEIVING_SESSION_ACKNOWLEDGED', session_hooks
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                reader.join(timeout=2)
                errors.seek(0)
                print(errors.read().decode(), file=sys.stderr)
                print(json.dumps({'provider_events': messages}), file=sys.stderr)
                server.shutdown()
                server.server_close()
                server_thread.join(timeout=2)


if __name__ == '__main__':
    run_probe(native_stop='--native-stop' in sys.argv)

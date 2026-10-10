"""One owned, single-flight process for the two optional exposure commands.

Admission remains strict. This process receives work only after the gate root
starts. It never runs a gate, changes limits, retries a helper or certifies work.
"""

import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import sys
import time

from gate_measurement_optional import COMMANDS, CONTRACT, ExposureTimeout, complete, quiescence
from verifier_state import Refusal, held, paths, read
from host_admission import native
from host_admission.policy import Refusal as CustodyRefusal

MAX_SAMPLES = 4096
MAX_PACKET = 8192
MAX_SAMPLE_BYTES = 8 * 1024 * 1024


def owner(path, collector=None):
    from gate_measurement_context import manifest
    value = manifest(path)
    if value.get('policy', {}).get('optional_telemetry') != CONTRACT:
        raise Refusal('optional worker has no matching policy')
    common, worktree, key = paths(value['common'], value['worktree'])
    row = read(str(key) + '.owner')
    if (not row or row.get('measurement') != str(path)
            or row.get('measurement_sha256') != hashlib.sha256(Path(path).read_bytes()).hexdigest()):
        raise Refusal('optional worker lost its exact measurement owner')
    if collector is None:
        valid = held(common, worktree, key)
    else:
        boot = native.boot_identity()
        valid = (type(collector.get('pid')) is int and collector['pid'] == os.getppid()
                 and native.identity(collector['pid'], boot) == collector
                 and row.get('common') == str(common) and row.get('worktree') == str(worktree)
                 and row.get('nonce') == os.environ.get('STORYHOOK_VERIFIER_OWNER')
                 and row.get('boot', '').lower() == boot.lower()
                 and os.getsid(collector['pid']) == row.get('session'))
    if not valid:
        raise Refusal('optional worker lost its live collector capability')
    return value, row


def running(path, collector=None):
    _, row = owner(path, collector)
    return row.get('gate_started') is True and type(row.get('gate_session')) is int and row['gate_session'] > 0


def save_sample(directory, sequence, row):
    data = json.dumps(dict(row, version=1), allow_nan=False, sort_keys=True).encode()
    if len(data) > MAX_SAMPLE_BYTES:
        raise Refusal('optional exposure sample is too large')
    path = directory / f'sample-{sequence:04}.json'
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data); stream.flush(); os.fsync(stream.fileno())
    return hashlib.sha256(data).hexdigest()


def sample(directory, end, collector_pid):
    from gate_measurement_command import bounded
    from gate_measurement_campaign import competing_work, owned_resources
    row = {'at': time.monotonic(), 'fields': {}}
    for name, argv in COMMANDS.items():
        if time.monotonic() >= end:
            raise Refusal('optional worker overall deadline expired')
        try:
            result = bounded(argv, root=directory / 'helpers', seconds=30,
                             optional_overall_end=end)
        except ExposureTimeout as error:
            return {'state': 'timeout', 'helper': name, 'proof': error.proof}
        if result.returncode:
            raise Refusal('descriptive helper failed without a local timeout')
        row['fields'][name] = result.stdout
    # Parsing/omitted owner errors remain fatal, not the approved timeout class.
    row['competing_pids'] = competing_work(row['fields']['processes'], collector_pid)
    row['owned_resources'] = owned_resources(row['fields']['resource_processes'], collector_pid)
    return {'state': 'complete', 'sample': row}


def serve(directory, manifest_path, end, fd, collector):
    from gate_measurement_context import validate_workspace
    # This bootstrap validates the real launcher/manifest boundary before ready.
    value, _ = owner(manifest_path, collector)
    validate_workspace(value)
    if value.get('kind') != 'gate-throughput-measurement' or value.get('policy', {}).get('optional_telemetry') != CONTRACT:
        raise Refusal('optional worker requires the versioned throughput policy')
    channel = socket.socket(fileno=fd)
    os.set_inheritable(fd, False)  # Never inherit IPC into observed ps commands.
    channel.settimeout(1)
    sequence = 0
    def interrupted(number, _frame):
        raise InterruptedError(f'optional worker signal {number}')
    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, interrupted)
    try:
        channel.send(b'{"state":"ready"}')
        while time.monotonic() < end:
            owner(manifest_path, collector)
            try:
                request = channel.recv(16)
            except TimeoutError:
                continue
            if request == b'Q':
                return 0
            if request != b'R' or sequence >= MAX_SAMPLES:
                raise Refusal('optional worker request is invalid or exhausted')
            sequence += 1
            if not running(manifest_path, collector):
                channel.send(json.dumps({'state': 'root-ended', 'sequence': sequence}).encode())
                return 0
            row = sample(directory, end, collector['pid'])
            owner(manifest_path, collector)
            if time.monotonic() >= end:
                raise Refusal('overall deadline expired during descriptive observation')
            row['sequence'] = sequence
            digest = save_sample(directory, sequence, row)
            reply = dict(state=row['state'], sequence=sequence, sha256=digest)
            if row['state'] == 'timeout':
                reply.update(helper=row['helper'], proof=row['proof'])
            channel.send(json.dumps(reply).encode())
            if row['state'] == 'timeout':
                return 0  # First gap disables all further optional sampling.
        raise Refusal('optional worker overall deadline expired')
    finally:
        channel.close()


class Worker:
    def __init__(self, directory, manifest_path, end):
        from build_products import ProductCustody
        from host_admission.supervisor import ManagedProcess
        self.directory = Path(directory) / 'telemetry'
        self.directory.mkdir(mode=0o700)
        from gate_measurement_context import validate
        validate(manifest_path)
        self.end, self.process = end, None
        self.sequence, self.pending, self.ready = 0, False, False
        self.disabled, self.closed = False, False
        self.summary = complete()
        self.receipts = []
        self.channel, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
        self.channel.setblocking(False)
        self.manifest_path = manifest_path
        command = [sys.executable, '-B', str(Path(__file__).resolve()), 'serve', str(self.directory),
                   str(manifest_path), str(end), str(child.fileno()),
                   json.dumps(native.identity(os.getpid(), native.boot_identity()))]
        class Deadline:
            def publish(inner):
                if time.monotonic() >= self.end:
                    raise CustodyRefusal('optional worker overall deadline expired')
        try:
            custody = ProductCustody(self.directory, command)
            self.process = ManagedProcess(custody, custody.lease, command, publisher=Deadline(),
                                          grant_environment=False, pass_fds=(child.fileno(),))
        except BaseException:
            self.channel.close()
            raise
        finally:
            child.close()

    def poll(self):
        # One request and one reply: no unbounded drain, thread or whole-host scan.
        try:
            packet = self.channel.recv(MAX_PACKET)
        except BlockingIOError:
            if not self.disabled and (self.process.finished or self.process._exited()):
                raise Refusal('optional worker exited without a terminal reply')
            return
        try:
            row = json.loads(packet)
        except (ValueError, UnicodeError) as error:
            raise Refusal('optional worker reply is malformed') from error
        if not self.ready and row == {'state': 'ready'}:
            self.ready = True
            return
        if (not self.pending or not isinstance(row, dict)
                or type(row.get('sequence')) is not int or row['sequence'] != self.sequence):
            raise Refusal('optional worker reply has no exact pending request')
        if row.get('state') == 'root-ended' and set(row) == {'state', 'sequence'}:
            if running(self.manifest_path):
                raise Refusal('optional worker incorrectly reported gate completion')
            self.disabled, self.pending = True, False
            return
        expected_keys = {'state', 'sequence', 'sha256'}
        if row.get('state') == 'timeout':
            expected_keys |= {'helper', 'proof'}
            if row.get('helper') not in COMMANDS:
                raise Refusal('unknown optional helper timeout')
            proof = row.get('proof', {})
            helper = Path(proof.get('directory', ''))
            if helper.parent != self.directory / 'helpers':
                raise Refusal('foreign helper quiescence proof')
            quiescence(helper, expected=proof)
            if time.monotonic() >= self.end:
                raise Refusal('overall deadline expired during optional cleanup proof')
            self.summary = {'version': 1, 'complete': False,
                            'gaps': [{'helper': row['helper'], 'reason': 'local-timeout-quiescent'}]}
            self.disabled = True
        elif row.get('state') != 'complete':
            raise Refusal('optional worker failed outside the timeout exemption')
        if set(row) != expected_keys or not isinstance(row['sha256'], str) or len(row['sha256']) != 64:
            raise Refusal('optional worker reply schema changed')
        self.receipts.append(row)
        self.pending = False

    def request(self):
        if self.disabled or self.pending or not self.ready:
            return
        if self.sequence >= MAX_SAMPLES:
            raise Refusal('optional sampling ceiling exhausted')
        if not running(self.manifest_path):
            return
        self.channel.send(b'R')
        self.sequence += 1
        self.pending = True

    def close(self, *, safety=None, cancel=False):
        if self.closed:
            return self.summary
        try:
            if not cancel and self.pending:
                self.poll()
            if not cancel and not self.disabled:
                try:
                    self.channel.send(b'Q')
                except (BrokenPipeError, ConnectionRefusedError):
                    # A terminal gap can arrive between poll and Q. Accept only
                    # its exact validated terminal reply, never a quiet exit.
                    self.poll()
                    if not self.disabled:
                        raise Refusal('optional worker exited before stop acknowledgement')
            # At gate completion, settle this worker and any last helper. Continue
            # mandatory checks while waiting; no detached background sampler.
            outer = self
            class Observe:
                last = 0
                def publish(inner):
                    if time.monotonic() >= outer.end:
                        raise CustodyRefusal('optional worker settlement reached overall deadline')
                    if safety is not None and time.monotonic() - inner.last >= 5:
                        try:
                            safety()
                        except Refusal as error:
                            raise CustodyRefusal(str(error)) from error
                        inner.last = time.monotonic()
            self.process.publisher = Observe()
            code = self.process.wait(force_cancel=cancel)
            if code or cancel:
                raise Refusal('optional worker failed or was cancelled')
            # Drain at most ready plus the single outstanding response.
            for _ in range(2):
                if not self.ready or self.pending:
                    self.poll()
            if self.pending or not self.ready:
                raise Refusal('optional worker has missing terminal evidence')
            self.verify_receipts()
            self.closed = True
            return self.summary
        finally:
            self.closed = True
            try:
                self.process.close()
            finally:
                self.channel.close()

    def verify_receipts(self):
        for row in self.receipts:
            path = self.directory / f"sample-{row['sequence']:04}.json"
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(fd, 'rb') as stream:
                data = stream.read(MAX_SAMPLE_BYTES + 1)
            if len(data) > MAX_SAMPLE_BYTES or hashlib.sha256(data).hexdigest() != row['sha256']:
                raise Refusal('optional sample evidence changed')
            value = json.loads(data)
            if value.get('version') != 1 or value.get('state') != row['state'] or value.get('sequence') != row['sequence']:
                raise Refusal('optional sample does not match its response')
            if row['state'] == 'timeout' and (value.get('proof') != row['proof'] or value.get('helper') != row['helper']):
                raise Refusal('optional timeout evidence differs from its proof')
            if row['state'] == 'complete':
                sample_row = value.get('sample', {})
                if set(sample_row.get('fields', {})) != set(COMMANDS) or not isinstance(sample_row.get('owned_resources'), dict):
                    raise Refusal('optional exposure sample is incomplete')
        # Failed helper journals deliberately remain nonterminal; prove them
        # independently, and never turn absent processes into a finished record.
        from build_products import read_record
        for path in (self.directory / 'helpers').glob('build-*/record.json'):
            record = read_record(path)
            if record.get('state') == 'finished':
                if record.get('executions') != []:
                    raise Refusal('finished helper retains executions')
            else:
                matches = [r for r in self.receipts if r['state'] == 'timeout'
                           and r['proof']['directory'] == str(path.parent)]
                if len(matches) != 1:
                    raise Refusal('optional worker left unaccounted helper custody')
                quiescence(path.parent, expected=matches[0]['proof'])


if __name__ == '__main__':
    if len(sys.argv) != 7 or sys.argv[1] != 'serve':
        raise SystemExit('invalid optional worker invocation')
    raise SystemExit(serve(Path(sys.argv[2]), sys.argv[3], float(sys.argv[4]), int(sys.argv[5]), json.loads(sys.argv[6])))

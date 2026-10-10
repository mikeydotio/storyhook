"""A fixed, isolated real CLI and SessionStart workload outside the gate clamp."""

import json
import importlib.util
import os
from pathlib import Path
import subprocess
import time

from gate_measurement_data import hook_context
from gate_measurement_runtime import capture, probe_environment, scheduling, sha256
from gate_measurement_setup import immutable
from verifier_state import Refusal
from gate_measurement_bounds import Deadline, LIMITS
from gate_measurement_command import bounded


class Probes:
    """Own the isolated probe daemon and retain each complete probe response."""

    def __init__(self, output, identity):
        self.root = Path(output) / 'probe'
        self.project = self.root / 'project'
        self.project.mkdir(exist_ok=True)
        self.story = str(self.root / 'bin/story')
        self.hook = Path(identity['worktree']) / 'plugins/story/hooks/session-start.sh'
        self.env = probe_environment(os.environ, self.root, os.getpid())
        helper = self.hook.parent.parent / 'lib/process_identity.py'
        spec = importlib.util.spec_from_file_location('measurement_process_identity', helper)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.env['STORYHOOK_PARENT_START_TIME'] = module.process_identity(os.getpid())['start']
        self.identity = identity

    def cli(self, *args):
        """Use only this experiment's binary, project and store."""
        return capture([self.story, *args], cwd=self.project, env=self.env)

    def start(self):
        """Seed once and refuse partial or changed fixture state on restart."""
        preparation = Deadline(LIMITS['probe_preparation_seconds'])
        if sha256(self.story) != self.identity['binary_sha256']:
            raise Refusal('probe binary identity changed')
        if not (self.project / '.storyhook.toml').exists():
            self.cli('project', 'new', '--prefix', 'MB', '--name', 'Gate measurement', '--no-agents-md')
            for index in range(10):
                preparation.require('probe preparation')
                self.cli('new', f'Measurement fixture {index + 1:02}', '--type', 'normal', '--priority', 'low')
        rows = json.loads(self.cli('list', '--json'))
        # Preserve the full immutable response,
        # including IDs and state, rather than accepting only a row count.
        if not isinstance(rows, dict) or rows.get('result') != 'ok' or len(rows.get('stories', [])) != 10:
            raise Refusal(f'probe project requires exactly ten stories: {rows!r}')
        immutable(self.root / 'fixture.json', {'version': 1, 'stories': rows,
                                              'hook_sha256': sha256(self.hook)})
        preparation.require('probe preparation')
        self.expected = rows
        for name in ('list', 'hook'):
            result = self.run(name, self.root / ('warm-' + name))
            if not result['ok']:
                raise Refusal(f'probe warmup failed: {result}')

    def run(self, name, prefix):
        """Measure one real response and reject degraded hook success."""
        if name not in ('list', 'hook'):
            raise Refusal(f'unknown probe: {name}')
        command = [self.story, 'list', '--json'] if name == 'list' else ['bash', str(self.hook)]
        result = bounded(command, root=self.root / 'operations', seconds=LIMITS['probe_seconds'],
                         input=json.dumps({'cwd': str(self.project)}) if name == 'hook' else '',
                         cwd=self.project, env=self.env)
        elapsed = result.wall_seconds
        Path(str(prefix) + '.stdout').write_text(result.stdout)
        Path(str(prefix) + '.stderr').write_text(result.stderr)
        error = None
        try:
            if result.returncode:
                raise Refusal(f'probe exited {result.returncode}')
            if name == 'hook':
                hook_context(result.stdout)
            elif json.loads(result.stdout) != self.expected:
                raise Refusal('list response differs from the fixed fixture')
        except (Refusal, ValueError) as problem:
            error = str(problem)
        return {'name': name, 'seconds': elapsed, 'ok': error is None, 'error': error,
                'exit_code': result.returncode, 'launcher_class': scheduling()}

    def stop(self):
        """Stop only the isolated daemon; cleanup failure is evidence, never ignored."""
        self.cli('daemon', 'stop')

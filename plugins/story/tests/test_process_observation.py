"""Native restored-process proof preserves argv, cwd and incarnation boundaries."""

import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import process_observation as observation


class ObservationTests(unittest.TestCase):
    """Use live child processes plus malformed native-buffer contracts."""

    def test_darwin_decoder_retains_spaces_empty_args_and_excludes_environment(self):
        raw = struct.pack('=i', 4) + b'/bin/provider\0\0\0provider\0resume\0\0two words\0SECRET=ignored\0'
        self.assertEqual(observation.darwin_argv(raw), ['provider', 'resume', '', 'two words'])

    def test_darwin_decoder_rejects_invalid_counts_or_truncated_arguments(self):
        for raw in (b'', struct.pack('=i', -1) + b'/p\0p\0',
                    struct.pack('=i', 2) + b'/p\0p\0missing-terminator',
                    struct.pack('=i', 1) + b'no-terminator'):
            with self.subTest(raw=raw):
                with self.assertRaises(ValueError): observation.darwin_argv(raw)

    def test_live_child_preserves_exact_argv_cwd_parent_and_kernel_identity(self):
        with tempfile.TemporaryDirectory(prefix='sh825-process-', dir='/tmp') as directory:
            script = 'import sys; print("ready", flush=True); sys.stdin.read()'
            args = [sys.executable, '-c', script, '', 'conversation with spaces', '--resume=x']
            with subprocess.Popen(args, cwd=directory, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  text=True, env=dict(os.environ, SH825_PRIVATE_TEST='never-return')) as child:
                try:
                    self.assertEqual(child.stdout.readline().strip(), 'ready')
                    actual = observation.observe_process(child.pid)
                    self.assertEqual(actual['argv'][1:], args[1:])
                    self.assertEqual(os.path.realpath(actual['argv'][0]), actual['process']['executable'])
                    self.assertEqual(actual['cwd'], str(Path(directory).resolve()))
                    self.assertEqual(actual['parent'], os.getpid())
                    self.assertEqual(actual['process']['pid'], child.pid)
                    self.assertTrue(actual['process']['start'].startswith(('macos:', 'linux:')))
                    self.assertNotIn('never-return', repr(actual))
                    self.assertEqual(observation.observe_process(child.pid), actual)
                finally:
                    child.communicate('')
            with self.assertRaises(ProcessLookupError): observation.observe_process(child.pid)

    def test_process_replacement_during_observation_refuses(self):
        old = dict(pid=123, start='old', executable='/bin/provider')
        with patch.object(observation, 'process_identity', side_effect=[old, dict(old, start='new')]), \
                patch.object(observation, '_details', return_value=(1, '/tmp', ['provider'])):
            with self.assertRaisesRegex(RuntimeError, 'changed'): observation.observe_process(123)

    def test_parent_chain_must_reach_the_captured_root_without_cycles(self):
        table = {10: 1, 11: 10, 12: 11, 13: 20, 20: 13, 30: 1}
        self.assertEqual(observation.descendants(table, 10), {10, 11, 12})
        self.assertEqual(observation.descendants(table, 99), set())

    def test_linux_proc_fields_preserve_comm_delimiters_and_argument_boundaries(self):
        with tempfile.TemporaryDirectory(prefix='sh825-procfs-', dir='/tmp') as directory:
            root = Path(directory)
            proc = root / '123'
            proc.mkdir()
            (proc / 'stat').write_text('123 (name with ) spaces) S 456 0 0')
            (proc / 'cwd').symlink_to(root)
            (proc / 'cmdline').write_bytes(b'provider\0resume\0\0conversation with spaces\0')
            with patch.object(observation.sys, 'platform', 'linux'), patch.object(observation, 'Path', return_value=root):
                self.assertEqual(observation._details(123), (456, str(root), ['provider', 'resume', '', 'conversation with spaces']))
                for raw in (b'', b'unterminated'):
                    (proc / 'cmdline').write_bytes(raw)
                    with self.assertRaises(ValueError): observation._details(123)
                (proc / 'cmdline').unlink()
                with self.assertRaises(ProcessLookupError): observation._details(123)


if __name__ == '__main__':
    unittest.main()

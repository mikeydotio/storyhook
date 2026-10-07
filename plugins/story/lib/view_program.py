"""Compose the verification view's embedded dependency modules for process tests."""
import json
from pathlib import Path


def program(root):
    """Return the exact bytes of the native view bridge's VIEW_PROGRAM.

    window_tests.rs requires byte equality (SH-881), so the JSON is serde_json's
    compact form and each source is its exact UTF-8 bytes, as include_str! reads
    them, with no newline translation.
    """
    root = Path(root)
    library = root / 'plugins/story/lib'

    def read(path):
        """One source exactly as include_str! embeds it."""
        return path.read_bytes().decode('utf-8')

    modules = [(name, read(library / (name + '.py')))
               for name in ('process_identity', 'process_observation', 'restored_dispatch')]
    return (read(library / 'probe_budget.py') + '\nprobe_run = run\nprobe_operation = operation\n'
            + read(library / 'tmux_server_env.py') + '\n' + read(library / 'tmux_target.py')
            + '\nimport types,sys\nfor _name,_source in '
            + json.dumps(modules, separators=(',', ':'), ensure_ascii=False) + ':\n'
            + '    _module = types.ModuleType(_name)\n    sys.modules[_name] = _module\n    exec(_source, _module.__dict__)\n'
            + read(root / 'scripts/verification-view.py'))

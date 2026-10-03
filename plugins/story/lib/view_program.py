"""Compose the verification view's embedded dependency modules for process tests."""
import json
from pathlib import Path


def program(root):
    """Use the same module order and shipping sources as the native view bridge."""
    root = Path(root)
    library = root / 'plugins/story/lib'
    modules = [(name, (library / (name + '.py')).read_text())
               for name in ('process_identity', 'process_observation', 'restored_dispatch')]
    return ((library / 'probe_budget.py').read_text() + '\nprobe_run = run\nprobe_operation = operation\n'
            + (library / 'tmux_server_env.py').read_text() + '\n' + (library / 'tmux_target.py').read_text()
            + '\nimport types,sys\nfor _name,_source in ' + json.dumps(modules) + ':\n'
            + '    _module = types.ModuleType(_name)\n    sys.modules[_name] = _module\n    exec(_source, _module.__dict__)\n'
            + (root / 'scripts/verification-view.py').read_text())

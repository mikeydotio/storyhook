"""Operation-local protected transport for identity and lifecycle helpers."""

import contextlib
import contextvars
import os

import probe_budget
from tmux_server_env import client_environment
from tmux_target import logical_socket, resolve_target, target_arguments

_clients = contextvars.ContextVar("storyhook_tmux_clients", default=None)


@contextlib.contextmanager
def operation(budget=probe_budget.BUDGET_SECONDS):
    """Share one deadline and immutable server targets through nested helpers."""
    with probe_budget.operation(budget=budget):
        if _clients.get() is not None:
            yield
            return
        token = _clients.set({})
        try:
            yield
        finally:
            _clients.reset(token)


class Client:
    """A checked transport target; numeric identity still needs a current binding."""

    def __init__(self, socket):
        """Inspect persistent ownership without starting or restoring a server."""
        self.environment = os.environ.copy()
        self.target = resolve_target(socket, self.environment, probe_budget.run,
                                     client_environment(self.environment))

    def require_binding(self, socket):
        """Reject saved numeric identities that would follow another generation."""
        selected = logical_socket(socket, self.environment)
        if self.target['protected'] and selected != self.target['endpoint']:
            raise RuntimeError(f"protected tmux binding on {selected} requires re-adoption before using {self.target['endpoint']}")

    def arguments(self, arguments, binding=False, socket=None):
        """Pin argv to this target, optionally requiring saved pane authority."""
        if binding:
            self.require_binding(socket)
        return ['tmux', *target_arguments(self.target, arguments)]

    def observed_socket(self, socket):
        """Normalize only a provider-proven logical/private alias in a tmux row."""
        if not self.target['protected']:
            return socket
        if os.path.realpath(socket) not in (self.target['socket'], self.target['endpoint']):
            raise RuntimeError(f"tmux reported foreign socket {socket} through {self.target['endpoint']}")
        return self.target['endpoint']


def client(socket=None):
    """Reuse this operation's target, never cache discovery across operations."""
    selected = logical_socket(socket, os.environ)
    cache = _clients.get()
    if cache is not None and selected in cache:
        return cache[selected]
    found = Client(socket)
    if cache is not None:
        target = found.target
        existing = cache.get(target['socket'])
        if existing and existing.target != target:
            raise RuntimeError(f"tmux generation changed during operation on {target['socket']}")
        for alias in (selected, target['socket'], target['endpoint']):
            cache[alias] = found
    return found

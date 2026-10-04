"""Replay durable admission observations into an explicitly bound gate journal."""

import json
import os
from pathlib import Path
import stat

from .policy import Refusal, integer, label


class Publisher:
    """A gate-local delivery cursor; the authority remains the durable source."""

    def __init__(self, client, binding, journal):
        if not isinstance(binding, dict) or set(binding) != {"attempt_id", "execution_id", "generation"}:
            raise Refusal("incomplete resource evidence binding")
        label(binding["attempt_id"], "attempt"); label(binding["execution_id"], "execution")
        integer(binding["generation"], "generation")
        self.client, self.binding, self.journal = client, dict(binding), Path(journal)
        self.cursor = 0

    def publish(self):
        """Publish complete bound records, refusing lost evidence."""
        while True:
            events = self.client.call("events", after=self.cursor)
            if not events:
                return
            try:
                fd = os.open(self.journal, os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW | os.O_CLOEXEC)
                try:
                    facts = os.fstat(fd)
                    if not stat.S_ISREG(facts.st_mode) or facts.st_uid != os.getuid():
                        raise Refusal("resource journal is not an owned regular file")
                    cursor = self.cursor
                    for event in events:
                        sequence = integer(event["sequence"], "event sequence")
                        if sequence <= cursor:
                            raise Refusal("resource evidence cursor did not advance")
                        if event.get("lease") is None or event.get("binding") == self.binding:
                            record = dict(kind="resource", **self.binding, observation=event)
                            data = (json.dumps(record, separators=(",", ":")) + "\n").encode()
                            if os.write(fd, data) != len(data):
                                raise Refusal("short resource journal write")
                        cursor = sequence
                    os.fsync(fd)
                    self.cursor = cursor
                finally:
                    os.close(fd)
            except OSError as error:
                raise Refusal(f"resource journal delivery failed for {self.journal}: {error}") from error

"""SH-872 cold/warm/reuse evidence controller, separate from production receipts.

This module plans and validates one revision's nine observations. An owned gate
adapter must supply actual execution/settlement evidence; this module neither
executes a command nor certifies a production tree.
"""

import hashlib
import json
from pathlib import Path
import re

from verifier_state import Refusal
from gate_measurement_optional import completeness
from gate_measurement_runtime import journal, records

IDENTITY_FIELDS = {
    'source_commit', 'source_tree', 'toolchain', 'environment_digest',
    'configuration_digest', 'worker_limits', 'gate_argv', 'target_identity',
    'applicable_legs',
}
ORDER = ['cold', 'warm', 'reuse'] * 3
WINDOW_SECONDS = 10 * 60 * 60
CAMPAIGN_SECONDS = 2 * WINDOW_SECONDS


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False)


def fingerprint(identity):
    """Unknown/missing identity components never become a reusable result."""
    if (not isinstance(identity, dict) or set(identity) != IDENTITY_FIELDS
            or any(value is None for value in identity.values())):
        raise Refusal('measurement identity is incomplete or has unknown fields')
    for name in ('source_commit', 'source_tree'):
        if not isinstance(identity[name], str) or not re.fullmatch(r'[a-f0-9]{40}|[a-f0-9]{64}', identity[name]):
            raise Refusal(f'measurement {name} must be an exact Git identity')
    for name in ('environment_digest', 'configuration_digest'):
        if not isinstance(identity[name], str) or not re.fullmatch(r'[a-f0-9]{64}', identity[name]):
            raise Refusal(f'measurement {name} must cover the pinned complete input')
    for name in ('toolchain', 'worker_limits', 'target_identity'):
        if not isinstance(identity[name], dict) or not identity[name]:
            raise Refusal(f'measurement {name} must be observed and nonempty')
    if (not isinstance(identity['gate_argv'], list) or not identity['gate_argv']
            or any(not isinstance(arg, str) or not arg or '\x00' in arg
                   for arg in identity['gate_argv'])):
        raise Refusal('measurement gate argv is missing')
    legs = identity['applicable_legs']
    if (not isinstance(legs, list) or not legs
            or any(not isinstance(leg, str) or not re.fullmatch(r'[a-z][a-z0-9-]*', leg) for leg in legs)
            or len(set(legs)) != len(legs)):
        raise Refusal('measurement applicable legs are missing, duplicated or unsafe')
    try:
        return hashlib.sha256(canonical(identity).encode()).hexdigest()
    except (TypeError, ValueError) as error:
        raise Refusal('measurement identity contains unsupported values') from error


class Cohort:
    """An append-only nine-slot window. Failures consume slots and stop admission."""

    def __init__(self, output, revision):
        if revision not in ('baseline', 'optimization'):
            raise Refusal('only baseline and one optimization are included')
        output = Path(output)
        if not output.is_absolute() or output.resolve() != output:
            raise Refusal('measurement result namespace must be physical')
        self.root = output / 'measurement-results-v1' / revision
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self.root.resolve() != self.root:
            raise Refusal('measurement result namespace was substituted')
        self.path = self.root / 'observations.jsonl'
        self.revision = revision

    def history(self):
        rows = records(self.path)
        expected = 0
        pending = None
        failed = False
        for row in rows:
            if row.get('version') != 2:
                raise Refusal('measurement evidence predates required telemetry completeness')
            if row['kind'] == 'start':
                if (pending or failed or type(row.get('slot')) is not int
                        or row.get('slot') != expected or expected >= len(ORDER)):
                    raise Refusal('duplicate/out-of-order measurement start')
                ceiling = 600 if ORDER[expected] == 'reuse' else 4500
                if (row.get('mode') != ORDER[expected]
                        or type(row.get('block')) is not int or row['block'] != expected // 3
                        or type(row.get('ceiling_seconds')) is not int
                        or row['ceiling_seconds'] != ceiling
                        or fingerprint(row.get('identity')) != row.get('key')):
                    raise Refusal('measurement start identity/order is corrupt')
                pending = row
            elif row['kind'] == 'finish':
                if (not pending or type(row.get('slot')) is not int
                        or row.get('slot') != pending['slot']):
                    raise Refusal('measurement finish has no matching start')
                from gate_measurement_data import finite_seconds
                elapsed = finite_seconds(row.get('admission_to_settlement_seconds'))
                legs = pending['identity']['applicable_legs']
                coverage = (row.get('executed') == legs and row.get('reused') == []) if pending['mode'] != 'reuse' else (row.get('reused') == legs and row.get('executed') == [])
                telemetry_complete = completeness(row.get('telemetry'))
                accepted = telemetry_complete and row.get('exit_code') == 0 and row.get('settled') is True and coverage and elapsed <= pending['ceiling_seconds']
                if (type(row.get('exit_code')) is not int or type(row.get('settled')) is not bool
                        or row.get('mode') != pending['mode'] or row.get('accepted') is not accepted
                        or row.get('production_target_breach') is not (elapsed >= 900)
                        or row.get('production_certification') is not False):
                    raise Refusal('measurement terminal evidence is corrupt')
                failed = not accepted
                pending = None
                expected += 1
            else:
                raise Refusal('unknown measurement observation')
        return rows, pending, expected

    def begin(self, identity, *, remaining_window, remaining_campaign):
        rows, pending, slot = self.history()
        if pending:
            raise Refusal('previous measurement has no terminal settlement evidence')
        if any(row['kind'] == 'finish' and row.get('accepted') is not True for row in rows):
            raise Refusal('failed measurement retained; no retry or replacement is allowed')
        if slot >= len(ORDER):
            raise Refusal('measurement revision exhausted its nine gate slots')
        mode = ORDER[slot]
        ceiling = 600 if mode == 'reuse' else 4500
        from gate_measurement_data import finite_seconds
        finite_seconds(remaining_window)
        finite_seconds(remaining_campaign)
        if not ceiling <= remaining_window <= WINDOW_SECONDS or not ceiling <= remaining_campaign <= CAMPAIGN_SECONDS:
            raise Refusal('measurement allowance does not fit the retained window/campaign')
        key = fingerprint(identity)
        starts = [row for row in rows if row['kind'] == 'start']
        if mode == 'cold' and starts:
            comparable = lambda value: {k: v for k, v in value.items() if k != 'target_identity'}
            if comparable(identity) != comparable(starts[0]['identity']):
                raise Refusal('measurement revision identity changed between blocks')
            if any(row['identity']['target_identity'] == identity['target_identity'] for row in starts):
                raise Refusal('cold block must use a fresh target identity')
        if self.revision == 'optimization':
            baseline = Cohort(self.root.parent.parent, 'baseline')
            prior, live, completed = baseline.history()
            if live or completed != 9 or any(row['kind'] == 'finish' and not row['accepted'] for row in prior):
                raise Refusal('optimization requires a complete accepted baseline')
            baseline_identity = prior[0]['identity']
            for field in IDENTITY_FIELDS - {'source_commit', 'source_tree', 'target_identity'}:
                if identity[field] != baseline_identity[field]:
                    raise Refusal('optimization changed a matched measurement control: ' + field)
        if mode != 'cold':
            previous = next(row for row in reversed(rows) if row['kind'] == 'start')
            if previous['key'] != key:
                raise Refusal('warm/reuse inputs, toolchain, environment, configuration or target changed')
        if mode == 'reuse':
            warm = rows[-1]
            if warm.get('accepted') is not True or warm.get('mode') != 'warm':
                raise Refusal('reuse requires the immediately preceding successful warm execution')
        row = {'version': 2, 'kind': 'start', 'slot': slot, 'block': slot // 3, 'mode': mode,
               'key': key, 'identity': identity, 'ceiling_seconds': ceiling}
        journal(self.path, row)
        return row

    def finish(self, *, exit_code, settled, executed, reused, elapsed, telemetry):
        rows, pending, _ = self.history()
        if pending is None:
            raise Refusal('no started measurement to finish')
        from gate_measurement_data import finite_seconds
        finite_seconds(elapsed)
        if type(exit_code) is not int or type(settled) is not bool:
            raise Refusal('measurement exit/settlement evidence is missing')
        expected = pending['identity']['applicable_legs']
        coverage = (executed == expected and reused == []) if pending['mode'] != 'reuse' else (reused == expected and executed == [])
        telemetry_complete = completeness(telemetry)
        accepted = telemetry_complete and exit_code == 0 and settled and coverage and elapsed <= pending['ceiling_seconds']
        row = {'version': 2, 'telemetry': telemetry, 'kind': 'finish', 'slot': pending['slot'], 'mode': pending['mode'],
               'exit_code': exit_code, 'settled': settled, 'executed': executed, 'reused': reused,
               'admission_to_settlement_seconds': elapsed, 'production_target_breach': elapsed >= 900,
               'accepted': accepted, 'production_certification': False}
        journal(self.path, row)
        return row

    def reuse_key(self, identity, leg):
        """Return only the key earned by W for the currently started R observation."""
        rows, pending, _ = self.history()
        if not pending or pending['mode'] != 'reuse' or fingerprint(identity) != pending['key']:
            raise Refusal('no exact current measurement reuse authority')
        if leg not in identity['applicable_legs']:
            raise Refusal('leg was not covered by the warm execution')
        warm = rows[-2]
        if warm['kind'] != 'finish' or warm.get('accepted') is not True or warm['mode'] != 'warm':
            raise Refusal('no successful immediately preceding warm execution')
        return pending['key'] + ':' + leg

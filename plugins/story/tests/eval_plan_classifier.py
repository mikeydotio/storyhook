"""Opt-in live Luna evaluation; uses existing Codex login and the production runner."""
import json
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'hooks'))
from codex_classifier import classify


def main():
    """Check representative positives and permission boundaries against real Luna."""
    cases = json.loads(Path(__file__).with_name('plan-classifier-cases.json').read_text())
    failures = 0
    for case in cases:
        start = time.monotonic()
        try:
            answer = classify(case['message'])
            passed = (answer.get('decision') != 'approve_plan' if case['expected'] == 'not_approval'
                      else answer.get('decision') == case['expected'])
            if answer.get('decision') == 'approve_plan':
                passed = passed and bool(answer.get('evidence')) and answer['evidence'] in case['message']
        except (OSError, ValueError, RuntimeError) as exc:
            answer, passed = {'error': str(exc)}, False
        failures += not passed
        print(json.dumps({'case': case['name'], 'passed': passed, 'answer': answer,
                          'seconds': round(time.monotonic() - start, 2)}), flush=True)
    return bool(failures)


if __name__ == '__main__':
    sys.exit(main())

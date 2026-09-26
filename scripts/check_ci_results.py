#!/usr/bin/env python3
"""Require the planned CI legs to pass with matching reports and every other leg to be skipped."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
POLICY = ROOT / 'scripts' / 'ci-policy.json'
SCHEMA = 'codespace-ci-plan/v1'
REQUIRED_JOBS = {'plan', 'policy-scan', 'rust-format', 'rust-clippy', 'rust-unit',
                 'rust-linux-isolation', 'rust-integration', 'rust-macos'}
FULL_EVENTS = {'schedule', 'workflow_dispatch'}


def artifacts(reports, leg, attempt):
    """Report artifacts of one leg from this and earlier run attempts, oldest first."""
    job, name = leg.split('/')
    pattern = re.compile(re.escape(f'upstream-{job}-{name}-') + '([0-9]+)')
    found = []
    for path in reports.iterdir() if reports is not None and reports.is_dir() else []:
        match = pattern.fullmatch(path.name)
        if match and int(match.group(1)) <= attempt:
            found.append((int(match.group(1)), path))
    return sorted(found)


def report_problems(leg, found, stages, source):
    if not found:
        return [f'{leg}: no uploaded report']
    files = list(found[-1][1].rglob('report.json'))
    if len(files) != 1:
        return [f'{leg}: expected one report.json, found {len(files)}']
    report = json.loads(files[0].read_text())
    steps = report.get('stages') or []
    if (report.get('status') != 'passed' or report.get('scope') != stages
            or [step.get('name') for step in steps] != stages
            or any(step.get('status') != 'passed' for step in steps)
            or (report.get('inputs') or {}).get('head') != source):
        return [f'{leg}: report does not show {" ".join(stages)} passing at {source[:12]}']
    return []


def problems(results, plan, policy, digest, env, reports):
    """Every way this run differs from its plan; an empty list means the gate passes."""
    if not isinstance(results, dict) or set(results) != REQUIRED_JOBS:
        return ['the gate does not see exactly the required jobs']
    source, legs, selected = env.get('GITHUB_SHA', ''), policy['legs'], plan.get('legs')
    if (plan.get('schema') != SCHEMA or not re.fullmatch('[0-9a-f]{40}', source)
            or plan.get('source_sha') != source or plan.get('policy_sha256') != digest
            or plan.get('event') != env.get('GITHUB_EVENT_NAME')
            or plan.get('profile') not in {'full', 'affected'}
            or not isinstance(selected, list) or len(set(selected)) != len(selected) or not set(selected) <= set(legs)
            or plan.get('jobs') != sorted({leg.split('/')[0] for leg in selected})
            or (plan['profile'] == 'full' and selected != list(legs))
            or (plan['event'] in FULL_EVENTS and plan['profile'] != 'full')):
        return ['the plan is not bound to this source, event and policy']
    always = {leg.split('/')[0] for leg in policy['always']}
    found = []
    for name in sorted(REQUIRED_JOBS):
        result = (results[name] or {}).get('result')
        expected = 'success' if name == 'plan' or name in always or name in plan['jobs'] else 'skipped'
        if result != expected:
            found.append(f'{name}: {result}, expected {expected}')
    attempt = int(env.get('GITHUB_RUN_ATTEMPT') or 1)
    planned = dict(policy['always'], **{leg: legs[leg]['stages'] for leg in selected})
    for leg in [*policy['always'], *legs]:
        runs = artifacts(reports, leg, attempt)
        if leg in planned:
            found += report_problems(leg, runs, planned[leg], source)
        elif runs:
            found.append(f'{leg}: uploaded a report although it was not planned')
    return found


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reports', type=Path, required=True, help='directory of downloaded upstream-* artifacts')
    args = parser.parse_args(argv)
    env = os.environ
    try:
        raw = POLICY.read_bytes()
        found = problems(json.loads(env['CI_RESULTS']), json.loads(env['CI_PLAN']), json.loads(raw),
                         hashlib.sha256(raw).hexdigest(), env, args.reports)
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        found = [f'cannot evaluate the CI results: {error}']
    text = '\n'.join(['Required CI: ' + ('failed' if found else 'passed')] + ['- ' + line for line in found])
    print(text)
    if env.get('GITHUB_STEP_SUMMARY'):
        with open(env['GITHUB_STEP_SUMMARY'], 'a') as handle:
            handle.write(text + '\n')
    raise SystemExit(1 if found else 0)


if __name__ == '__main__':
    main()

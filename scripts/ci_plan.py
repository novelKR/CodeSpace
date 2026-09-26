#!/usr/bin/env python3
"""Select the CI legs a change needs and bind the plan to the checked-out source.

Changed paths are matched against scripts/ci-policy.json. Build inputs, CI
files and unknown paths select every leg; documentation selects none; a crate
selects every leg that compiles it. Scheduled and manual runs are full.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
POLICY = 'scripts/ci-policy.json'
# A change to any planning input runs everything, so a pull request cannot
# narrow the checks of its own run.
PLANNING = ('.github/workflows/ci.yml', POLICY, 'scripts/ci_plan.py', 'scripts/check_ci_results.py')
SCHEMA = 'codespace-ci-plan/v1'
TARGET = 'target/upstream-validation'
SHA = re.compile('[0-9a-f]{40}')
GLOB = {'**/': '(?:.*/)?', '**': '.*', '*': '[^/]*', '?': '[^/]'}


class PlanError(Exception):
    pass


def git(root, *args):
    return subprocess.run(['git', *args], cwd=root, check=True, capture_output=True).stdout


def read_policy(root=ROOT):
    raw = (root / POLICY).read_bytes()
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def matches(patterns, path):
    """`*` stays inside one path segment; `**` spans segments."""
    for pattern in patterns:
        parts = re.split(r'(\*\*/|\*\*|\*|\?)', pattern)
        if re.fullmatch(''.join(GLOB.get(part, re.escape(part)) for part in parts), path):
            return True
    return False


def classify(policy, paths):
    """Return the changed components and the reasons that force a full run."""
    components, reasons, unknown = set(), set(), []
    for path in paths:
        full = [pattern for pattern in policy['full'] if matches([pattern], path)]
        if full:
            reasons.add('full-path:' + full[0])
        elif not matches(policy['no_rust'], path):
            owners = {name for name, patterns in policy['components'].items() if matches(patterns, path)}
            components |= owners
            if not owners:
                unknown.append(path)
    reasons |= {'unclassified:' + path for path in unknown[:10]}
    return components, reasons


def build_plan(policy, digest, event, source, base, paths, reasons):
    components, more = classify(policy, paths)
    reasons = set(reasons) | more
    if base is not None and not paths:
        reasons.add('empty-diff')
    legs = [leg for leg, spec in policy['legs'].items() if reasons or components & set(spec['compiles'])]
    return {'schema': SCHEMA, 'event': event, 'source_sha': source, 'base_sha': base,
            'policy_sha256': digest, 'profile': 'full' if reasons else 'affected',
            'reasons': sorted(reasons), 'components': sorted(components),
            'legs': legs, 'jobs': sorted({leg.split('/')[0] for leg in legs})}


def planning_changed(root, base, head):
    for path in PLANNING:
        try:
            if git(root, 'show', f'{base}:{path}') != git(root, 'show', f'{head}:{path}'):
                return True
        except subprocess.CalledProcessError:
            return True
    return False


def changed_paths(root, base, head):
    out = git(root, 'diff', '--name-only', '--no-renames', '-z', base, head)
    return sorted(path for path in out.decode().split('\0') if path)


def prepare(root, env, event):
    name = env['GITHUB_EVENT_NAME']
    source = git(root, 'rev-parse', 'HEAD').decode().strip()
    if not SHA.fullmatch(source) or source != env['GITHUB_SHA']:
        raise PlanError('the checkout is not GITHUB_SHA')
    policy, digest = read_policy(root)
    if name in ('schedule', 'workflow_dispatch'):
        return build_plan(policy, digest, name, source, None, [], {'event:' + name})
    if name == 'pull_request':
        parents = git(root, 'rev-list', '--parents', '-n', '1', source).decode().split()
        if len(parents) != 3 or parents[2] != event['pull_request']['head']['sha']:
            raise PlanError('the checkout is not the merge of the pull request head')
        base = parents[1]
    elif name == 'push':
        base = event.get('before') or ''
        ancestor = SHA.fullmatch(base) and subprocess.run(
            ['git', 'merge-base', '--is-ancestor', base, source], cwd=root, capture_output=True).returncode == 0
        if not ancestor or event.get('forced') or base == '0' * 40:
            return build_plan(policy, digest, name, source, None, [], {'push-base-untrusted'})
    else:
        raise PlanError('unsupported event: ' + name)
    reasons = {'planning-changed'} if planning_changed(root, base, source) else set()
    return build_plan(policy, digest, name, source, base, changed_paths(root, base, source), reasons)


def anchor(workspace):
    return f'{workspace} -> {os.path.relpath(TARGET, workspace)}'


def outputs(policy, plan):
    """GITHUB_OUTPUT lines: the plan, one flag per job and a matrix per matrix job."""
    lines = ['plan=' + json.dumps(plan, separators=(',', ':'), sort_keys=True)]
    jobs = {}
    for leg in policy['legs']:
        job, name = leg.split('/')
        jobs.setdefault(job, []).append(name)
    for job, names in jobs.items():
        lines.append(f'{job}=' + str(job in plan['jobs']).lower())
        if names != ['single']:
            include = [{'name': name, 'stage': ' '.join(policy['legs'][f'{job}/{name}']['stages']),
                        'cache-workspace': anchor(policy['legs'][f'{job}/{name}']['cache'])}
                       for name in names if f'{job}/{name}' in plan['legs']]
            lines.append(f'{job}-matrix=' + json.dumps({'include': include}, separators=(',', ':')))
    return lines


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--base', help='print the plan for the committed diff BASE..HEAD instead')
    parser.add_argument('--head', default='HEAD')
    args = parser.parse_args(argv)
    try:
        policy, digest = read_policy()
        if args.base:
            base, head = (git(ROOT, 'rev-parse', '--verify', rev + '^{commit}').decode().strip()
                          for rev in (args.base, args.head))
            reasons = {'planning-changed'} if planning_changed(ROOT, base, head) else set()
            plan = build_plan(policy, digest, 'local', head, base, changed_paths(ROOT, base, head), reasons)
            print(json.dumps(plan, indent=2))
            return
        plan = prepare(ROOT, os.environ, json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text()))
        out = ROOT / 'target' / 'ci-plan'
        out.mkdir(parents=True, exist_ok=True)
        (out / 'plan.json').write_text(json.dumps(plan, indent=2) + '\n')
        with open(os.environ['GITHUB_OUTPUT'], 'a') as handle:
            handle.write('\n'.join(outputs(policy, plan)) + '\n')
        if os.environ.get('GITHUB_STEP_SUMMARY'):
            with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as handle:
                handle.write('### CI plan\n\n```json\n' + json.dumps(plan, indent=2) + '\n```\n')
        print(f"CI plan: {plan['profile']}, {len(plan['legs'])} of {len(policy['legs'])} legs")
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError, PlanError) as error:
        raise SystemExit(f'CI planning failed ({error}); no reduced coverage is authorized')


if __name__ == '__main__':
    main()

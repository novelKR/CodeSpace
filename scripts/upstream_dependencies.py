#!/usr/bin/env python3
"""Inspect target-filtered Cargo product graphs, excluding development edges."""
import argparse
from collections import deque
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
ADAPTERS = ('patch', 'codex-runtime', 'pty', 'file-system', 'linux-sandbox')
PRODUCTS = {'root': {'codespace-domain', 'codespace-policy', 'codespace-runner', 'codespace-store', 'codespace-server', 'codespace-linux-sandbox-protocol'},
            'patch': {'codespace-patch'}, 'codex-runtime': {'codespace-codex-runtime'},
            'pty': {'codespace-pty'}, 'file-system': {'codespace-fs'}, 'linux-sandbox': {'codespace-linux-sandbox'}}
FORBIDDEN = {'codex-core', 'codex-exec', 'codex-app-server', 'codex-login'}
RUNNER_FORBIDDEN = {'codespace-linux-sandbox', 'codex-linux-sandbox'}


def metadata(manifest, target):
    return json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--format-version', '1',
         '--filter-platform', target, '--manifest-path', str(manifest)], cwd=ROOT))


def graph(data, roots):
    packages = {p['id']: p for p in data['packages']}
    nodes = {n['id']: n for n in data['resolve']['nodes']}
    if not roots or any(r not in packages or r not in nodes for r in roots):
        raise ValueError('missing product root or resolve node')

    def key(pid):
        p = packages[pid]
        source = p['source']
        if source is None:
            path = Path(p['manifest_path']).resolve().parent
            source = 'path:' + path.relative_to(ROOT).as_posix()
        return json.dumps([p['name'], p['version'], source], separators=(',', ':'))

    found, edges, violations = set(), set(), []
    for root in roots:
        queue, seen = deque([(root, [root])]), set()
        banned = FORBIDDEN | (RUNNER_FORBIDDEN if packages[root]['name'] == 'codespace-runner' else set())
        while queue:
            pid, trail = queue.popleft()
            if pid in seen:
                continue
            seen.add(pid)
            found.add(key(pid))
            if packages[pid]['name'] in banned:
                violations.append(' -> '.join(packages[x]['name'] for x in trail))
            for dep in nodes[pid]['deps']:
                for kind in dep['dep_kinds']:
                    if kind['kind'] not in (None, 'normal', 'build'):
                        continue
                    child = dep['pkg']
                    if child not in nodes or child not in packages:
                        raise ValueError('unresolved dependency node')
                    edges.add((key(pid), key(child), kind['kind'] or 'normal', kind.get('target') or ''))
                    queue.append((child, trail + [child]))
    return {'packages': sorted(found), 'edges': [list(e) for e in sorted(edges)],
            'violations': sorted(set(violations))}


def difference(old, new):
    if old.get('schema') != new.get('schema') or old.get('target') != new.get('target'):
        raise ValueError('baseline schema/target mismatch')
    if old['graphs'].keys() != new['graphs'].keys():
        raise ValueError('baseline product roots mismatch')
    result = {}
    for name, current in new['graphs'].items():
        previous = old['graphs'][name]
        changes = {}
        for field in ('packages', 'edges'):
            before = {json.dumps(v, sort_keys=True) for v in previous[field]}
            after = {json.dumps(v, sort_keys=True) for v in current[field]}
            changes[field] = {'added': [json.loads(v) for v in sorted(after-before)],
                              'removed': [json.loads(v) for v in sorted(before-after)]}
        result[name] = changes
    return result


def inspect(target):
    report = {'schema': 1, 'target': target, 'graphs': {}}
    for area in ('root',) + ADAPTERS:
        manifest = ROOT / ('Cargo.toml' if area == 'root' else f'crates/{area}/Cargo.toml')
        data = metadata(manifest, target)
        members = {p['name']: p['id'] for p in data['packages'] if p['id'] in data['workspace_members']}
        if set(members) != PRODUCTS[area]:
            raise ValueError(f'{area}: missing or unexpected product roots: {sorted(members)}')
        report['graphs'][area] = graph(data, [members[name] for name in sorted(PRODUCTS[area])])
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--compare', type=Path)
    args = parser.parse_args()
    report = {'schema': 1, 'target': args.target, 'graphs': {}}
    code = 1
    try:
        if args.compare and args.compare.resolve() == args.output.resolve():
            raise ValueError('output must not overwrite baseline')
        report = inspect(args.target)
        if args.compare:
            report['comparison'] = difference(json.loads(args.compare.read_text()), report)
        code = int(any(g['violations'] for g in report['graphs'].values()))
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError) as error:
        report['error'] = str(error)
    if args.compare and args.compare.resolve() == args.output.resolve():
        print(report['error'], file=sys.stderr)
        return 1
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + '\n')
    if code:
        print(json.dumps(report, indent=2), file=sys.stderr)
    return code


if __name__ == '__main__':
    sys.exit(main())

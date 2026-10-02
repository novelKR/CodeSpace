#!/usr/bin/env python3
"""Shared local/CI upstream gates. No source or lockfile updates."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time

from upstream_dependencies import ADAPTERS, ROOT


def capture(*cmd):
    return subprocess.check_output(cmd, cwd=ROOT).decode().strip()


def fingerprint():
    paths = subprocess.check_output(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], cwd=ROOT).split(b'\0')
    hashes = {}
    for raw in paths:
        if not raw:
            continue
        name = os.fsdecode(raw)
        path = ROOT / name
        if path.is_file():
            hashes[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    return {'head': capture('git', 'rev-parse', 'HEAD'),
            'codex_sha': capture('git', '-C', 'third_party/codex', 'rev-parse', 'HEAD') if (ROOT / 'third_party/codex/.git').exists() else None,
            'codex_status': capture('git', '-C', 'third_party/codex', 'status', '--porcelain') if (ROOT / 'third_party/codex/.git').exists() else 'not initialized',
            'codex_diff': hashlib.sha256(subprocess.check_output(['git', '-C', 'third_party/codex', 'diff', '--binary', 'HEAD'], cwd=ROOT)).hexdigest() if (ROOT / 'third_party/codex/.git').exists() else None,
            'files': hashes}


def cargo(action, area='root', *args):
    cmd = ['cargo', action]
    if action != 'fmt':
        cmd += ['--locked']
    if area != 'root':
        cmd += ['--manifest-path', f'crates/{area}/Cargo.toml']
    return cmd + list(args)


def stages():
    result = {
        'pin': [['bash', 'scripts/check-upstream-pin.sh']],
        'policy': [['bash', 'scripts/check-no-model-deps.sh']],
        'python': [[sys.executable, '-m', 'unittest', 'discover', '-s', 'scripts/tests']],
        'format': [cargo('fmt', area, '--check') for area in ('root',) + ADAPTERS],
        'clippy-root': [cargo('clippy', 'root', '--all-targets', '--', '-D', 'warnings'),
                        # The gateway with its opt-in DevGuard status connection (CSRG-U1).
                        cargo('clippy', 'root', '-p', 'codespace-server', '--features', 'devguard', '--all-targets', '--', '-D', 'warnings')],
        'clippy-adapters': [cargo('clippy', a, '--all-targets', '--', '-D', 'warnings') for a in ('patch', 'pty', 'file-system', 'devguard')],
        'clippy-codex': [cargo('clippy', a, '--all-targets', '--', '-D', 'warnings') for a in ('codex-runtime', 'linux-sandbox')]
        # The worker with its own DevGuard registration (CSRG-U2).
        + [cargo('clippy', 'codex-runtime', '--features', 'devguard', '--all-targets', '--', '-D', 'warnings')],
        'macos-core': [cargo(action, a, *(['--all-targets', '--', '-D', 'warnings'] if action == 'clippy' else [])) for a in ('pty', 'file-system') for action in ('clippy', 'test')]
        # Descriptor hygiene of the runner host's spawns, whose macOS path the Linux legs cannot exercise.
        + [cargo('test', 'root', '-p', 'codespace-runner', '--lib', '--', 'descriptors', 'missing_executable_is_process_spawn_failed'),
           cargo('test', 'root', '-p', 'codespace-server', '--lib', '--', 'descriptors')]
        # The DevGuard adapter against an authority with native host evidence, and the DevGuard
        # tests of the runner, the worker and the gateway where socket creation is not atomically
        # close-on-exec, the gateway's against the worker and the fixture authority built here.
        + devguard_binaries()
        + [cargo('test', 'devguard'),
           cargo('test', 'codex-runtime', '--features', 'devguard', '--bin', 'codespace-codex-runtime'),
           cargo('test', 'root', '-p', 'codespace-runner', '--features', 'devguard', '--lib', '--', 'registration', 'wire'),
           cargo('test', 'root', '-p', 'codespace-server', '--features', 'devguard', '--lib', '--', 'devguard')],
        'integration': [],
        'linux-isolation': [cargo('build', 'linux-sandbox', '--bin', 'codespace-linux-sandbox'), cargo('test', 'linux-sandbox', '--test', 'isolation')],
        'dependencies': [],
    }
    for area in ADAPTERS:
        extra = ['--bins', '--test', 'cli'] if area == 'linux-sandbox' else []
        result['unit-' + area] = [cargo('test', area, *extra)]
    result['unit-codex-runtime'].append(cargo('test', 'codex-runtime', '--features', 'devguard', '--bin', 'codespace-codex-runtime'))
    result['unit-linux-sandbox-protocol'] = [cargo('test', 'root', '-p', 'codespace-linux-sandbox-protocol')]
    return result


HELPERS = {'patch': ('codespace-patch', 'CODESPACE_PATCH_BIN'),
           'codex-runtime': ('codespace-codex-runtime', 'CODESPACE_RUNTIME_BIN'),
           'linux-sandbox': ('codespace-linux-sandbox', 'CODESPACE_LINUX_SANDBOX_BIN')}
TARGET_DIR = (ROOT / 'target' / 'upstream-validation').resolve()
# The gateway's DevGuard registration tests (CSRG-U2) need the worker built with its DevGuard
# feature, kept beside the default worker, and DevGuard's fixture authority in its own process.
DEVGUARD_RUNTIME = 'codespace-codex-runtime-devguard'
DEVGUARD_FIXTURE = 'examples/fixture_authority'
DEVGUARD_STAGES = ('integration', 'macos-core')


def helper_env(target_dir):
    return {variable: str(target_dir / 'debug' / binary) for binary, variable in HELPERS.values()}


def devguard_env(target_dir):
    """Where the stages that build them find the DevGuard test binaries. With them named, a
    test that needs one fails instead of skipping."""
    return {'CODESPACE_DEVGUARD_RUNTIME_BIN': str(target_dir / 'debug' / DEVGUARD_RUNTIME),
            'CODESPACE_DEVGUARD_FIXTURE_BIN': str(target_dir / 'debug' / DEVGUARD_FIXTURE),
            'CODESPACE_REQUIRE_DEVGUARD_BINS': '1'}


def devguard_binaries():
    """Build the worker with its DevGuard feature and copy it beside the default worker, whose
    build replaces that path, then the fixture authority."""
    worker = TARGET_DIR / 'debug' / 'codespace-codex-runtime'
    return [cargo('build', 'codex-runtime', '--features', 'devguard', '--bin', 'codespace-codex-runtime'),
            ['cp', str(worker), str(TARGET_DIR / 'debug' / DEVGUARD_RUNTIME)],
            cargo('build', 'devguard', '--example', 'fixture_authority')]


def execute(commands, env, log):
    for command in commands:
        print('+ ' + ' '.join(command), flush=True)
        log.write('+ ' + ' '.join(command) + '\n')
        log.flush()
        status = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT).returncode
        if status:
            raise RuntimeError(f'command exited {status}: {command}')


def run(selected, output):
    output.mkdir(parents=True, exist_ok=True)
    report = {'schema': 1, 'platform': platform.platform(), 'stages': [], 'status': 'failed'}
    failed = False
    try:
        before = fingerprint()
        report['inputs'] = before
        report['rust'] = capture('rustc', '-vV')
        report['build_environment'] = {k: os.environ.get(k) for k in ('DEVELOPER_DIR', 'SDKROOT', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTUP_TOOLCHAIN', 'CARGO_INCREMENTAL', 'CARGO_PROFILE_DEV_DEBUG')}
        if before.get('codex_sha') is None and any(s not in ('policy', 'python') for s in selected):
            raise RuntimeError('Codex submodule is not initialized')
        target = next(line.split(': ', 1)[1] for line in report['rust'].splitlines() if line.startswith('host: '))
        env = os.environ.copy()
        env['PIN_ONLY'] = '1'
        # Full policy scan for reproducible standalone and CI qualification.
        env.pop('SCAN_BASE', None)
        target_dir = TARGET_DIR
        env['CARGO_TARGET_DIR'] = str(target_dir)
        env.update(helper_env(target_dir))
        for stage in selected:
            entry = {'name': stage, 'status': 'failed'}
            report['stages'].append(entry)
            if stage == 'linux-isolation' and platform.system() != 'Linux':
                entry.update(status='not_run', reason='requires Linux with bubblewrap and user namespaces')
                continue
            started = time.monotonic()
            try:
                commands = stages()[stage]
                if stage == 'dependencies':
                    commands = [[sys.executable, 'scripts/upstream_dependencies.py', '--target', target, '--output', str(output / 'dependencies.json')]]
                elif stage == 'integration':
                    commands = devguard_binaries()
                    commands += [cargo('build', area, '--bin', binary) for area, (binary, _) in HELPERS.items()]
                    commands += [cargo('test', 'root', '--workspace'),
                                 cargo('test', 'root', '-p', 'codespace-server', '--features', 'devguard')]
                stage_env = env.copy()
                if stage == 'linux-isolation':
                    stage_env['CODESPACE_REQUIRE_LINUX_SANDBOX'] = '1'
                if stage in DEVGUARD_STAGES:
                    stage_env.update(devguard_env(target_dir))
                with (output / (stage + '.log')).open('w') as log:
                    execute(commands, stage_env, log)
                entry['status'] = 'passed'
            except (OSError, RuntimeError) as error:
                entry['error'] = str(error)
                failed = True
            entry['seconds'] = round(time.monotonic() - started, 2)
        if fingerprint() != before:
            raise RuntimeError('source or validation inputs changed during execution')
        report['status'] = 'failed' if failed else ('incomplete' if any(s['status'] == 'not_run' for s in report['stages']) else 'passed')
        report['scope'] = selected
        report['full_linux_qualification'] = not failed and platform.system() == 'Linux' and set(all_stages()) <= set(selected)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, StopIteration) as error:
        report['error'] = str(error)
        failed = True
    finally:
        (output / 'report.json').write_text(json.dumps(report, indent=2, sort_keys=True) + '\n')
    print(json.dumps({k: report[k] for k in ('status', 'stages')}, indent=2))
    return int(failed)


def all_stages():
    return [s for s in stages() if s != 'macos-core']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('stage', nargs='+', choices=['all'] + list(stages()))
    parser.add_argument('--output', type=Path, default=ROOT / 'target/upstream-reports/local')
    args = parser.parse_args()
    selected = all_stages() if 'all' in args.stage else list(dict.fromkeys(args.stage))
    # Reports must be ignored; otherwise they would invalidate the source snapshot themselves.
    output = args.output.resolve()
    if subprocess.run(['git', 'check-ignore', '-q', str(output / 'report.json')], cwd=ROOT).returncode != 0:
        parser.error('--output must be inside a git-ignored directory (e.g. target/upstream-reports)')
    return run(selected, output)


if __name__ == '__main__':
    sys.exit(main())

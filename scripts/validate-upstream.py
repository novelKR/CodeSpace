#!/usr/bin/env python3
"""Shared local/CI upstream gates. No source or lockfile updates, except the recorded Codex
patches, which are applied to a clean checkout of the pinned commit before anything runs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import time

import codex_patch
from upstream_dependencies import ADAPTERS, DEVGUARD_SOURCE, ROOT


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
            # The checkout's files as built, untracked ones included: the recorded patched tree.
            'codex_tree': codex_patch.working_tree(ROOT / 'third_party/codex') if (ROOT / 'third_party/codex/.git').exists() else None,
            'files': hashes}


def apply_codex_patches():
    """Apply the patches docs/upstream-lock.md records to the pinned Codex checkout, or confirm
    they are applied. Any other state of the checkout fails the run."""
    if not (ROOT / 'third_party/codex/.git').exists():
        return None
    return codex_patch.apply(codex_patch.read_record())


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
                        # The gateway with its opt-in DevGuard connection (CSRG-U1, U2), and the
                        # domain types and runner registration that its feature turns on.
                        cargo('clippy', 'root', '-p', 'codespace-server', '--features', 'devguard', '--all-targets', '--', '-D', 'warnings'),
                        cargo('clippy', 'root', '-p', 'codespace-runner', '--features', 'devguard', '--all-targets', '--', '-D', 'warnings'),
                        cargo('clippy', 'root', '-p', 'codespace-domain', '--features', 'devguard', '--all-targets', '--', '-D', 'warnings')],
        'clippy-adapters': [cargo('clippy', a, '--all-targets', '--', '-D', 'warnings') for a in ('patch', 'pty', 'file-system', 'devguard')],
        'clippy-codex': [cargo('clippy', a, '--all-targets', '--', '-D', 'warnings') for a in ('codex-runtime', 'linux-sandbox')]
        # The worker with its own DevGuard registration (CSRG-U2).
        + [cargo('clippy', 'codex-runtime', '--features', 'devguard', '--all-targets', '--', '-D', 'warnings')],
        'macos-core': [cargo(action, a, *(['--all-targets', '--', '-D', 'warnings'] if action == 'clippy' else [])) for a in ('pty', 'file-system') for action in ('clippy', 'test')]
        # Descriptor hygiene of the runner host's spawns and their fork-time libnotify state, whose
        # macOS path the Linux legs cannot exercise.
        + [cargo('test', 'root', '-p', 'codespace-runner', '--lib', '--', 'descriptors', 'fork_handlers', 'missing_executable_is_process_spawn_failed'),
           cargo('test', 'root', '-p', 'codespace-server', '--lib', '--', 'descriptors')]
        # The process records of the gateway with an independent Runner (P1R-C02): the operations
        # database's schema version, its records and its single writer's lock, and the root
        # process that a spawn reports with its macOS start identity.
        + [cargo('test', 'root', '-p', 'codespace-store', '--lib'),
           cargo('test', 'root', '-p', 'codespace-runner', '--lib', '--', 'a_spawn_reports_its_root')]
        # The DevGuard adapter against an authority with native host evidence and DevGuard's
        # launch helper, and the DevGuard tests of the runner (registration, its wire, the
        # preparation of governed executions, CSRG-U3, and their launch, CSRG-U4), the worker
        # and the gateway where socket creation is not atomically close-on-exec, the gateway's
        # against the worker, the fixture authority and the helper built here. With them, the
        # independent Runner (P1R-C01): its identity, socket directory and wire, its Claim with
        # the peer credentials that macOS reads through `getpeereid` (P1R-C03), the gateway's
        # settings, refusals and reconnect, the operator's records commands, which read the
        # host's boot session and whether a process still runs as itself (P1R-C05), then the
        # worker binary and the gateway binary against it, with the default worker built again
        # over the DevGuard one.
        + devguard_binaries()
        + [cargo('test', 'devguard'),
           cargo('test', 'codex-runtime', '--features', 'devguard', '--bin', 'codespace-codex-runtime'),
           cargo('test', 'root', '-p', 'codespace-runner', '--features', 'devguard', '--lib', '--', 'registration', 'wire', 'admission', 'launch', 'independent', 'lifecycle'),
           cargo('test', 'root', '-p', 'codespace-server', '--features', 'devguard', '--lib', '--', 'devguard', 'independent', 'operator'),
           cargo('build', 'codex-runtime', '--bin', 'codespace-codex-runtime'),
           cargo('test', 'codex-runtime', '--test', 'runtime_binary', '--', 'independent'),
           cargo('test', 'root', '-p', 'codespace-server', '--features', 'devguard', '--test', 'independent_runner')],
        # Children killed inside fork on macOS: fresh-process trials of reads concurrent with
        # pipe spawns (crates/runner/tests/fork_race.rs); any child without an exit code fails.
        'macos-fork-race': [cargo('test', 'root', '-p', 'codespace-runner', '--test', 'fork_race', '--',
                                  '--trials', FORK_RACE_TRIALS, '--require-zero')],
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


# Chosen from the failure rate measured before the fix (see the fork_race harness).
FORK_RACE_TRIALS = '400'
# Stages that only the macOS leg runs; `all` is the Linux qualification.
MACOS_ONLY = ('macos-core', 'macos-fork-race')

HELPERS = {'patch': ('codespace-patch', 'CODESPACE_PATCH_BIN'),
           'codex-runtime': ('codespace-codex-runtime', 'CODESPACE_RUNTIME_BIN'),
           'linux-sandbox': ('codespace-linux-sandbox', 'CODESPACE_LINUX_SANDBOX_BIN')}
TARGET_DIR = (ROOT / 'target' / 'upstream-validation').resolve()
# The gateway's DevGuard registration tests (CSRG-U2) need the worker built with its DevGuard
# feature, kept beside the default worker, and DevGuard's fixture authority in its own process;
# the launch tests (CSRG-U4) need DevGuard's launch helper.
DEVGUARD_RUNTIME = 'codespace-codex-runtime-devguard'
DEVGUARD_FIXTURE = 'examples/fixture_authority'
DEVGUARD_LAUNCH = 'devguard-launch'
DEVGUARD_STAGES = ('integration', 'macos-core')


def helper_env(target_dir):
    return {variable: str(target_dir / 'debug' / binary) for binary, variable in HELPERS.values()}


def devguard_env(target_dir):
    """Where the stages that build them find the DevGuard test binaries. With them named, a
    test that needs one fails instead of skipping."""
    return {'CODESPACE_DEVGUARD_RUNTIME_BIN': str(target_dir / 'debug' / DEVGUARD_RUNTIME),
            'CODESPACE_DEVGUARD_FIXTURE_BIN': str(target_dir / 'debug' / DEVGUARD_FIXTURE),
            'CODESPACE_DEVGUARD_LAUNCH_BIN': str(target_dir / DEVGUARD_LAUNCH / 'bin' / DEVGUARD_LAUNCH),
            'CODESPACE_REQUIRE_DEVGUARD_BINS': '1'}


def devguard_pin():
    """The reviewed DevGuard repository and commit that every `devguard-*` crate comes from."""
    url, rest = DEVGUARD_SOURCE.removeprefix('git+').split('?rev=', 1)
    return url, rest.split('#', 1)[0]


def devguard_binaries():
    """Build the worker with its DevGuard feature and copy it beside the default worker, whose
    build replaces that path, then the fixture authority, then DevGuard's launch helper from the
    reviewed pin with DevGuard's own lockfile. The helper is a test binary here: no CodeSpace
    product links or ships it."""
    worker = TARGET_DIR / 'debug' / 'codespace-codex-runtime'
    url, rev = devguard_pin()
    return [cargo('build', 'codex-runtime', '--features', 'devguard', '--bin', 'codespace-codex-runtime'),
            ['cp', str(worker), str(TARGET_DIR / 'debug' / DEVGUARD_RUNTIME)],
            cargo('build', 'devguard', '--example', 'fixture_authority'),
            ['cargo', 'install', '--locked', '--git', url, '--rev', rev, DEVGUARD_LAUNCH,
             '--root', str(TARGET_DIR / DEVGUARD_LAUNCH), '--target-dir', str(TARGET_DIR / (DEVGUARD_LAUNCH + '-build'))]]


def execute(commands, env, log):
    for command in commands:
        print('+ ' + ' '.join(command), flush=True)
        log.write('+ ' + ' '.join(command) + '\n')
        log.flush()
        status = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT).returncode
        if status:
            raise RuntimeError(f'command exited {status}: {command}')


# The lines of a failed stage's log that name what failed: the failed tests and where they
# panicked, a test binary's counts and the target that cargo or unittest failed. They are also
# printed, so a CI job log names them without the uploaded report. A test's own output is never
# repeated: its messages can carry a child's environment.
FAILURE_LINE = re.compile('|'.join([
    r'test \S.* \.\.\. FAILED',
    r'test \S.* has been running for over \d+ seconds',
    r"thread '[^']*'(?: \(\d+\))? panicked at [^\s']+:\d+:\d+:",
    r'test result: FAILED\. \d+ passed; \d+ failed; \d+ ignored; \d+ measured; \d+ filtered out; finished in [\d.]+s',
    r'error: (?:doc)?test failed, to rerun pass `[^`]*`',
    r'error: could not compile `[\w-]+`[^`]*',
    r"process didn't exit successfully: `[^`]*` \([^()]*\)",
    r'(?:FAIL|ERROR): \w+ \([\w.]+\)(?: \([^()]*\))?',
    r'FAILED \([\w ]+=\d+(?:, [\w ]+=\d+)*\)']))
# Enough for one test binary's failures; the log keeps the rest.
FAILURES_SHOWN = 40
# The terminal colors of cargo's own lines, which CI asks for with CARGO_TERM_COLOR.
COLOR = re.compile(r'\x1b\[[0-9;]*m')


def failure_lines(path):
    """The lines of a stage's log that FAILURE_LINE matches without their colors, each once, in
    order."""
    with path.open(encoding='utf-8', errors='replace') as log:
        lines = (COLOR.sub('', line).strip() for line in log)
        return list(dict.fromkeys(line for line in lines if FAILURE_LINE.fullmatch(line)))


def print_failures(stage, path):
    """Print what a failed stage's log names as failed. A log that cannot be read changes nothing
    but this summary."""
    try:
        lines = failure_lines(path)
    except OSError as error:
        print(f'{stage} failed; its log could not be read: {error}', flush=True)
        return
    print(f'{stage} failed; ' + (f'from {path.name}:' if lines else f'{path.name} names no failed test'), flush=True)
    for line in lines[:FAILURES_SHOWN]:
        print('  ' + line, flush=True)
    if len(lines) > FAILURES_SHOWN:
        print(f'  and {len(lines) - FAILURES_SHOWN} more lines in {path.name}', flush=True)


def run(selected, output):
    output.mkdir(parents=True, exist_ok=True)
    report = {'schema': 1, 'platform': platform.platform(), 'stages': [], 'status': 'failed'}
    failed = False
    try:
        report['codex_patches'] = apply_codex_patches()
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
                                 cargo('test', 'root', '-p', 'codespace-server', '--features', 'devguard'),
                                 cargo('test', 'root', '-p', 'codespace-runner', '--features', 'devguard', '--lib'),
                                 cargo('test', 'root', '-p', 'codespace-domain', '--features', 'devguard', '--lib')]
                stage_env = env.copy()
                if stage == 'linux-isolation':
                    stage_env['CODESPACE_REQUIRE_LINUX_SANDBOX'] = '1'
                if stage in DEVGUARD_STAGES:
                    stage_env.update(devguard_env(target_dir))
                if stage == 'macos-fork-race':
                    # The harness's summary, trial lines and crash reports go to the report.
                    stage_env['CODESPACE_FORK_RACE_OUT'] = str(output / 'fork-race')
                with (output / (stage + '.log')).open('w') as log:
                    execute(commands, stage_env, log)
                entry['status'] = 'passed'
            except (OSError, RuntimeError) as error:
                entry['error'] = str(error)
                failed = True
                print_failures(stage, output / (stage + '.log'))
            entry['seconds'] = round(time.monotonic() - started, 2)
            fork_race = output / 'fork-race' / 'summary.json'
            if stage == 'macos-fork-race' and fork_race.is_file():
                # The stage's counts for readers of the job log; the report keeps the details.
                print('fork_race summary: ' + fork_race.read_text().strip(), flush=True)
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
    return [s for s in stages() if s not in MACOS_ONLY]


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

#!/usr/bin/env python3
"""CSRG-U6 and P1R-C06 qualification of CodeSpace with DevGuard.

docs/cs-rg-qualification.md and docs/recovery-qualification.md are the protocols.

  candidate             record the exact combination under test
  matrix                the claimed modes against the installed DevGuard service, each launch
                        checked against DevGuard's own record of its attempt
  faults                the cases that would leave the installed service with Suspect or charged
                        work, against DevGuard's fixture authority at CodeSpace's pin
  control               control-path latency under DG-1's development load, against the
                        installed service
  authority             control-path latency while the fixture authority stalls, pauses,
                        restarts and stops
  recovery-lifecycle    the independent Runner across every way a gateway ends (P1R-C01)
  recovery-store        process records and the operations database's one writer (P1R-C02)
  recovery-fence        Claims, controller epochs and other processes at the socket (P1R-C03)
  recovery-reconcile    reconciliation after a restart: approvals, in-flight calls, an
                        authority outage, a lost record (P1R-C04)
  recovery-observation  output gaps, retention and a lost Runner (P1R-C05)
  recovery              every recovery suite, then gateway restarts measured over idle and
                        loaded intervals against the installed service
  verify                check a sealed run and recompute its verdicts from its raw evidence

Every suite but verify writes a new evidence directory and seals it; verify writes nothing.
Nothing here edits DevGuard's configuration, restarts its service or reads its credential: the
gateway, or the independent Runner the harness starts, is given that file's path and reads it
itself, and the fault cases use DevGuard's fixture authority and throwaway credentials in their
own scratch directories. Exit status: 0 passed or qualified, 1 failed, 2 incomplete or
inconclusive, 3 the harness failed, 130 interrupted; verify exits 0 when the sealed run checks out
and 1 when it does not.
"""
import argparse
import hashlib
import importlib.util
import itertools
import json
import os
from pathlib import Path
import platform
import re
import resource
import secrets
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import traceback
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qualify_analysis as analysis  # noqa: E402
from qualify_analysis import check  # noqa: E402
import qualify_host as host  # noqa: E402
from qualify_host import ToolError  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
PINNED_RUST = '1.95.0'
EXIT = {'passed': 0, 'qualified': 0, 'failed': 1, 'incomplete': 2, 'inconclusive': 2}
EXIT_CRASH = 3
IO = (False, True)
MODES = ('in-process', 'uds')
# What a matrix execution asks for; it runs one process at a time, never a descendant.
MATRIX_REQUEST = {'cpu_milli': 50, 'memory_bytes': 64 << 20, 'tasks': 8}
KERNEL_MINIMUM = {'cpu': 'accounted', 'memory': 'accounted', 'pids': 'kernel'}
MATRIX_TIMEOUT_S = 15
APPLIED = {'cpu': 'applied', 'memory': 'applied', 'pids': 'applied'}
# The claimed modes (docs/cs-rg-qualification.md, "Supported modes").
CLAIMS = [
    {'platform': 'macos', 'runner': 'in-process', 'io': ['pipe', 'pty'], 'participation': 'required', 'claim': 'qualify'},
    {'platform': 'macos', 'runner': 'uds --runtime-bin', 'io': ['pipe', 'pty'], 'participation': 'required', 'claim': 'qualify'},
    {'platform': 'macos', 'runner': 'uds --runner-socket', 'io': ['pipe', 'pty'], 'participation': 'required', 'claim': 'unsupported'},
    {'platform': 'macos', 'runner': 'any', 'io': ['pipe', 'pty'], 'participation': 'required under --devguard status', 'claim': 'unsupported'},
    {'platform': 'macos', 'runner': 'supported', 'io': ['pipe', 'pty'], 'participation': 'required without a launch helper', 'claim': 'unsupported'},
    {'platform': 'macos', 'runner': 'supported', 'io': ['pipe', 'pty'], 'participation': 'off and required in one gateway', 'claim': 'qualify'},
    {'platform': 'macos, linux', 'runner': 'all', 'io': ['pipe', 'pty'], 'participation': 'off', 'claim': 'existing behavior'},
    {'platform': 'linux', 'runner': 'all', 'io': ['pipe', 'pty'], 'participation': 'required', 'claim': 'unsupported until DG-LINUX'},
    # The independent Runner (P1-RECOVERY, docs/recovery-qualification.md).
    {'platform': 'macos', 'runner': 'uds --runner-lifecycle independent', 'io': ['pipe', 'pty'], 'participation': 'off and required', 'claim': 'qualify'},
    {'platform': 'linux', 'runner': 'uds --runner-lifecycle independent', 'io': ['pipe', 'pty'], 'participation': 'off', 'claim': 'CI functional tests only'},
    {'platform': 'linux', 'runner': 'uds --runner-lifecycle independent', 'io': ['pipe', 'pty'], 'participation': 'required', 'claim': 'unsupported until DG-LINUX'},
    {'platform': 'macos, linux', 'runner': 'uds --runner-lifecycle independent', 'io': ['pipe', 'pty'], 'participation': "restoring processes after the Runner's own restart", 'claim': 'unsupported'},
]


# ---------------------------------------------------------------- what a run starts

class Live:
    """What a run started, settled however it ends: every process a gateway started is ended and
    waited for before the gateway's input closes, then independent Runners stop, which ends what
    they still hold, workers and fixture authorities stop, and DG-1's load owners are asked to
    settle their scopes."""

    def __init__(self):
        self.gateways, self.fixtures, self.workers, self.runners = [], [], [], []
        self.measure = None
        self.reports = []

    def settle(self):
        for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            signal.signal(number, signal.SIG_IGN)
        if self.measure is not None:
            self.measure.settle_live()
        for gateway in reversed(self.gateways):
            if gateway.process.poll() is None:
                left = gateway.settle(limit=20)
                self.reports.append({'gateway': gateway.pid, 'left_running': left, **gateway.close()})
        for runner in self.runners:
            if runner.alive():
                self.reports.append({'runner': runner.pid, **runner.stop(limit=20)})
        for worker in self.workers:
            if worker.poll() is None:
                worker.terminate()
                try:
                    worker.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    worker.kill()
                    worker.wait()
        for fixture in self.fixtures:
            fixture.stop()
        return self.reports


def scratch():
    """A short private directory for sockets, workspaces and runner state: Unix socket paths are
    limited to about 100 bytes."""
    return Path(tempfile.mkdtemp(prefix='csq-', dir='/private/tmp' if platform.system() == 'Darwin' else None))


def git(*args, root=ROOT):
    return subprocess.run(['git', '-C', str(root), *args], capture_output=True, text=True, check=True).stdout.strip()


def gone(pid, limit=10.0):
    """Whether a process is gone within `limit` seconds."""
    deadline = time.monotonic() + limit
    while True:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        except PermissionError:
            pass
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)


# ---------------------------------------------------------------- candidate

def documentation(path):
    return path.startswith(('docs/', 'docs-site/')) or path.endswith('.md')


def code_tree(root=ROOT, commit='HEAD'):
    """A hash of a commit's tree without documentation, so that a documentation-only commit keeps
    the candidate; the Codex gitlink is part of it."""
    listing = git('ls-tree', '-r', '--full-tree', commit, root=root)
    kept = [line for line in listing.splitlines() if not documentation(line.split('\t', 1)[1])]
    return hashlib.sha256(('\n'.join(kept) + '\n').encode()).hexdigest()


def devguard_lock(root=ROOT):
    """The `devguard-*` packages of Cargo.lock: the DevGuard client pin."""
    found = {}
    for block in (root / 'Cargo.lock').read_text().split('[[package]]'):
        name = re.search(r'^name = "([^"]+)"', block, re.M)
        if name and name[1].startswith('devguard'):
            version = re.search(r'^version = "([^"]+)"', block, re.M)
            source = re.search(r'^source = "([^"]+)"', block, re.M)
            found[name[1]] = {'version': version[1] if version else None, 'source': source[1] if source else None}
    return found


SETTING_KEYS = ('role', 'generation', 'max_instances', 'cpu_milli', 'memory_bytes', 'tasks')


def consumer_settings(text, consumer=host.CONSUMER):
    """A consumer's role, generation, instance limit and control reservation from host.toml. Only
    these keys are read; a credential digest is never returned."""
    found, section = {}, None
    prefix = f'consumers.{consumer}'
    for raw in text.splitlines():
        line = raw.split('#', 1)[0].strip()
        header = re.fullmatch(r'\[\s*([^\]]+?)\s*\]', line)
        if header:
            section = header[1].replace('"', '').replace("'", '').replace(' ', '')
            continue
        if section not in (prefix, prefix + '.control_reservation') or '=' not in line:
            continue
        key, value = (part.strip() for part in line.split('=', 1))
        if section == prefix and key == 'control_reservation' and value.startswith('{'):
            for inner in re.finditer(r'(\w+)\s*=\s*(\d+)', value):
                if inner[1] in SETTING_KEYS:
                    found.setdefault('control_reservation', {})[inner[1]] = int(inner[2])
        elif key in SETTING_KEYS:
            target = found.setdefault('control_reservation', {}) if section != prefix else found
            target[key] = int(value) if value.isdigit() else value.strip('"\'')
    return found


def binary(path, version=False):
    path = Path(path).resolve()
    entry = {'path': str(path), 'sha256': host.sha256(path), 'bytes': path.stat().st_size}
    if version:
        entry['version'] = host.run_text(path, '--version')
    return entry


def toolchain():
    found = {'rustc': host.run_text('rustc', '-vV'), 'cargo': host.run_text('cargo', '-V'),
             'python': sys.version.split()[0]}
    if platform.system() == 'Darwin':
        found['xcode'] = host.run_text('xcodebuild', '-version')
        found['clt'] = host.run_text('xcode-select', '-p')
    return found


def codex_check():
    result = subprocess.run([sys.executable, str(ROOT / 'scripts/codex_patch.py'), 'check'], capture_output=True,
                            text=True, cwd=ROOT)
    return {'returncode': result.returncode, 'output': (result.stdout + result.stderr).strip()[-2000:]}


def policy(release, args):
    """The service's policy as DevGuard's promotion compares it, and the codespace consumer."""
    config = Path(args.host_config)
    found = {'host_toml_sha256': host.sha256(config) if config.exists() else None, 'doctor': release.doctor(),
             'consumer': consumer_settings(config.read_text()) if config.exists() else None}
    qualification = host.SUPPORT / 'qualifications' / f'{release.id}.json'
    if qualification.exists():
        record = json.loads(qualification.read_text())
        measured = record.get('policy') or {}
        doctor = measured.get('doctor') or {}
        found['dg1_qualification'] = {
            'path': str(qualification), 'sha256': host.sha256(qualification),
            'combinations': {name: entry.get('verdict') for name, entry in (record.get('combinations') or {}).items()},
            'same_policy': (measured.get('host_toml_sha256') == found['host_toml_sha256']
                            and doctor.get('configuration_fingerprint') == found['doctor']['configuration_fingerprint']
                            and doctor.get('capabilities') == found['doctor']['capabilities'])}
    else:
        found['dg1_qualification'] = None
    return found


def dirty(root=ROOT):
    """Whether tracked files differ from the commit; untracked files do not change a build. The
    Codex submodule's files hold the recorded patch on the qualification host, so its contents
    are left to the Codex check; a submodule at another commit still counts."""
    return bool(git('status', '--porcelain', '--untracked-files=no', '--ignore-submodules=dirty', root=root))


def candidate_problems(record):
    """What keeps a recorded candidate from being the frozen one, from its record alone."""
    problems = []
    if record['codespace']['dirty']:
        problems.append('the CodeSpace checkout has uncommitted changes')
    if record['codespace']['codex_patch']['returncode']:
        problems.append('the Codex checkout is not the recorded patched tree')
    if not record['release']['state']['runs_release']:
        problems.append('the service does not run the release')
    if not all(entry['matches'] for entry in record['release']['artifacts'].values()):
        problems.append('an installed binary differs from the release manifest')
    if not record['policy']['doctor'].get('satisfied'):
        problems.append('devguard doctor is not satisfied')
    if (record['policy']['consumer'] or {}).get('role') != 'control_service':
        problems.append('host.toml has no codespace control-service consumer')
    if not (record['toolchain']['rustc'] or '').startswith(f'rustc {PINNED_RUST} '):
        problems.append(f'Rust {PINNED_RUST} is not first in PATH')
    return problems


def candidate(args, live, evidence):
    release = host.Release(args.release)
    record = {
        'schema': 'codespace-qualification-candidate/v1', 'recorded_unix': time.time(),
        'codespace': {'commit': git('rev-parse', 'HEAD'), 'dirty': dirty(),
                      'code_tree_sha256': code_tree(), 'devguard_lock': devguard_lock(),
                      'codex_gitlink': git('rev-parse', 'HEAD:third_party/codex'), 'codex_patch': codex_check()},
        'binaries': {'codespace-mcp': binary(args.mcp_bin, version=True),
                     'codespace-codex-runtime': binary(args.runtime_bin)},
        'toolchain': toolchain(),
        'release': {'id': release.id, 'directory': str(release.directory), **release.artifacts(),
                    'state': release.state()},
        'policy': policy(release, args),
        'work_capacity': host.work_capacity(args.service_log), 'pressure': host.pressure_state(args.service_log),
        'host': host.host_facts(), 'claims': CLAIMS}
    for name in ('fixture_bin', 'fixture_helper', 'qualify_bin'):
        if getattr(args, name, None):
            record['binaries'][name.replace('_', '-')] = binary(getattr(args, name))
    evidence.write_json('candidate.json', record)
    problems = candidate_problems(record)
    return {'verdict': 'failed' if problems else 'passed', 'problems': problems}


# ---------------------------------------------------------------- a runner mode's gateways

class Bench:
    """One runner mode's gateways against one authority, sharing a workspace registry and an
    evidence prefix. `authority` names the socket, the credential file (its path only), the
    launch helper, the consumer and the journal to check against; a fault case's bench also has
    its fixture authority."""

    def __init__(self, args, live, evidence, writer, mode, work, workspaces, authority, prefix=None, fixture=None):
        self.args, self.live, self.evidence, self.writer, self.mode = args, live, evidence, writer, mode
        self.work, self.authority, self.fixture = work, authority, fixture
        self.prefix = prefix or mode
        self.database = authority['journal']
        self.roots = {name: Path(value[0]) for name, value in workspaces.items()}
        self.requests = {name: (value[1] or {}).get('request') for name, value in workspaces.items()}
        self.config = evidence.file(f'{self.prefix}/registry.json')
        self.config.write_text(json.dumps(host.registry(workspaces), indent=2) + '\n')
        (work / 'runner').mkdir(mode=0o700, exist_ok=True)
        self.count = 0
        self.owner_pid = None
        self.gateways = []

    def gateway(self, name, mode='register', helper='release', socket=None, timeout_s=None, runner_socket=None,
                credential_file=None):
        self.count += 1
        authority = self.authority
        flags = host.runner_flags(self.mode, self.args.runtime_bin, self.work / 'runner', runner_socket)
        helper = authority['helper'] if helper == 'release' else helper
        flags += host.devguard_flags(mode, socket or authority['socket'], credential_file or authority['credential_file'],
                                     helper, authority['consumer'], authority['generation'])
        env = {'CODESPACE_CONFIG': str(self.config),
               'CODESPACE_OPERATIONS_DB': str(self.work / f'operations-{self.count}.sqlite')}
        if timeout_s:
            env['CODESPACE_PROCESS_TIMEOUT_SECS'] = str(timeout_s)
        prefix = f'{self.prefix}/{self.count:02d}-{name}'
        transcript = self.evidence.file(prefix + '.mcp.jsonl')
        gateway = host.Gateway(self.args.mcp_bin, flags, host.minimal_env(self.work, env),
                               self.evidence.file(prefix + '.gateway.log'),
                               record=lambda row: self.writer.write(transcript, row))
        self.live.gateways.append(gateway)
        self.gateways.append(gateway)
        self.evidence.append(f'{self.prefix}/gateways.jsonl', {'name': name, 'pid': gateway.pid, 'argv': gateway.argv,
                                                              'timeout_s': timeout_s, 'started_unix': time.time()})
        gateway.initialize()
        return gateway

    def registration(self, gateway, workspace):
        """workspace_info's resource authority, remembering the registered owner's PID."""
        info, latency = gateway.call('workspace_info', {'workspace_id': workspace})
        authority = info.get('resource_authority') or {}
        pid = (authority.get('registration') or {}).get('pid')
        if pid:
            self.owner_pid = pid
        return authority, latency

    def close(self, gateway):
        """Settle and close a gateway, then wait for its execution owner to be gone, so that the
        next owner's registration finds the consumer's one instance free."""
        left = gateway.settle()
        closed = gateway.close()
        owner_gone = gone(self.owner_pid) if self.owner_pid and self.owner_pid != gateway.pid else True
        self.evidence.append(f'{self.prefix}/gateways.jsonl', {'closed': gateway.pid, 'left_running': left,
                                                              'owner_gone': owner_gone, **closed})
        return left, closed, owner_gone

    def teardown(self):
        """Close every gateway still running, stop the fixture authority and remove the work
        directory, whatever state a case left them in."""
        for gateway in self.gateways:
            if gateway.process.poll() is None:
                try:
                    self.close(gateway)
                except Exception as error:  # noqa: BLE001 - teardown goes on regardless
                    self.evidence.append(f'{self.prefix}/gateways.jsonl', {'teardown_error': str(error)})
                    gateway.close(limit=10)
        if self.fixture is not None:
            self.fixture.stop()
        shutil.rmtree(self.work, ignore_errors=True)


def run_scenario(results, evidence, mode, io, name, function, *args, suite=None, participation=None, not_run=None):
    """Run one scenario and keep its checks; a scenario that breaks is a failed check. One that
    cannot run here is kept as not run, with `not_run` saying why."""
    began = time.time()
    checks, detail = [], {}
    if not not_run:
        try:
            checks, detail = function(*args)
        except Exception as error:  # noqa: BLE001 - every broken scenario is evidence
            checks = [check('the scenario completed', False, error=f'{type(error).__name__}: {error}',
                            trace=traceback.format_exc()[-3000:])]
            detail = {}
    entry = {'scenario': name, 'mode': mode, 'io': io, 'verdict': analysis.scenario_verdict(checks, not_run),
             'checks': checks, 'detail': detail, 'started_unix': began, 'ended_unix': time.time()}
    for key, value in (('suite', suite), ('participation', participation), ('not_run', not_run)):
        if value:
            entry[key] = value
    results.append(entry)
    evidence.append('scenarios.jsonl', entry)
    print(f"qualify: {scenario_label(entry)}: {entry['verdict']}", flush=True)
    return entry


def scenario_label(entry):
    return ' '.join(str(part) for part in (entry.get('suite'), entry['mode'], entry.get('participation'),
                                           entry['io'] or '-', entry['scenario']) if part)


def io_name(tty):
    return 'pty' if tty else 'pipe'


def quantities(request):
    return {key: request[key] for key in ('cpu_milli', 'memory_bytes', 'tasks')}


def prefixed(label, checks):
    return [dict(item, check=f"{label}: {item['check']}") for item in checks]


def account_checks(bench, gateway, process_id, request, limit=60.0):
    """DevGuard's own record of one launched execution: the attempt the gateway logged reserved
    exactly the workspace's request, applied every resource to the scope rooted at the launched
    process, and was released once the scope ended, leaving nothing charged."""
    entry = gateway.execution(process_id)
    checks = [check('the gateway logged a governed launch', bool(entry and entry.get('launched')), entry=entry)]
    if not (entry and entry.get('launched')):
        return checks, None
    found, _ = host.settle_attempts([entry['attempt_id']], bench.database, limit, bench.authority['consumer'])
    view = found.get(entry['attempt_id'])
    checks.append(check('DevGuard recorded the attempt', view is not None, attempt_id=entry['attempt_id']))
    if view is None:
        return checks, None
    wanted = quantities(request)
    checks += [
        check('it reserved and applied exactly the workspace request',
              view['reserved'] == wanted and view['applied_quantities'] == wanted,
              reserved=view['reserved'], applied=view['applied_quantities'], requested=wanted),
        check('every resource was applied', view['applied'] == APPLIED, applied=view['applied']),
        check('the scope root is the launched process',
              view['scope_root_pid'] is not None and view['scope_root_pid'] == entry.get('scope_root_pid'),
              journal=view['scope_root_pid'], logged=entry.get('scope_root_pid')),
        check('released once its scope ended, holding nothing',
              view['phase'] == 'released' and view['release_reason'] == 'scope_terminated' and not view['charged'],
              phase=view['phase'], release_reason=view['release_reason'], charged=view['charged']),
        check('its tracking stayed complete', view['tracking_lost'] is False, tracking_lost=view['tracking_lost'])]
    return checks, dict(view, attempt_id=entry['attempt_id'])


def refusal_checks(bench, result, code, phase, denial=None):
    """A refused exec_command: CodeSpace's code, and the attempt it names as DevGuard recorded it,
    holding nothing. `phase` None means no attempt may exist; 'absent' that DevGuard has none."""
    checks = [check(f'refused with {code}', result['outcome'] == 'refused' and result.get('code') == code,
                    outcome=result['outcome'], code=result.get('code'), message=result.get('message'),
                    latency_ms=result.get('latency_ms'))]
    attempt = result.get('attempt_id')
    if phase is None:
        checks.append(check('refused before any attempt existed', attempt is None, attempt_id=attempt))
        return checks
    if phase == 'absent':
        found = host.journal_attempts([attempt], bench.database, bench.authority['consumer']) if attempt else {}
        checks.append(check('DevGuard holds no attempt for it', not found, attempt_id=attempt))
        return checks
    checks.append(check('the refusal names its attempt', attempt is not None, message=result.get('message')))
    if attempt:
        found, _ = host.settle_attempts([attempt], bench.database, 30, bench.authority['consumer'])
        view = found.get(attempt)
        checks.append(check(f'DevGuard recorded it {phase}, holding nothing',
                            view is not None and view['phase'] == phase and not view['charged']
                            and (denial is None or view['denial'] == denial),
                            view=view, attempt_id=attempt))
    return checks


def started_checks(label, started):
    return check(f'{label}: started and confirmed', started['outcome'] == 'started' and started.get('dispatch') == 'confirmed',
                 started=started)


# ---------------------------------------------------------------- matrix scenarios (installed service)
# Every command here runs one process, or children its root waits for: none survives its root,
# leaves its group or is reaped unobserved, so no attempt can stay Suspect on the owner's service.

def scenario_registration(bench, gateway):
    authority, latency = bench.registration(gateway, 'gov')
    registration = authority.get('registration') or {}
    pid = registration.get('pid')
    owner = 'in_process' if bench.mode == 'in-process' else 'worker'
    owner_ok = pid == gateway.pid if bench.mode == 'in-process' else pid in host.children(gateway.pid)
    instances = host.journal_instances(bench.authority['consumer'], bench.database)
    active = [entry for entry in instances if entry['state'] == 'active']
    checks = [
        check('participation is registration', authority.get('participation') == 'registration', authority=authority),
        check('the authority is available', authority.get('state') == 'available'),
        check(f'the {owner} owner is registered', registration.get('owner') == owner and registration.get('state') == 'registered',
              registration=registration),
        check('it governs executions', authority.get('governs_execution') is True),
        check('the registered process is the owner', owner_ok, pid=pid, gateway=gateway.pid),
        check("DevGuard's one active instance is that process", len(active) == 1 and active[0]['pid'] == pid,
              instances=instances)]
    return checks, {'workspace_info_ms': latency, 'resource_authority': authority, 'instances': instances}


def scenario_exits(bench, gateway, tty):
    checks, detail = [], {}
    # A governed PTY child inherits its launch descriptors, so the pinned Codex PTY spawns it on
    # its descriptor-keeping path, which reports a signal death as exit code 128 + the signal
    # (docs/agent-integration.md).
    death = {'termination': 'exited', 'exit_code': 128 + 15} if tty else {'termination': 'signaled', 'signal': 15}
    for label, command, expected in (('exit 0', ['true'], {'termination': 'exited', 'exit_code': 0}),
                                     ('exit 3', ['sh', '-c', 'exit 3'], {'termination': 'exited', 'exit_code': 3}),
                                     ('death by SIGTERM', ['sh', '-c', 'kill -TERM $$'], death)):
        started = gateway.exec('gov', command, tty=tty)
        checks.append(started_checks(label, started))
        if started['outcome'] != 'started':
            continue
        status = gateway.wait_exited(started['process_id'])
        observed = {'termination': (status or {}).get('termination'), 'exit_code': (status or {}).get('exit_code'),
                    'signal': ((status or {}).get('signal') or {}).get('number')}
        checks.append(check(f'{label}: reported as expected', all(observed[key] == value for key, value in expected.items()),
                            observed=observed, expected=expected))
        account, view = account_checks(bench, gateway, started['process_id'], MATRIX_REQUEST)
        checks += prefixed(label, account)
        detail[label] = {'status': status, 'attempt': view}
    return checks, detail


def scenario_io(bench, gateway, tty):
    if tty:
        started = gateway.exec('gov', ['sh', '-c', 'stty size; read line; stty size; echo "got-$line"'], tty=True)
        checks = [started_checks('a reading shell', started)]
        if started['outcome'] != 'started':
            return checks, {'started': started}
        pid = started['process_id']
        first, cursor, _, found = gateway.read_until(pid, '24 80')
        resized, _ = gateway.call('process_resize', {'process_id': pid, 'rows': 40, 'cols': 100})
        gateway.call('write_stdin', {'process_id': pid, 'data': 'x\n'})
        second, cursor, _, echoed = gateway.read_until(pid, 'got-x', cursor=cursor)
        status = gateway.wait_exited(pid)
        checks += [check('the terminal starts at 24x80', found, output=first),
                   check('process_resize answers 40x100', resized.get('rows') == 40 and resized.get('cols') == 100,
                         answer=resized),
                   check('stdin reaches the process', echoed, output=second),
                   check('the process sees the new size', '40 100' in second, output=second),
                   check('it exits 0', (status or {}).get('termination') == 'exited' and status.get('exit_code') == 0,
                         status=status)]
    else:
        started = gateway.exec('gov', ['cat'])
        checks = [started_checks('cat', started)]
        if started['outcome'] != 'started':
            return checks, {'started': started}
        pid = started['process_id']
        gateway.call('write_stdin', {'process_id': pid, 'data': 'ping-1\n'})
        text, _, _, echoed = gateway.read_until(pid, 'ping-1')
        try:
            gateway.call('process_resize', {'process_id': pid, 'rows': 40, 'cols': 100})
            resize = None
        except ToolError as error:
            resize = error.code
        gateway.terminate(pid)
        status = gateway.wait_exited(pid)
        checks += [check('stdin reaches the process and its output returns', echoed, output=text),
                   check('process_resize refuses a pipe', resize == 'PROCESS_NOT_TTY', code=resize),
                   check('terminated', (status or {}).get('termination') == 'terminated', status=status)]
    account, view = account_checks(bench, gateway, pid, MATRIX_REQUEST)
    return checks + account, {'attempt': view}


def scenario_termination(bench, gateway, tty):
    started = gateway.exec('gov', ['sleep', '600'], tty=tty)
    checks = [started_checks('sleep', started)]
    if started['outcome'] != 'started':
        return checks, {'started': started}
    pid = started['process_id']
    root = (gateway.execution(pid) or {}).get('scope_root_pid')
    asked = time.monotonic()
    ack = gateway.terminate(pid)
    status = gateway.wait_exited(pid)
    exited_ms = round((time.monotonic() - asked) * 1000, 3)
    scope = host.wait_group_gone(root) if root else None
    checks += [check('terminate_process answered', ack is not None, ack_ms=ack),
               check('reported terminated', (status or {}).get('termination') == 'terminated', status=status),
               check("the scope's process group ended", scope is not None, root=root,
                     scope_exit_ms=None if scope is None else round(scope * 1000 + exited_ms, 3))]
    account, view = account_checks(bench, gateway, pid, MATRIX_REQUEST)
    return checks + account, {'ack_ms': ack, 'exited_ms': exited_ms, 'attempt': view}


def scenario_timeout(bench, gateway, tty):
    started = gateway.exec('gov', ['sleep', '600'], tty=tty)
    checks = [started_checks('sleep', started)]
    if started['outcome'] != 'started':
        return checks, {'started': started}
    status = gateway.wait_exited(started['process_id'], limit=MATRIX_TIMEOUT_S + 45)
    checks.append(check(f'timed out after {MATRIX_TIMEOUT_S} s', (status or {}).get('termination') == 'timeout',
                        status=status))
    account, view = account_checks(bench, gateway, started['process_id'], MATRIX_REQUEST)
    return checks + account, {'attempt': view}


def scenario_exec_failed(bench, gateway, tty):
    """READY, then the program cannot be executed: an interpreter that does not exist (ENOENT,
    helper exit 127) or one that is not executable (EACCES, helper exit 126)."""
    root = bench.roots['gov']
    plain = root / 'plain-file'
    plain.write_text('not a program\n')
    os.chmod(plain, 0o644)
    scripts = {'ENOENT': (root / 'missing-interpreter', '#!/nonexistent/interpreter\n', 2),
               'EACCES': (root / 'unusable-interpreter', f'#!{plain}\n', 13)}
    checks, detail = [], {}
    for label, (path, text, errno) in scripts.items():
        path.write_text(text)
        os.chmod(path, 0o755)
        result = gateway.exec('gov', [str(path)], tty=tty)
        checks += prefixed(label, refusal_checks(bench, result, 'PROCESS_SPAWN_FAILED', 'released'))
        checks.append(check(f'{label}: the refusal gives errno {errno}', f'(errno {errno})' in (result.get('message') or ''),
                            message=result.get('message')))
        attempt = result.get('attempt_id')
        view = host.journal_attempts([attempt], bench.database, bench.authority['consumer']).get(attempt) if attempt else None
        checks.append(check(f'{label}: the helper claimed the grant and its scope settled it',
                            bool(view and view['launched'] and view['release_reason'] == 'scope_terminated'), view=view))
        detail[label] = {'result': result, 'attempt': view}
    return checks, detail


def scenario_shortage(bench, gateway, tty):
    """A request larger than the work capacity is denied every time, and each refusal frees the
    workspace: the next call is refused for capacity again, never WORKSPACE_BUSY."""
    results = [gateway.exec('hog', ['true'], tty=tty) for _ in range(3)]
    checks = []
    for index, result in enumerate(results, 1):
        checks += prefixed(f'call {index}', refusal_checks(bench, result, 'RESOURCE_UNAVAILABLE', 'denied',
                                                           'resource_unavailable'))
    attempts = [result.get('attempt_id') for result in results]
    checks.append(check('each call is a new attempt', len(set(attempts)) == len(attempts) and None not in attempts,
                        attempts=attempts))
    return checks, {'latency_ms': [result.get('latency_ms') for result in results]}


def scenario_unsupported(bench, gateway, tty):
    result = gateway.exec('kernel', ['true'], tty=tty)
    return refusal_checks(bench, result, 'RESOURCE_POLICY_UNSUPPORTED', 'denied', 'resource_policy_unsupported'), {
        'result': result}


def child_running(owner, needle, limit=10.0):
    """The PID of the owner's child whose command line is `needle`."""
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        for pid in host.children(owner):
            if host.run_text('ps', '-o', 'command=', '-p', str(pid)) == needle:
                return pid
        time.sleep(0.1)
    return None


def scenario_mixed(bench, gateway, tty):
    """A governed and an ungoverned execution of one gateway at once: each runs, and neither holds
    a descriptor beyond its standard three, so the launch's permit carrier and transcript reached
    the helper only."""
    off = gateway.exec('off', ['sleep', '31'], tty=tty)
    gov = gateway.exec('gov', ['sleep', '32'], tty=tty)
    checks = [check('the ungoverned execution started', off['outcome'] == 'started', started=off),
              started_checks('the governed execution', gov)]
    detail = {}
    if off['outcome'] == 'started' and gov['outcome'] == 'started':
        owner = bench.owner_pid or gateway.pid
        governed = (gateway.execution(gov['process_id']) or {}).get('scope_root_pid')
        ungoverned = child_running(owner, 'sleep 31')
        detail = {'governed_pid': governed, 'ungoverned_pid': ungoverned,
                  'governed_descriptors': host.descriptors(governed) if governed else None,
                  'ungoverned_descriptors': host.descriptors(ungoverned) if ungoverned else None}
        checks += [check('the ungoverned child was found', ungoverned is not None, owner=owner),
                   check('the governed child holds only its standard descriptors',
                         detail['governed_descriptors'] == [0, 1, 2], descriptors=detail['governed_descriptors']),
                   check('the ungoverned child holds only its standard descriptors',
                         detail['ungoverned_descriptors'] == [0, 1, 2], descriptors=detail['ungoverned_descriptors'])]
    for started in (off, gov):
        if started['outcome'] == 'started':
            gateway.terminate(started['process_id'])
            gateway.wait_exited(started['process_id'])
    if gov['outcome'] == 'started':
        account, view = account_checks(bench, gateway, gov['process_id'], MATRIX_REQUEST)
        checks += account
        detail['attempt'] = view
    return checks, detail


def scenario_no_helper(bench, helper, code_label):
    """Rollback of new admissions: a gateway without a usable launch helper admits, then cancels,
    and still runs ungoverned workspaces."""
    gateway = bench.gateway(code_label, helper=helper)
    try:
        result = gateway.exec('gov', ['true'])
        checks = refusal_checks(bench, result, 'MANAGED_LAUNCH_UNAVAILABLE', 'cancelled')
        checks += off_still_runs(gateway)
    finally:
        bench.close(gateway)
    return checks, {'result': result}


def off_still_runs(gateway):
    started = gateway.exec('off', ['true'])
    status = gateway.wait_exited(started['process_id']) if started['outcome'] == 'started' else None
    return [check('an off workspace still runs', (status or {}).get('exit_code') == 0, started=started, status=status)]


def scenario_status_mode(bench):
    gateway = bench.gateway('status-mode', mode='status', helper=None)
    try:
        result = gateway.exec('gov', ['true'])
        checks = refusal_checks(bench, result, 'RESOURCE_POLICY_UNSUPPORTED', None) + off_still_runs(gateway)
    finally:
        bench.close(gateway)
    return checks, {'result': result}


def scenario_authority_unavailable(bench):
    """No authority at the socket: refused promptly, nothing reaches DevGuard."""
    gateway = bench.gateway('no-authority', socket=bench.work / 'missing' / 'authority.sock')
    try:
        result = gateway.exec('gov', ['true'])
        checks = refusal_checks(bench, result, 'RESOURCE_AUTHORITY_UNAVAILABLE', 'absent') + off_still_runs(gateway)
    finally:
        bench.close(gateway)
    return checks, {'result': result}


def scenario_runner_socket(bench):
    """`uds` with a worker the gateway did not start: governed executions are unsupported."""
    socket = bench.work / 'standalone.sock'
    with open(bench.evidence.file(f'{bench.mode}/standalone-worker.log'), 'ab') as log:
        worker = subprocess.Popen([str(bench.args.runtime_bin), str(socket)], stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=log, env=host.minimal_env(bench.work),
                                  start_new_session=True)
    bench.live.workers.append(worker)
    deadline = time.monotonic() + 15
    while not socket.exists() and time.monotonic() < deadline and worker.poll() is None:
        time.sleep(0.05)
    gateway = bench.gateway('runner-socket', runner_socket=socket)
    try:
        result = gateway.exec('gov', ['true'])
        checks = refusal_checks(bench, result, 'RESOURCE_POLICY_UNSUPPORTED', None) + off_still_runs(gateway)
    finally:
        bench.close(gateway)
        worker.terminate()
        worker.wait(timeout=10)
    return checks, {'result': result}


def matrix_workspaces(work, capacity):
    """`gov` and `gov2` require participation; `off` does not; `kernel` asks for kernel task
    control, which macOS cannot give; `hog` asks for more CPU than the whole work capacity."""
    hog = dict(MATRIX_REQUEST, cpu_milli=((capacity or {}).get('cpu_milli') or 1_000_000) + 1)
    governed = host.required(**MATRIX_REQUEST)
    return {'gov': (work / 'ws/gov', governed), 'gov2': (work / 'ws/gov2', governed), 'off': (work / 'ws/off', None),
            'kernel': (work / 'ws/kernel', host.required(**MATRIX_REQUEST, minimum=KERNEL_MINIMUM)),
            'hog': (work / 'ws/hog', host.required(**hog))}


def settled_checks(bench, label, limit=60.0, period=1.0):
    """After a mode: nothing of the consumer is charged, and its instances are not active. DevGuard
    retires an instance once it sees that its owner is gone, which can come after the gateway has
    exited, so both are awaited for up to `limit` seconds."""
    deadline = time.monotonic() + limit
    while True:
        charged = host.journal_charged(bench.database)
        mine = charged['attempts'].get(bench.authority['consumer'], 0)
        instances = host.journal_instances(bench.authority['consumer'], bench.database)
        active = [entry for entry in instances if entry['state'] == 'active']
        if (not mine and not active) or time.monotonic() >= deadline:
            break
        time.sleep(period)
    return [check(f'{label}: nothing of the consumer stays charged', mine == 0, charged=charged),
            check(f'{label}: no instance stays active', not active, instances=instances)]


def installed_authority(args):
    return {'socket': args.socket, 'credential_file': args.credential_file, 'helper': host.Release(args.release).helper,
            'consumer': host.CONSUMER, 'generation': host.GENERATION, 'journal': args.journal}


def matrix(args, live, evidence):
    release = host.Release(args.release)
    before = {'state': release.state(), 'charged': host.journal_charged(args.journal),
              'pressure': host.pressure_state(args.service_log), 'work_capacity': host.work_capacity(args.service_log)}
    evidence.write_json('before.json', before)
    if not before['state']['runs_release']:
        return {'verdict': 'incomplete', 'problems': ['the service does not run the release'], 'scenarios': []}
    if sum(before['charged']['attempts'].values()) or before['charged']['leases']:
        return {'verdict': 'incomplete', 'problems': ['work was charged when the matrix began'], 'scenarios': []}
    log_offset = host.log_size(args.service_log)
    writer = host.Writer()
    results = []
    authority = installed_authority(args)
    try:
        for mode in args.modes:
            work = scratch()
            bench = None
            try:
                bench = Bench(args, live, evidence, writer, mode, work,
                              matrix_workspaces(work, before['work_capacity']), authority)
                gateway = bench.gateway('main', timeout_s=MATRIX_TIMEOUT_S)
                entry = run_scenario(results, evidence, mode, None, 'registration', scenario_registration, bench, gateway)
                if entry['verdict'] == 'pass':
                    for tty in IO:
                        for name, function in (('exits', scenario_exits), ('io', scenario_io),
                                               ('termination', scenario_termination), ('timeout', scenario_timeout),
                                               ('exec-failed', scenario_exec_failed), ('shortage', scenario_shortage),
                                               ('unsupported-minimum', scenario_unsupported), ('mixed', scenario_mixed)):
                            run_scenario(results, evidence, mode, io_name(tty), name, function, bench, gateway, tty)
                bench.close(gateway)
                run_scenario(results, evidence, mode, None, 'no-helper', scenario_no_helper, bench, None, 'no-helper')
                run_scenario(results, evidence, mode, None, 'unusable-helper', scenario_no_helper, bench,
                             Path('/nonexistent/devguard-launch'), 'unusable-helper')
                run_scenario(results, evidence, mode, None, 'status-mode', scenario_status_mode, bench)
                run_scenario(results, evidence, mode, None, 'authority-unavailable', scenario_authority_unavailable, bench)
                if mode == 'uds':
                    run_scenario(results, evidence, mode, None, 'runner-socket', scenario_runner_socket, bench)
                entry = run_scenario(results, evidence, mode, None, 'settled', lambda: (settled_checks(bench, mode), {}))
                if entry['verdict'] != 'pass':
                    print('qualify: work stayed charged or an instance stayed active; the matrix stops here',
                          flush=True)
                    break
            finally:
                if bench is not None:
                    bench.teardown()
                else:
                    shutil.rmtree(work, ignore_errors=True)
    finally:
        evidence.write_json('writer.json', writer.close())
        evidence.write_json('service-events.json', host.service_events(args.service_log, log_offset or 0))
        evidence.write_json('after.json', {'state': release.state(), 'charged': host.journal_charged(args.journal),
                                           'instances': host.journal_instances(database=args.journal)})
    verdict = analysis.suite_verdict([entry['verdict'] for entry in results])
    return {'verdict': verdict, 'scenarios': [{key: entry[key] for key in ('scenario', 'mode', 'io', 'verdict')}
                                              for entry in results]}


# ---------------------------------------------------------------- faults (DevGuard's fixture authority)
# Each case gets its own fixture authority (DevGuard's daemon code at CodeSpace's pin, in a private
# state directory), because it leaves Suspect or charged work behind, kills the authority or holds
# its lock. The expectations are DevGuard's contracts ("Native policy application and scope
# evidence", "Reconciliation") as CodeSpace's gateway tests of the same cases assert them.

# What a fault case's execution asks for, as CodeSpace's gateway tests ask.
FIXTURE_REQUEST = {'cpu_milli': 50, 'memory_bytes': 32 << 20, 'tasks': 4}
# Process control after a lost authority: the reaper's two-second bound for an observation that
# cannot be answered, twice.
LOSS_LIMIT_S = 4.0
# How long the stalled-authority case holds the authority's lock.
HOLD_MS = 1500


def fault_workspaces(work):
    """`gov-N` require participation, `gov-confirm` also an approval; `off` does not."""
    governed = host.required(**FIXTURE_REQUEST)
    spaces = {'off': (work / 'ws/off', None),
              'gov-confirm': (work / 'ws/gov-confirm', governed, {'approvals': 'confirm'})}
    for n in range(3):
        spaces[f'gov-{n}'] = (work / f'ws/gov-{n}', governed)
    return spaces


def fixture_bench(args, live, evidence, writer, mode, name, workspaces=fault_workspaces):
    """A fresh fixture authority and a bench of gateways against it."""
    work = scratch()
    try:
        fixture = host.FixtureAuthority(args.fixture_bin, work / 'fx', host.minimal_env(work),
                                        evidence.file(f'{mode}/{name}/fixture.log'))
    except Exception:
        shutil.rmtree(work, ignore_errors=True)
        raise
    live.fixtures.append(fixture)
    settings = fixture.settings
    if not settings.get('native'):
        fixture.stop()
        shutil.rmtree(work, ignore_errors=True)
        raise NotNative('the fixture authority has no native host evidence on this platform')
    authority = {'socket': settings['socket'], 'credential_file': settings['credential_file'],
                 'helper': args.fixture_helper, 'consumer': settings['consumer'],
                 'generation': settings['generation'], 'journal': fixture.journal()}
    return Bench(args, live, evidence, writer, mode, work, workspaces(work), authority, prefix=f'{mode}/{name}',
                 fixture=fixture)


class NotNative(Exception):
    """The fixture authority cannot govern on this platform (Linux before DG-LINUX)."""


def run_fault(results, args, live, evidence, writer, mode, io, name, function):
    """One fault case on its own fixture authority, torn down however the case ends."""
    label = name if io is None else f'{name}-{io}'
    holder = {}

    def case():
        bench = holder['bench'] = fixture_bench(args, live, evidence, writer, mode, label)
        return function(bench) if io is None else function(bench, io == 'pty')
    try:
        return run_scenario(results, evidence, mode, io, name, case)
    finally:
        if 'bench' in holder:
            holder['bench'].teardown()


def view_of(bench, attempt):
    """DevGuard's record of one attempt now, or None."""
    if not attempt:
        return None
    return host.journal_attempts([attempt], bench.database, bench.authority['consumer']).get(attempt)


def settled_view(bench, attempt, limit=30.0):
    """DevGuard's record of one attempt once it holds nothing, or as it stands after `limit`."""
    if not attempt:
        return None
    found, _ = host.settle_attempts([attempt], bench.database, limit, bench.authority['consumer'])
    return found.get(attempt)


def released_cleanly(view):
    return bool(view and view['phase'] == 'released' and view['release_reason'] == 'scope_terminated'
                and not view['charged'] and view['tracking_lost'] is False)


def suspect(view):
    return bool(view and view['phase'] == 'suspect' and view['charged'])


def read_all(gateway, process_id, limit=10.0):
    return gateway.read_until(process_id, None, limit=limit)[0]


def until(moment):
    time.sleep(max(0.0, moment - time.monotonic()))


def attempt_of(gateway, process_id):
    return (gateway.execution(process_id) or {}).get('attempt_id')


def ran(gateway, started, output, label='it'):
    """The checks of a governed command that should have run to exit 0 with `output`."""
    if started['outcome'] != 'started':
        return [started_checks(label, started)], None, None
    status = gateway.wait_exited(started['process_id'])
    text = read_all(gateway, started['process_id'])
    return [started_checks(label, started),
            check(f'{label} printed {output!r} and exited 0', text == output and (status or {}).get('exit_code') == 0,
                  output=text, status=status)], status, text


def fault_descendant(bench, tty):
    """A root that exits leaving a descendant: the attempt stays charged, not Suspect, while the
    descendant runs, and is released once it ends. On a PTY the reconciler is paused around the
    exit, so only the owner's observation before the reap can adopt the descendant."""
    gateway = bench.gateway('main')
    script = ("trap '' HUP; " if tty else '') + 'sleep 3 </dev/null >/dev/null 2>&1 & echo started'
    if tty:
        bench.fixture.command('pause')
    try:
        started = gateway.exec('gov-0', ['/bin/sh', '-c', script], tty=tty)
        checks, status, _ = ran(gateway, started, 'started\n', 'the root')
    finally:
        if tty:
            bench.fixture.command('resume')
    attempt = attempt_of(gateway, started.get('process_id'))
    time.sleep(1.5)
    during = view_of(bench, attempt)
    after = settled_view(bench, attempt)
    checks += [check('the descendant keeps the attempt charged, not Suspect',
                     bool(during and during['charged'] and during['phase'] != 'suspect'), view=during),
               check('released once the descendant ended, tracking complete', released_cleanly(after), view=after)]
    return checks, {'status': status, 'during': during, 'after': after}


def fault_escape(bench, tty):
    """A descendant that leaves the scope's process group with setsid while the root still runs:
    DevGuard finds a child of a live member outside the group, an escape, which is sticky and
    keeps the attempt charged as Suspect after everything has ended. The descendant is this
    harness's own Python, so the host needs no setsid program."""
    gateway = bench.gateway('main')
    script = ('"$0" -c "import os, time; os.setsid(); time.sleep(3)" </dev/null >/dev/null 2>&1 & '
              'sleep 2; echo started')
    began = time.monotonic()
    started = gateway.exec('gov-0', ['/bin/sh', '-c', script, sys.executable], tty=tty)
    checks, status, _ = ran(gateway, started, 'started\n', 'the root')
    attempt = attempt_of(gateway, started.get('process_id'))
    until(began + 7.0)
    view = view_of(bench, attempt)
    checks.append(check('the escape keeps the attempt charged as Suspect after everything ended', suspect(view),
                        view=view))
    return checks, {'status': status, 'attempt': view}


def fault_stalled_observer(bench, tty):
    """The owner's observation before the reap cannot be answered: the reconciler is paused and
    the authority held from just before the root exits, leaving a descendant. CodeSpace reaps the
    root at its observer's bound while the authority is still held, so process control never
    waits on DevGuard; the unproven descendant keeps the attempt charged as Suspect, also after
    it ends (CSRG-U5)."""
    gateway = bench.gateway('main')
    go = bench.roots['gov-0'] / 'go'
    script = (f"while [ ! -e '{go}' ]; do sleep 0.05; done; "
              "trap '' HUP; sleep 6 </dev/null >/dev/null 2>&1 & echo started")
    started = gateway.exec('gov-0', ['/bin/sh', '-c', script], tty=tty)
    checks = [started_checks('the root', started)]
    if started['outcome'] != 'started':
        return checks, {'started': started}
    pid = started['process_id']
    attempt = attempt_of(gateway, pid)
    bench.fixture.command('pause')
    held = time.monotonic()
    bench.fixture.command('hold 4000')
    go.touch()
    status = gateway.wait_exited(pid, limit=10)
    reaped_s = time.monotonic() - held
    output = read_all(gateway, pid)
    until(held + 4.5)
    bench.fixture.command('resume')
    time.sleep(1.5)
    middle = view_of(bench, attempt)
    until(held + 8.0)
    end = view_of(bench, attempt)
    checks += [check('the root printed and exited 0', output == 'started\n' and (status or {}).get('exit_code') == 0,
                     output=output, status=status),
               check('reaped at the observer bound while the authority was still held',
                     status is not None and reaped_s < 3.9, reaped_s=round(reaped_s, 3)),
               check('the unproven descendant leaves the attempt Suspect', suspect(middle), view=middle),
               check('still Suspect after the descendant ended', suspect(end), view=end)]
    return checks, {'reaped_s': reaped_s, 'middle': middle, 'end': end}


def fault_authority_lost(bench):
    """The authority dies while a governed pipe and PTY process run: CodeSpace keeps answering for
    both and terminates them as any other process, within the reaper's bound for an observation
    that cannot be answered; their workspaces are free again, where a new governed execution is
    refused having started nothing; ungoverned ones still run."""
    gateway = bench.gateway('main')
    checks, running, detail = [], [], {}
    for workspace, tty in (('gov-0', False), ('gov-1', True)):
        started = gateway.exec(workspace, ['/bin/sleep', '30'], tty=tty)
        checks.append(started_checks(f'{workspace} ({io_name(tty)})', started))
        if started['outcome'] == 'started':
            running.append((workspace, started['process_id']))
    bench.fixture.kill()
    began = time.monotonic()
    for workspace, pid in running:
        payload, latency = gateway.status(pid)
        detail[workspace] = {'state': payload.get('state'), 'status_ms': latency, 'ack_ms': gateway.terminate(pid)}
    for workspace, pid in running:
        status = gateway.wait_exited(pid, limit=10)
        detail[workspace]['status'] = status
        checks.append(check(f'{workspace}: answered running, then terminated',
                            detail[workspace]['state'] == 'running' and (status or {}).get('termination') == 'terminated',
                            **detail[workspace]))
    elapsed = time.monotonic() - began
    checks.append(check(f'both ended within {LOSS_LIMIT_S} s of the loss', len(running) == 2 and elapsed < LOSS_LIMIT_S,
                        elapsed_s=round(elapsed, 3)))
    for workspace, _ in running:
        result = gateway.exec(workspace, ['/usr/bin/true'])
        detail[workspace]['afterwards'] = result
        checks += prefixed(f'{workspace} afterwards',
                           refusal_checks(bench, result, 'RESOURCE_AUTHORITY_UNAVAILABLE', 'absent'))
    return checks + off_still_runs(gateway), detail


def fault_authority_restart(bench):
    """The authority restarts while a governed process runs, one held for approval and resumed:
    CodeSpace keeps the process and its status; once it ends, DevGuard, which no longer knows its
    scope, keeps the attempt charged as Suspect; resuming the approval again returns the same
    process, never a second launch; a new execution is admitted and released (CSRG-U5)."""
    gateway = bench.gateway('main')
    go = bench.roots['gov-confirm'] / 'go'
    held = gateway.exec('gov-confirm', ['/bin/sh', '-c', f"while [ ! -e '{go}' ]; do sleep 0.05; done; echo done"])
    approval = held.get('approval_id')
    checks = [check('held for approval', held['outcome'] == 'refused' and held.get('code') == 'APPROVAL_REQUIRED'
                    and approval is not None, held=held)]
    if not approval:
        return checks, {'held': held}
    gateway.call('approval_resolve', {'approval_id': approval, 'decision': 'grant'})
    resumed, _ = gateway.resume(approval)
    pid = (resumed.get('exec_command') or {}).get('process_id')
    checks.append(check('resumed and confirmed', (resumed.get('exec_command') or {}).get('dispatch_status') == 'confirmed',
                        resumed=resumed))
    if not pid:
        return checks, {'resumed': resumed}
    attempt = attempt_of(gateway, pid)
    restarted = bench.fixture.command('restart')
    payload, latency = gateway.status(pid)
    go.touch()
    status = gateway.wait_exited(pid)
    output = read_all(gateway, pid)
    time.sleep(1.5)
    after = view_of(bench, attempt)
    again, _ = gateway.resume(approval)
    fresh = gateway.exec('gov-0', ['/bin/echo', 'after'])
    fresh_checks, _, _ = ran(gateway, fresh, 'after\n', 'a new execution')
    fresh_view = settled_view(bench, attempt_of(gateway, fresh.get('process_id')))
    still = view_of(bench, attempt)
    checks += [check('the authority restarted', restarted == {'restarted': True}, answer=restarted),
               check('the process still runs under CodeSpace', payload.get('state') == 'running', status_ms=latency),
               check('it ran to its end', output == 'done\n' and (status or {}).get('exit_code') == 0,
                     output=output, status=status),
               check('its attempt stays charged as Suspect', suspect(after), view=after),
               check('resuming again returns the same process',
                     (again.get('exec_command') or {}).get('process_id') == pid, again=again),
               check('the log reports one launch of it', gateway.launches().get(pid) == 1,
                     launches=gateway.launches().get(pid))]
    checks += fresh_checks
    checks += [check('the new execution was released', released_cleanly(fresh_view), view=fresh_view),
               check('the Suspect attempt is still charged', suspect(still), view=still)]
    return checks, {'after': after, 'fresh': fresh_view, 'still': still}


def fault_authority_stalled(bench):
    """The authority holds its lock past the owner's deadlines for the whole exec_command, so the
    session's first request that needs it gets no answer in time (registration, or the admission
    once registered). Whichever it was, the call is refused before the hold ends, the command
    never runs, DevGuard never hands the attempt to a helper and nothing of it stays charged once
    an admission it took late has expired; the workspace's next execution is a new attempt and
    runs. A lost answer to a request already past registration is not reachable from outside the
    owner; DevGuard's client and the runner test it (docs/cs-rg-qualification.md)."""
    gateway = bench.gateway('main')
    marker = bench.roots['gov-0'] / 'ran'
    bench.fixture.command(f'hold {HOLD_MS}')
    held = time.monotonic()
    result = gateway.exec('gov-0', ['/bin/sh', '-c', f"touch '{marker}'"])
    answered_s = time.monotonic() - held
    # A prepared admission DevGuard took after the hold expires on its five-second deadline.
    until(held + HOLD_MS / 1000 + 6.0)
    attempt = result.get('attempt_id')
    view = settled_view(bench, attempt)
    following = gateway.exec('gov-0', ['/bin/echo', 'next'])
    next_checks, _, _ = ran(gateway, following, 'next\n', 'the next execution')
    checks = [check('refused as unknown or unavailable', result['outcome'] == 'refused'
                    and result.get('code') in ('ADMISSION_UNKNOWN', 'RESOURCE_AUTHORITY_UNAVAILABLE'), result=result),
              check('answered before the hold ended', answered_s < HOLD_MS / 1000, answered_s=round(answered_s, 3)),
              check('the command never ran', not marker.exists()),
              check('DevGuard never handed it to a helper', view is None or view['scope_root_pid'] is None, view=view),
              check('nothing of it stays charged', not (view and view['charged']), view=view)]
    checks += next_checks
    checks.append(check('the next execution is a new attempt',
                        attempt_of(gateway, following.get('process_id')) not in (None, attempt)))
    return checks, {'result': result, 'attempt': view}


def fault_helper_ends(bench):
    """A launch helper that exits before it reports READY (a stand-in that exits 1): the command is
    refused as never attempted, and once CodeSpace has reaped the helper it reports that it holds
    none, so DevGuard releases the unclaimed grant as never started."""
    fake = bench.work / 'ending-helper'
    fake.write_text('#!/bin/sh\nexit 1\n')
    os.chmod(fake, 0o755)
    gateway = bench.gateway('ending-helper', helper=fake)
    marker = bench.roots['gov-0'] / 'ran'
    result = gateway.exec('gov-0', ['/bin/sh', '-c', f"touch '{marker}'"])
    view = settled_view(bench, result.get('attempt_id'))
    checks = refusal_checks(bench, result, 'PROCESS_SPAWN_FAILED', 'released')
    checks += [check('the helper ended before READY', 'ended before it reported ready' in (result.get('message') or ''),
                     message=result.get('message')),
               check('the command never ran', not marker.exists()),
               check('released as never started, never claimed',
                     bool(view and view['release_reason'] == 'no_helper_created' and view['scope_root_pid'] is None
                          and not view['launched']), view=view)]
    return checks, {'result': result, 'attempt': view}


def fault_credential(bench):
    """A credential the authority refuses (a fresh random secret) and one that cannot be read (a
    missing file): every governed execution is refused before anything is asked, and ungoverned
    ones still run."""
    wrong = bench.work / 'wrong.secret'
    descriptor = os.open(wrong, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'w') as handle:
        handle.write(secrets.token_hex(32))
    checks, detail = [], {}
    for label, path, state in (('refused credential', wrong, 'credential_refused'),
                               ('missing credential', bench.work / 'missing.secret', 'credential_unavailable')):
        gateway = bench.gateway(label.replace(' ', '-'), credential_file=path)
        try:
            result = gateway.exec('gov-0', ['/usr/bin/true'])
            detail[label] = result
            checks += prefixed(label, refusal_checks(bench, result, 'RESOURCE_AUTHORITY_UNAVAILABLE', 'absent'))
            checks.append(check(f'{label}: the refusal names the registration state {state}',
                                f'(registration: {state}' in (result.get('message') or ''), message=result.get('message')))
            checks += prefixed(label, off_still_runs(gateway))
        finally:
            bench.close(gateway)
    return checks, detail


def fault_race(bench):
    """terminate_process around a governed process's own exit, alternating pipe and PTY: each ends
    once with one final status, is launched once and released, and nothing stays charged."""
    gateway = bench.gateway('main')
    rows, attempts = [], []
    for index in range(12):
        tty = bool(index % 2)
        started = gateway.exec('gov-0', ['/bin/sleep', '0.3'], tty=tty)
        if started['outcome'] != 'started':
            rows.append({'index': index, 'started': started})
            continue
        time.sleep(0.25 + 0.01 * (index % 6))
        try:
            gateway.terminate(started['process_id'])
            answer = 'acknowledged'
        except ToolError as error:
            answer = 'transport' if error.transport else error.code
        status = gateway.wait_exited(started['process_id'])
        attempts.append(attempt_of(gateway, started['process_id']))
        rows.append({'index': index, 'tty': tty, 'terminate': answer, 'termination': (status or {}).get('termination')})
    found, charged = host.settle_attempts([a for a in attempts if a], bench.database, 30, bench.authority['consumer'])
    launches = gateway.launches()
    checks = [check('every round started', all('started' not in row for row in rows), rows=rows),
              check('terminate_process always answered', all(row.get('terminate') != 'transport' for row in rows)),
              check('each ended once as exited or terminated',
                    all(row.get('termination') in ('exited', 'terminated') for row in rows if 'tty' in row)),
              check('each was launched once', len(attempts) == len(rows) and all(count == 1 for count in launches.values()),
                    launches=launches),
              check('each was released, tracking complete',
                    None not in attempts and all(released_cleanly(found.get(a)) for a in attempts), charged=charged)]
    return checks, {'rounds': rows}


def fault_shutdown(bench):
    """The gateway shuts down, its input closed, while a governed pipe and PTY process run (in
    `uds` mode the worker goes with it): their scopes end, DevGuard releases them once it sees
    that, and a new gateway registers and runs without replaying either."""
    gateway = bench.gateway('main')
    authority, _ = bench.registration(gateway, 'gov-0')
    owner = bench.owner_pid
    checks, roots, attempts = [], [], []
    for workspace, tty in (('gov-0', False), ('gov-1', True)):
        started = gateway.exec(workspace, ['/bin/sleep', '30'], tty=tty)
        checks.append(started_checks(f'{workspace} ({io_name(tty)})', started))
        entry = gateway.execution(started.get('process_id')) or {}
        roots.append(entry.get('scope_root_pid'))
        attempts.append(entry.get('attempt_id'))
    closed = gateway.close()
    ended = [host.wait_group_gone(root, limit=15) if root else None for root in roots]
    found, charged = host.settle_attempts([a for a in attempts if a], bench.database, 30, bench.authority['consumer'])
    successor = bench.gateway('successor')
    registered, _ = bench.registration(successor, 'gov-0')
    fresh = successor.exec('gov-0', ['/bin/echo', 'fresh'])
    fresh_checks, _, _ = ran(successor, fresh, 'fresh\n', 'the successor')
    launched = [view for view in host.journal_attempts(None, bench.database, bench.authority['consumer']).values()
                if view['launched']]
    checks += [check('the gateway exited by itself', closed['stopped'] is None and closed['returncode'] == 0, **closed),
               check('the execution owner is gone', gone(owner) if owner else False, owner=owner),
               check("both scopes' process groups ended", None not in roots and None not in ended,
                     ended_s=[None if value is None else round(value, 3) for value in ended]),
               check('both attempts were released, tracking complete',
                     None not in attempts and all(released_cleanly(found.get(a)) for a in attempts), charged=charged,
                     views=[found.get(a) for a in attempts]),
               check('the successor registered', (registered.get('registration') or {}).get('state') == 'registered',
                     authority=registered)]
    checks += fresh_checks
    checks.append(check('nothing was launched again', len(launched) == 3, launched=len(launched)))
    return checks, {'closed': closed, 'authority': authority}


def fault_worker_killed(bench):
    """`uds`: the worker is killed while it prepares a governed execution (the authority held, so
    its session waits): the gateway answers the call, the command never runs, DevGuard never
    hands the attempt to a helper and holds nothing of it once a late admission expired."""
    gateway = bench.gateway('main')
    bench.registration(gateway, 'gov-0')
    worker = bench.owner_pid
    marker = bench.roots['gov-0'] / 'ran'
    answer = {}

    def call():
        answer.update(gateway.exec('gov-0', ['/bin/sh', '-c', f"touch '{marker}'"]))
    bench.fixture.command(f'hold {HOLD_MS}')
    held = time.monotonic()
    caller = threading.Thread(target=call, daemon=True)
    caller.start()
    time.sleep(0.1)
    killed = False
    if worker and worker != gateway.pid:
        try:
            os.kill(worker, signal.SIGKILL)
            killed = True
        except ProcessLookupError:
            pass
    caller.join(timeout=60)
    until(held + HOLD_MS / 1000 + 6.0)
    view = settled_view(bench, answer.get('attempt_id'))
    alive = gateway.process.poll() is None
    try:
        later = gateway.exec('gov-1', ['/bin/echo', 'later'])
    except ToolError as error:
        later = {'outcome': 'error', 'code': error.code}
    checks = [check('the worker was killed during the call', killed and caller.is_alive() is False, worker=worker),
              check('the gateway answered the call', answer.get('outcome') in ('refused', 'started')
                    and answer.get('code') not in host.TRANSPORT_CODES, answer=answer),
              check('the command never ran', not marker.exists()),
              check('DevGuard never handed it to a helper', view is None or view['scope_root_pid'] is None, view=view),
              check('nothing of it stays charged', not (view and view['charged']), view=view),
              check('the gateway still runs', alive)]
    return checks, {'answer': answer, 'attempt': view, 'later': later}


FAULTS = [('descendant', IO, fault_descendant), ('escape', IO, fault_escape),
          ('stalled-observer', IO, fault_stalled_observer), ('authority-lost', None, fault_authority_lost),
          ('authority-restart', None, fault_authority_restart), ('authority-stalled', None, fault_authority_stalled),
          ('helper-ends', None, fault_helper_ends), ('credential', None, fault_credential),
          ('terminate-race', None, fault_race), ('shutdown', None, fault_shutdown)]


def faults(args, live, evidence):
    if platform.system() != 'Darwin':
        return {'verdict': 'incomplete', 'problems': ['the fault cases need native host evidence (macOS)'],
                'scenarios': []}
    writer = host.Writer()
    results = []
    try:
        for mode in args.modes:
            cases = FAULTS + ([('worker-killed', None, fault_worker_killed)] if mode == 'uds' else [])
            for name, ios, function in cases:
                for tty in (ios if ios else (None,)):
                    run_fault(results, args, live, evidence, writer, mode, None if tty is None else io_name(tty),
                              name, function)
    finally:
        evidence.write_json('writer.json', writer.close())
    verdict = analysis.suite_verdict([entry['verdict'] for entry in results])
    return {'verdict': verdict, 'scenarios': [{key: entry[key] for key in ('scenario', 'mode', 'io', 'verdict')}
                                              for entry in results]}


# ---------------------------------------------------------------- control-path sampling

class Schedule(threading.Thread):
    """Runs `action(slot)` on a fixed schedule from `start` (monotonic seconds): every `period`
    seconds, or at the given `offsets`, until stopped. A slot whose moment passed while the
    previous action still ran is recorded as missed, never run late, as DevGuard's protocol
    counts it; so is one that passed before `stop` while the last action ran. Every row goes to
    the writer and is kept."""

    def __init__(self, writer, path, action, start, period=None, offsets=None):
        super().__init__(daemon=True)
        self.writer, self.path, self.action, self.begin = writer, path, action, start
        self.period, self.offsets = period, offsets
        self.halt = threading.Event()
        self.rows = []
        self.ended = start
        self.stopped_at = None

    def moments(self):
        if self.offsets is not None:
            return (self.begin + offset for offset in self.offsets)
        return (self.begin + slot * self.period for slot in itertools.count())

    def keep(self, row):
        self.rows.append(row)
        self.writer.write(self.path, row)

    def run(self):
        for slot, moment in enumerate(self.moments()):
            if self.halt.is_set() and (self.stopped_at is None or moment >= self.stopped_at):
                return
            if self.ended > moment:
                self.keep({'slot': slot, 'unix': time.time(), 'outcome': 'missed_slot'})
                continue
            if self.halt.wait(max(0.0, moment - time.monotonic())):
                return
            row = {'slot': slot, 'unix': time.time()}
            try:
                row.update(self.action(slot))
            except ToolError as error:
                row.update(outcome='error', code=error.code, message=error.message[:300], latency_ms=error.latency_ms)
            except Exception as error:  # noqa: BLE001 - a failed sample is evidence
                row.update(outcome='error', code='harness', message=f'{type(error).__name__}: {error}'[:300])
            self.ended = time.monotonic()
            self.keep(row)

    def stop(self):
        self.stopped_at = time.monotonic()
        self.halt.set()
        self.join()
        return self.rows


class ReleaseWatcher(threading.Thread):
    """Times DevGuard's release of terminated targets: from the termination request until the
    journal, read every 100 ms, shows the attempt holding nothing. One row per watched slot; an
    attempt still charged after `limit` seconds is recorded unreleased."""

    def __init__(self, bench, writer, path, limit=60.0):
        super().__init__(daemon=True)
        self.bench, self.writer, self.path, self.limit = bench, writer, path, limit
        self.pending = {}
        self.lock = threading.Lock()
        self.halt = threading.Event()
        self.rows = []
        self.read_errors = 0

    def watch(self, slot, attempt, asked):
        with self.lock:
            self.pending[slot] = (attempt, asked)

    def poll(self, final=False):
        with self.lock:
            pending = dict(self.pending)
        if not pending:
            return
        ids = [attempt for attempt, _ in pending.values() if attempt]
        try:
            found = host.journal_attempts(ids, self.bench.database, self.bench.authority['consumer']) if ids else {}
        except Exception:  # noqa: BLE001 - a busy journal is read again
            self.read_errors += 1
            found = None
        now = time.monotonic()
        for slot, (attempt, asked) in sorted(pending.items()):
            view = (found or {}).get(attempt)
            if view is not None and not view['charged']:
                released = round((now - asked) * 1000, 3)
            elif final or not attempt or now - asked >= self.limit:
                released = None
            else:
                continue
            with self.lock:
                self.pending.pop(slot, None)
            row = {'slot': slot, 'attempt_id': attempt, 'released_ms': released,
                   'phase': (view or {}).get('phase'), 'release_reason': (view or {}).get('release_reason')}
            self.rows.append(row)
            self.writer.write(self.path, row)

    def run(self):
        while not self.halt.is_set():
            self.poll()
            self.halt.wait(0.1)

    def finish(self, limit=60.0):
        """Wait up to `limit` seconds for the pending releases; the rest are recorded unreleased."""
        deadline = time.monotonic() + limit
        while self.pending and time.monotonic() < deadline and self.is_alive():
            time.sleep(0.2)
        self.halt.set()
        if self.is_alive():
            self.join()
        self.poll(final=True)
        return self.rows


def status_action(gateway, process_id):
    def sample(slot):
        payload, latency = gateway.status(process_id, timeout=10)
        return {'outcome': 'answered', 'latency_ms': latency, 'state': payload.get('state')}
    return sample


class Echo:
    """A numbered line written with write_stdin to the running governed `cat`, timed until
    read_process returns it."""

    def __init__(self, gateway, process_id):
        self.gateway, self.process_id, self.cursor = gateway, process_id, 0

    def __call__(self, slot):
        line = f'io-{slot}\n'
        began = time.monotonic()
        self.gateway.call('write_stdin', {'process_id': self.process_id, 'data': line}, timeout=10)
        _, self.cursor, eof, found = self.gateway.read_until(self.process_id, line, limit=10, period=0.01,
                                                             cursor=self.cursor, timeout=10)
        return {'outcome': 'echoed' if found else 'not_echoed',
                'latency_ms': round((time.monotonic() - began) * 1000, 3), 'eof': eof}


class Terminations:
    """Each slot terminates one governed target and times it as DevGuard's protocol does: the
    acknowledgement to terminate_process's answer; the target's end to its exited status and to
    its scope's process group gone; DevGuard's release through the watcher. `target(slot)` gives
    a started target, or the result of one that did not start: refused for capacity or pressure
    (RESOURCE_UNAVAILABLE) it is not a sample, otherwise a missing one."""

    def __init__(self, gateway, watcher, target):
        self.gateway, self.watcher, self.target = gateway, watcher, target

    def __call__(self, slot):
        started = self.target(slot)
        if started.get('outcome') != 'started':
            code = started.get('code')
            return {'outcome': 'not_started' if code == 'RESOURCE_UNAVAILABLE' else 'error', 'code': code,
                    'attempt_id': started.get('attempt_id'), 'latency_ms': started.get('latency_ms'),
                    'tty': started.get('tty')}
        pid = started['process_id']
        entry = self.gateway.execution(pid) or {}
        root, attempt = entry.get('scope_root_pid'), entry.get('attempt_id')
        asked = time.monotonic()
        ack = self.gateway.terminate(pid, timeout=10)
        self.watcher.watch(slot, attempt, asked)
        status = self.gateway.wait_exited(pid, limit=30, period=0.02)
        exited = round((time.monotonic() - asked) * 1000, 3) if status else None
        scope = host.wait_group_gone(root, limit=30) if root else None
        return {'outcome': 'acknowledged', 'tty': started.get('tty'), 'ack_ms': ack,
                'termination': (status or {}).get('termination'), 'exited_ms': exited,
                'scope_exit_ms': round((time.monotonic() - asked) * 1000, 3) if scope is not None else None,
                'attempt_id': attempt, 'scope_root_pid': root}


def fresh_target(gateway, workspace):
    """A new governed `sleep` per slot, alternating pipe and PTY."""
    def target(slot):
        tty = bool(slot % 2)
        return dict(gateway.exec(workspace, ['/bin/sleep', '600'], tty=tty), tty=tty)
    return target


def pooled_target(pool):
    """Targets started before a fault, one per slot."""
    def target(slot):
        if not pool:
            return {'outcome': 'error', 'code': 'harness', 'message': 'no prepared target'}
        return pool.pop(0)
    return target


def admission_action(gateway, workspace):
    """A new governed execution that the authority refuses or, where it admits, ends at once:
    refused, admitted or a transport error, with the call's round trip."""
    def sample(slot):
        result = gateway.exec(workspace, ['/usr/bin/true'])
        row = {'latency_ms': result.get('latency_ms'), 'code': result.get('code'), 'attempt_id': result.get('attempt_id')}
        if result['outcome'] == 'started':
            row.update(outcome='admitted', attempt_id=attempt_of(gateway, result['process_id']))
            gateway.wait_exited(result['process_id'], limit=30)
        else:
            row['outcome'] = 'refused' if result['outcome'] == 'refused' else 'error'
        return row
    return sample


def end_target(gateway, process_id):
    """Terminate a status target at the end of its interval: effective when it is reported
    terminated and its scope's process group is gone."""
    root = (gateway.execution(process_id) or {}).get('scope_root_pid')
    try:
        ack = gateway.terminate(process_id, timeout=10)
        status = gateway.wait_exited(process_id, limit=30)
    except ToolError as error:
        return {'process_id': process_id, 'error': error.code, 'effective': False}
    scope = host.wait_group_gone(root, limit=30) if root else None
    termination = (status or {}).get('termination')
    return {'process_id': process_id, 'ack_ms': ack, 'termination': termination, 'scope_gone': scope is not None,
            'effective': termination == 'terminated' and scope is not None}


def read_rows(directory, name):
    path = Path(directory) / f'{name}.jsonl'
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def raw_hashes(directory):
    directory = Path(directory)
    return {str(path.relative_to(directory)): host.sha256(path) for path in sorted(directory.rglob('*'))
            if path.is_file() and path.suffix in ('.jsonl', '.json') and path.name not in ('interval.json', 'phase.json')}


# ---------------------------------------------------------------- authority (fixture authority faults)
# Control-path latency while the fixture authority is paused, stalled, restarted and stopped
# (plan §6, "Authority failure"): process control must not depend on the authority, and new
# governed executions must be refused promptly where it cannot answer.

PHASE_S = 15.0
PHASE_PERIODS = {'status': 1.0, 'termination': PHASE_S, 'io': 3.0}
PHASE_TERMINATIONS = (3.0, 7.0, 11.0)
PHASE_ADMISSIONS = (1.0, 5.0, 9.0, 13.0)
# Each phase's fault, what new governed executions must meet, and whether DevGuard can still
# release terminated targets (it cannot after a restart, which turns them Suspect, or once
# stopped).
AUTHORITY_PHASES = (('baseline', None, 'admitted', True), ('paused', 'pause', 'admitted', True),
                    ('stalled', 'hold', 'refused', True), ('restarted', 'restart', 'admitted', False),
                    ('stopped', 'kill', 'refused', False))
REFUSED_UNREACHABLE = ('RESOURCE_AUTHORITY_UNAVAILABLE', 'ADMISSION_UNKNOWN')
# A host sample that goes stale while the authority is held makes DevGuard close new work until
# its samples have been clear for 30 seconds, so a phase after `stalled` waits that long, and
# some margin, for its termination targets to be admitted.
READMIT_LIMIT_S = 90.0


def start_targets(gateway, count, limit=READMIT_LIMIT_S, period=1.0):
    """A phase's termination targets, alternating pipe and PTY. A target refused for capacity or
    pressure (`RESOURCE_UNAVAILABLE`) is tried again every `period` seconds until it is admitted
    or `limit` seconds have passed. Returns the targets, the refusals met, and how long it took."""
    began = time.monotonic()
    pool, refusals = [], 0
    for n in range(count):
        tty = bool(n % 2)
        while True:
            entry = gateway.exec(f'term-{n}', ['/bin/sleep', '600'], tty=tty)
            if (entry['outcome'] == 'started' or entry.get('code') != 'RESOURCE_UNAVAILABLE'
                    or time.monotonic() - began >= limit):
                break
            refusals += 1
            time.sleep(period)
        pool.append(dict(entry, tty=tty))
    return pool, refusals, round(time.monotonic() - began, 3)


def authority_workspaces(work):
    governed = host.required(**FIXTURE_REQUEST)
    return {name: (work / f'ws/{name}', governed)
            for name in ('status-pipe', 'status-pty', 'admit', 'term-0', 'term-1', 'term-2')}


def phase_checks(expectation, admissions):
    """Whether each new governed execution met the phase's expectation."""
    ran = [row for row in admissions if row.get('outcome') != 'missed_slot']
    if expectation == 'admitted':
        passed = bool(ran) and all(row.get('outcome') == 'admitted' for row in ran)
    else:
        passed = bool(ran) and all(row.get('outcome') == 'refused' and row.get('code') in REFUSED_UNREACHABLE
                                   for row in ran)
    return [check(f'new governed executions are {expectation}', passed,
                  outcomes=[(row.get('outcome'), row.get('code'), row.get('latency_ms')) for row in ran])]


def phase_verdict(record, rows):
    """A phase's verdict from its raw rows and recorded checks."""
    metrics = analysis.control_metrics(rows, record['seconds'], PHASE_PERIODS, record['release_expected'],
                                       len(PHASE_TERMINATIONS))
    verdict, reasons = analysis.interval_verdict('fault', record['invalid'], metrics)
    failed = [item['check'] for item in record['checks'] if item['passed'] is False]
    if failed and verdict != 'inconclusive':
        verdict, reasons = 'fail', reasons + failed
    return verdict, reasons, metrics


def authority_phase(bench, gateway, evidence, mode, targets, echo, phase, fault, expectation, release_expected):
    """One phase: its termination targets started before the fault, once DevGuard admits them,
    the fault applied, the schedules run for PHASE_S seconds, then the fault lifted where it can
    be. The samples have their own writer, closed before their files are hashed."""
    raw = evidence.file(f'{mode}/authority/{phase}/x').parent
    samples = host.Writer()
    schedules, watcher, invalid = {}, None, []
    try:
        pool, refusals, admitted_s = start_targets(gateway, len(PHASE_TERMINATIONS))
        prepared = [entry['process_id'] for entry in pool if entry['outcome'] == 'started']
        if len(prepared) < len(pool):
            invalid.append('a termination target could not be started before the fault')
        watcher = ReleaseWatcher(bench, samples, raw / 'releases.jsonl', limit=60)
        watcher.start()
        applied = None
        if fault == 'hold':
            applied = bench.fixture.command(f'hold {int((PHASE_S + 1) * 1000)}')
        elif fault == 'kill':
            bench.fixture.kill()
            applied = {'killed': True}
        elif fault:
            applied = bench.fixture.command(fault)
        began = time.monotonic()
        begin = began + 0.2
        started_unix = time.time()
        schedules = {
            'status_pipe': Schedule(samples, raw / 'status_pipe.jsonl', status_action(gateway, targets['pipe']), begin,
                                    period=PHASE_PERIODS['status']),
            'status_pty': Schedule(samples, raw / 'status_pty.jsonl', status_action(gateway, targets['pty']), begin,
                                   period=PHASE_PERIODS['status']),
            'terminations': Schedule(samples, raw / 'terminations.jsonl',
                                     Terminations(gateway, watcher, pooled_target(pool)), begin,
                                     offsets=PHASE_TERMINATIONS),
            'refusals': Schedule(samples, raw / 'refusals.jsonl', admission_action(gateway, 'admit'), begin,
                                 offsets=PHASE_ADMISSIONS),
            'io': Schedule(samples, raw / 'io.jsonl', echo, begin + 0.5, period=PHASE_PERIODS['io'])}
        for schedule in schedules.values():
            schedule.start()
        until(begin + PHASE_S)
        rows = {name: schedule.stop() for name, schedule in schedules.items()}
        seconds = round(time.monotonic() - begin, 3)
        if fault == 'pause':
            bench.fixture.command('resume')
        elif fault == 'hold':
            until(began + PHASE_S + 1.5)
        rows['releases'] = watcher.finish(limit=30 if release_expected else 2)
    except BaseException:
        for thread in list(schedules.values()) + ([watcher] if watcher else []):
            thread.halt.set()
        samples.close(limit=10)
        raise
    for process_id in prepared:
        try:
            if gateway.status(process_id, timeout=10)[0].get('state') != 'exited':
                end_target(gateway, process_id)
        except ToolError:
            pass
    written = samples.close()
    if written['error_count']:
        invalid.append(f"raw evidence could not be written: {written['errors'][:3]}")
    record = {'mode': mode, 'phase': phase, 'fault': fault, 'applied': applied, 'started_unix': started_unix,
              'seconds': seconds, 'release_expected': release_expected, 'expectation': expectation,
              'termination_targets': {'refusals_before_admitted': refusals, 'admitted_s': admitted_s},
              'invalid': invalid, 'checks': phase_checks(expectation, rows['refusals']),
              'read_errors': watcher.read_errors, 'evidence_writer': written, 'raw': raw_hashes(raw)}
    record['verdict'], record['reasons'], record['metrics'] = phase_verdict(record, rows)
    evidence.write_json(f'{mode}/authority/{phase}/phase.json', record)
    print(f"qualify: {mode} authority {phase}: {record['verdict']} {record['reasons']}", flush=True)
    return record


SETUP_FAILED = ['the status targets could not be started']


def authority_mode(bench, evidence, mode):
    gateway = bench.gateway('control', timeout_s=3600)
    pipe = gateway.exec('status-pipe', ['/bin/cat'])
    pty = gateway.exec('status-pty', ['/bin/sleep', '86400'], tty=True)
    if pipe['outcome'] != 'started' or pty['outcome'] != 'started':
        record = {'mode': mode, 'phase': 'setup', 'verdict': 'inconclusive', 'reasons': SETUP_FAILED,
                  'targets': [pipe, pty]}
        evidence.write_json(f'{mode}/authority/setup/phase.json', record)
        return [record]
    targets = {'pipe': pipe['process_id'], 'pty': pty['process_id']}
    echo = Echo(gateway, pipe['process_id'])
    return [authority_phase(bench, gateway, evidence, mode, targets, echo, *phase) for phase in AUTHORITY_PHASES]


def authority(args, live, evidence):
    if platform.system() != 'Darwin':
        return {'verdict': 'incomplete', 'problems': ['the authority faults need native host evidence (macOS)'],
                'phases': []}
    records = []
    for mode in args.modes:
        writer = host.Writer()
        bench = None
        try:
            bench = fixture_bench(args, live, evidence, writer, mode, 'authority', authority_workspaces)
            records += authority_mode(bench, evidence, mode)
        finally:
            if bench is not None:
                bench.teardown()
            evidence.write_json(f'{mode}/writer.json', writer.close())
    verdict = analysis.suite_verdict([record['verdict'] for record in records])
    return {'verdict': verdict, 'phases': [{key: record[key] for key in ('mode', 'phase', 'verdict', 'reasons')}
                                           for record in records]}


# ---------------------------------------------------------------- control (installed service, DG-1 load)
# Control-path latency of CodeSpace's gateway against the installed service while DevGuard's own
# development load runs (plan §6). The load is DG-1's six consumers from DevGuard's measure.py at
# the given checkout, run through the installed release's `devguard exec`, so pressure and queueing
# are the service's own; the targets fit with that load in half the work capacity, the Constrained
# capacity, as DG-1's own probe does.

CONTROL_PROTOCOL = {'idle_s': 600, 'load_s': 1800, 'repetitions': 3, 'combinations': list(MODES)}
CONTROL_REHEARSAL = {'idle_s': 30, 'load_s': 60, 'repetitions': 1, 'combinations': list(MODES)}
PERIODS = {'status': 1.0, 'termination': 20.0, 'io': 5.0, 'refusal': 30.0}
HOST_PERIOD_S = 5.0
SERVICE_PERIOD_S = 60.0
FOOTPRINT_PERIOD_S = 5.0
CONTROL_REQUEST = {'cpu_milli': 25, 'memory_bytes': 16 << 20, 'tasks': 2}
# Status targets run for a whole interval, the load's completion limit included.
CONTROL_TIMEOUT_S = 14400


def load_measure(checkout):
    """DevGuard's measure.py from a checkout, for DG-1's load consumers and their settlement."""
    scripts = Path(checkout).resolve() / 'scripts'
    sys.path.insert(0, str(scripts))
    spec = importlib.util.spec_from_file_location('devguard_measure', scripts / 'measure.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def control_workspaces(work, capacity):
    governed = host.required(**CONTROL_REQUEST)
    hog = dict(CONTROL_REQUEST, cpu_milli=((capacity or {}).get('cpu_milli') or 1_000_000) + 1)
    return {'status-pipe': (work / 'ws/status-pipe', governed), 'status-pty': (work / 'ws/status-pty', governed),
            'term': (work / 'ws/term', governed), 'hog': (work / 'ws/hog', host.required(**hog))}


def launched_count(database):
    return sum(1 for view in host.journal_attempts(None, database).values() if view['launched'])


def journal_findings(database, gateway, rows, launched_before):
    """The harness's attempts once settled: none still charged, none launched twice (the gateway's
    log against the journal's count of launched attempts), and no uncertain attempt launched."""
    attempts = set(gateway.attempts())
    uncertain = set()
    for name in ('refusals', 'terminations'):
        for row in rows[name]:
            if row.get('attempt_id'):
                attempts.add(row['attempt_id'])
                if row.get('code') == 'ADMISSION_UNKNOWN':
                    uncertain.add(row['attempt_id'])
    try:
        found, charged = host.settle_attempts(sorted(attempts), database, 60)
        launched_after = launched_count(database)
    except Exception as error:  # noqa: BLE001 - an unreadable journal invalidates the interval
        return {'error': f'{type(error).__name__}: {error}'}
    launches = gateway.launches()
    logged = sum(launches.values())
    in_journal = None if launched_before is None else launched_after - launched_before
    unaccounted = max(0, in_journal - logged) if in_journal is not None else 0
    return {'harness_attempts': len(attempts), 'found': len(found), 'still_charged': charged,
            'launched_in_log': logged, 'launched_in_journal': in_journal,
            'duplicate_launches': sum(1 for count in launches.values() if count > 1) + unaccounted,
            'readmitted_uncertain': sum(1 for attempt in uncertain if (found.get(attempt) or {}).get('launched'))}


def footprint(owner, gateway, mode):
    def sample():
        return {'owner': host.process_footprint(owner) if owner else None,
                'gateway': host.process_footprint(gateway.pid) if mode == 'uds' else None}
    return sample


def interval_metrics(record, rows, sampled, runs, release_id, service_pid, reservation):
    """A control interval's metrics from its raw rows, as the run computes them and `verify`
    recomputes them."""
    metrics = analysis.control_metrics(rows, record['seconds'], PERIODS)
    targets = record.get('status_targets') or []
    metrics['termination']['status_target_effective'] = (all(t.get('effective') for t in targets)
                                                         if targets else None)
    _, failure = analysis.service_findings(sampled['service'], release_id, service_pid)
    metrics['service'] = {'rows': len(sampled['service']), 'failure': failure}
    metrics['host'] = analysis.host_peaks(sampled['host'])
    metrics['footprint'] = {
        'owner': analysis.footprint_metrics([r['owner'] for r in sampled['footprint'] if r.get('owner')], reservation),
        'gateway': analysis.footprint_metrics([r['gateway'] for r in sampled['footprint'] if r.get('gateway')], None)}
    metrics['journal'] = record.get('journal') or {}
    if record['kind'] == 'load':
        metrics['load'] = analysis.load_metrics(runs, record['started_unix'], record['ended_unix'],
                                                record.get('consumers') or ())
        metrics['load']['issues'] += record.get('load_issues') or []
        metrics['load']['failures'] += record.get('load_failures') or []
    return metrics


def control_interval(context, mode, kind, seconds, key, service_pid):
    args, live, evidence, measure, release, dg_release, work, reservation = (
        context[name] for name in ('args', 'live', 'evidence', 'measure', 'release', 'dg_release', 'work', 'reservation'))
    prefix = f'{key}/{kind}'
    raw = evidence.file(prefix + '/x').parent
    record = {'kind': kind, 'combination': mode, 'invalid': []}
    invalid = record['invalid']
    launched_before = None
    try:
        before = host.journal_charged(args.journal)
        launched_before = launched_count(args.journal)
        if sum(before['attempts'].values()) or before['leases']:
            invalid.append("work other than the harness's was charged when the interval began")
    except Exception as error:  # noqa: BLE001 - an unreadable journal invalidates the interval
        before = {'error': str(error)}
        invalid.append(f'the journal could not be read: {error}')
    record['journal_before'] = before
    writer = host.Writer()
    directory = scratch()
    bench = Bench(args, live, evidence, writer, mode, directory,
                  control_workspaces(directory, host.work_capacity(args.service_log)), installed_authority(args),
                  prefix=prefix)
    schedules, samplers, load, watcher, targets = {}, {}, [], None, {}
    try:
        gateway = bench.gateway('control', timeout_s=CONTROL_TIMEOUT_S)
        record['resource_authority'], _ = bench.registration(gateway, 'status-pipe')
        targets = {'pipe': gateway.exec('status-pipe', ['/bin/cat']),
                   'pty': gateway.exec('status-pty', ['/bin/sleep', '86400'], tty=True)}
        if any(target['outcome'] != 'started' for target in targets.values()):
            invalid.append('the status targets could not be started: '
                           + ', '.join(str(target.get('code')) for target in targets.values()))
        watcher = ReleaseWatcher(bench, writer, raw / 'releases.jsonl')
        watcher.start()
        begin = time.monotonic() + 0.5
        for kind_, target in targets.items():
            if target['outcome'] == 'started':
                schedules[f'status_{kind_}'] = Schedule(writer, raw / f'status_{kind_}.jsonl',
                                                       status_action(gateway, target['process_id']), begin,
                                                       period=PERIODS['status'])
        if targets['pipe']['outcome'] == 'started':
            schedules['io'] = Schedule(writer, raw / 'io.jsonl', Echo(gateway, targets['pipe']['process_id']),
                                       begin + 0.25, period=PERIODS['io'])
        schedules['terminations'] = Schedule(writer, raw / 'terminations.jsonl',
                                             Terminations(gateway, watcher, fresh_target(gateway, 'term')),
                                             begin + PERIODS['termination'] / 2, period=PERIODS['termination'])
        schedules['refusals'] = Schedule(writer, raw / 'refusals.jsonl', admission_action(gateway, 'hog'),
                                         begin + PERIODS['refusal'] / 3, period=PERIODS['refusal'])
        samplers = {'host': host.Sampler(writer, raw / 'host.jsonl', HOST_PERIOD_S, host.host_sample),
                    'service': host.Sampler(writer, raw / 'service.jsonl', SERVICE_PERIOD_S, release.state),
                    'footprint': host.Sampler(writer, raw / 'footprint.jsonl', FOOTPRINT_PERIOD_S,
                                              footprint(bench.owner_pid, gateway, mode))}
        until(begin)
        record['started_unix'] = time.time()
        for thread in list(schedules.values()) + list(samplers.values()):
            thread.start()
        if kind == 'load':
            (raw / 'runs').mkdir()
            load = measure.consumers(SimpleNamespace(qualify_bin=Path(args.qualify_bin)), dg_release, args.load, work,
                                     raw / 'runs', begin + seconds, key, args.rehearsal)
            for consumer in load:
                consumer.start()
            limit = begin + seconds + measure.LOAD_COMPLETION_LIMIT_S
            for consumer in load:
                consumer.join(timeout=max(0.0, limit - time.monotonic()))
            if any(consumer.is_alive() for consumer in load):
                invalid.append('the load did not complete within the limit')
                measure.settle_consumers(load)
        else:
            until(begin + seconds)
        record['ended_unix'] = time.time()
        record['seconds'] = round(time.monotonic() - begin, 3)
    except BaseException:
        for thread in list(schedules.values()) + list(samplers.values()) + ([watcher] if watcher else []):
            thread.halt.set()
        writer.close(limit=10)
        bench.teardown()
        raise
    rows = {name: schedules[name].stop() if name in schedules else [] for name in analysis.SAMPLE_FILES
            if name != 'releases'}
    sampled = {name: sampler.stop() for name, sampler in samplers.items()}
    record['status_targets'] = [end_target(gateway, target['process_id']) for target in targets.values()
                                if target['outcome'] == 'started']
    rows['releases'] = watcher.finish(limit=60)
    bench.teardown()
    record['journal'] = journal_findings(args.journal, gateway, rows, launched_before)
    runs = [run for consumer in load for run in consumer.runs]
    if kind == 'load':
        (raw / 'runs.json').write_text(json.dumps(runs, indent=1) + '\n')
        record['consumers'] = [consumer.name for consumer in load]
        errors = {consumer.name: consumer.errors for consumer in load if consumer.errors}
        record['load_issues'] = ['a consumer stopped with an error'] if errors else []
        record['consumer_errors'] = errors
        ids = {attempt for run in runs for attempt in run.get('attempt_ids') or []}
        record['load_journal'] = measure.settle_journal(ids, runs)
        record['load_failures'] = (['load attempts still charged after the interval']
                                   if record['load_journal'].get('still_charged') else [])
    written = writer.close()
    record['evidence_writer'] = written
    if written['error_count']:
        invalid.append(f"raw evidence could not be written: {written['errors'][:3]}")
    if 'error' in record['journal']:
        invalid.append(f"the journal could not be read: {record['journal']['error']}")
    service_invalid, _ = analysis.service_findings(sampled['service'], release.id, service_pid)
    invalid += service_invalid
    record['raw'] = raw_hashes(raw)
    metrics = interval_metrics(record, rows, sampled, runs, release.id, service_pid, reservation)
    record['verdict'], record['reasons'] = analysis.interval_verdict(kind, invalid, metrics)
    record['metrics'] = metrics
    evidence.write_json(prefix + '/interval.json', record)
    print(f"qualify: {key} {kind}: {record['verdict']} {record['reasons']}", flush=True)
    return record


def early_outcome(report):
    """The verdict of a repetition that could not begin its intervals, or None."""
    if not report['service_before']['runs_release']:
        return 'inconclusive', ['the service did not run the release']
    prewarm = report.get('prewarm')
    if prewarm is not None and not any(run.get('result') == 'completed' and run.get('exit_code') == 0
                                       for run in prewarm):
        return 'inconclusive', ['the warm target directory could not be built']
    return None


def repetition_outcome(report, idle, load):
    """A repetition's verdict from its two intervals and the service before and after them."""
    verdict, reasons = analysis.repetition_verdict(idle, load)
    before, after = report['service_before'], report['service_after']
    if verdict == 'pass' and not (after['runs_release'] and after['service_pid'] == before['service_pid']):
        verdict, reasons = 'inconclusive', reasons + ['the service changed after the intervals']
    if report['rehearsal']:
        verdict, reasons = 'inconclusive', reasons + ['rehearsal durations are below the protocol']
    return verdict, reasons


def control_repetition(context, plan, mode, repetition):
    args, evidence, measure, release = (context[name] for name in ('args', 'evidence', 'measure', 'release'))
    key = f'{mode}-{repetition}'
    report = {'schema': 'codespace-control-repetition/v1', 'combination': mode, 'repetition': repetition,
              'rehearsal': args.rehearsal, 'load': args.load, 'service_before': release.state()}
    service_pid = report['service_before']['service_pid']

    def conclude(outcome):
        report['verdict'], report['reasons'] = outcome
        evidence.write_json(f'{key}/report.json', report)
        print(f"qualify: {key} {report['verdict']} {report['reasons']}", flush=True)
        return report

    if report['service_before']['runs_release'] and args.load == 'warm':
        report['prewarm'] = measure.prewarm(SimpleNamespace(qualify_bin=Path(args.qualify_bin)), context['dg_release'],
                                            context['work'], evidence.file(f'{key}/x').parent)
    early = early_outcome(report)
    if early:
        report['service_after'] = release.state()
        return conclude(early)
    idle = control_interval(context, mode, 'idle', plan['idle_s'], key, service_pid)
    load = control_interval(context, mode, 'load', plan['load_s'], key, service_pid)
    report['intervals'] = {'idle': {k: idle[k] for k in ('verdict', 'reasons')},
                           'load': {k: load[k] for k in ('verdict', 'reasons')}}
    for target in sorted((context['work'] / 'targets.noindex').glob(f'cold-{key}-*')):
        shutil.rmtree(target, ignore_errors=True)
    report['service_after'] = release.state()
    return conclude(repetition_outcome(report, idle, load))


def control_preconditions(args, release, measure):
    problems = []
    state = release.state()
    if not state['runs_release']:
        problems.append(f'the service does not run {release.id}')
    charged = host.journal_charged(args.journal)
    if sum(charged['attempts'].values()) or charged['leases']:
        problems.append(f'the service has charged work: {charged}')
    doctor = release.doctor()
    if not doctor.get('satisfied'):
        problems.append('devguard doctor is not satisfied')
    config = Path(args.host_config)
    if (consumer_settings(config.read_text()) if config.exists() else {}).get('role') != 'control_service':
        problems.append('host.toml has no codespace control-service consumer')
    if not (host.run_text('rustc', '--version') or '').startswith(f'rustc {PINNED_RUST} '):
        problems.append(f'the Cargo load needs Rust {PINNED_RUST} first in PATH')
    if not Path(args.qualify_bin).is_file():
        problems.append('--qualify-bin is not a file')
    commit = json.loads(release.manifest.read_text())['source']['commit']
    if host.run_text('git', '-C', measure.ROOT, 'cat-file', '-e', f'{commit}^{{commit}}') is None:
        problems.append(f"the DevGuard checkout does not have the release's source commit {commit}")
    return problems


def control(args, live, evidence):
    if platform.system() != 'Darwin':
        return {'verdict': 'inconclusive', 'problems': ['the control measurement runs only on macOS']}
    measure = load_measure(args.devguard_checkout)
    live.measure = measure
    release = host.Release(args.release)
    plan = dict(CONTROL_REHEARSAL if args.rehearsal else CONTROL_PROTOCOL, combinations=list(args.modes),
                load=args.load, rehearsal=args.rehearsal)
    if args.repetitions is not None:
        plan['repetitions'] = args.repetitions
    problems = control_preconditions(args, release, measure)
    work = Path(args.work).resolve()
    if work.exists():
        problems.append(f'{work} already exists')
    if problems:
        return {'verdict': 'inconclusive', 'problems': problems}
    work.mkdir(parents=True)
    manifest = json.loads(release.manifest.read_text())
    measure.export_source(manifest['source']['commit'], work)
    config = Path(args.host_config)
    reservation = consumer_settings(config.read_text()).get('control_reservation')
    evidence.write_json('run.json', {
        'schema': 'codespace-control-run/v1', 'argv': sys.argv, 'plan': plan, 'protocol': CONTROL_PROTOCOL,
        'periods': PERIODS, 'request': CONTROL_REQUEST, 'reservation': reservation,
        'targets': {'status_p99_ms': analysis.STATUS_P99_MS, 'termination_ack_p99_ms': analysis.TERMINATION_P99_MS},
        'release': {'id': release.id, **release.artifacts(), 'state': release.state()}, 'policy': policy(release, args),
        'work_capacity': host.work_capacity(args.service_log), 'host': host.host_facts(),
        'harness': {'commit': git('rev-parse', 'HEAD'), 'dirty': dirty(),
                    'measure_sha256': host.sha256(Path(measure.__file__)),
                    'devguard_checkout': git('rev-parse', 'HEAD', root=measure.ROOT),
                    'qualify_sha256': host.sha256(args.qualify_bin), 'rustc': host.run_text('rustc', '--version')},
        'started_unix': time.time()})
    context = {'args': args, 'live': live, 'evidence': evidence, 'measure': measure, 'release': release,
               'dg_release': measure.Release(args.release), 'work': work, 'reservation': reservation}
    # The display and the system stay awake for the run; caffeinate ends with this process.
    awake = subprocess.Popen(['/usr/bin/caffeinate', '-d', '-i', '-w', str(os.getpid())],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    live.workers.append(awake)
    results = {}
    try:
        for mode in plan['combinations']:
            results[mode] = [control_repetition(context, plan, mode, repetition)
                             for repetition in range(1, plan['repetitions'] + 1)]
    finally:
        awake.terminate()
    combinations = {}
    for mode, reports in results.items():
        combinations[mode] = {
            'verdict': analysis.combination_verdict([report['verdict'] for report in reports], plan['repetitions']),
            'repetitions': [{'repetition': report['repetition'], 'verdict': report['verdict'],
                             'reasons': report['reasons'],
                             'report_sha256': host.sha256(evidence.path / f"{mode}-{report['repetition']}/report.json")}
                            for report in reports]}
    return {'verdict': analysis.overall_verdict(combinations, plan, CONTROL_PROTOCOL, args.rehearsal),
            'plan': plan, 'combinations': combinations}


# ---------------------------------------------------------------- recovery (P1R-C06): the independent Runner
# docs/recovery-qualification.md is the protocol. The harness starts an independent Runner as an
# operator would, in its own session (D3), and drives gateways that connect to it, end in every
# way a gateway ends and connect again, all over one operations database. With `required` the
# Runner registers itself as the consumer's one instance (D12) and launches through the
# authority's helper; with `off` nothing reaches an authority. Each command the harness runs
# there is tagged: before it becomes the command, it appends its tag to its workspace's marker
# file and writes its PID beside it, so that every time it ran shows and its own end can be timed.

RECOVERY_UNITS = ('recovery-lifecycle', 'recovery-store', 'recovery-fence', 'recovery-reconcile',
                  'recovery-observation')
PARTICIPATIONS = ('off', 'required')
# The process timeout of a recovery gateway, unless a scenario gives its own.
RECOVERY_TIMEOUT_S = 600
MARKERS = '.qualify-markers'
NOT_NATIVE = ('the fixture authority governs only with native host evidence (macOS); Linux `required` stays '
              'unsupported until DG-LINUX')
NO_AUTHORITY = {'socket': None, 'credential_file': None, 'helper': None, 'consumer': host.CONSUMER,
                'generation': host.GENERATION, 'journal': None}
# A line every 0.2 s, numbered at a fixed width, so that a gap or a repeat in the output shows.
BEAT = ['/bin/sh', '-c', "i=0; while :; do i=$((i+1)); printf 'beat %08d\\n' \"$i\"; sleep 0.2; done"]
# More than the Runner's 256 KiB output ring holds, at once.
BURST_LINES = 40000
# Commands a tagged process can be, for the cleanup of what a case left running.
TAGGED_COMMANDS = ('sleep', 'sh', 'cat', 'awk', 'bash', 'dash', 'zsh')
RUNNER_LINES = {'detached': 'the controlling gateway detached', 'lost': 'connection ended without a detach or a stop'}


def burst(cue, lines=BURST_LINES):
    """Each time `cue` appears: remove it and write `lines` numbered lines at once, numbered on from
    the burst before."""
    script = ('k=0; while :; do while [ ! -e "$1" ]; do sleep 0.05; done; rm -f "$1"; '
              f'awk -v k="$k" \'BEGIN {{ for (i = 1; i <= {lines}; i++) printf "burst %08d\\n", k * {lines} + i }}\'; '
              'k=$((k+1)); done')
    return ['/bin/sh', '-c', script, 'burst', str(cue)]


def tagged(root, tag, command):
    """`command`, run so that it first appends `tag` to its workspace's marker file and writes its
    PID beside it, then becomes `command` itself, keeping that PID."""
    script = f'echo "$0" >> "$1/{MARKERS}"; echo $$ > "$1/.qualify-pid-$0"; shift; exec "$@"'
    return ['/bin/sh', '-c', script, tag, str(root), *[str(part) for part in command]]


def markers(root):
    """The tags of a workspace's commands, in the order they started; a tag twice is a command
    that ran twice."""
    try:
        return [line.strip() for line in (Path(root) / MARKERS).read_text().splitlines() if line.strip()]
    except OSError:
        return []


def tag_pid(root, tag, limit=10.0):
    return host.read_pid(Path(root) / f'.qualify-pid-{tag}', limit)


def kill_leftovers(roots, since_unix):
    """Kill the tagged processes a case left running: only a process whose PID a tagged command
    wrote, that started after the case began and runs one of the commands the harness starts, so
    that a PID used again by another process is never touched."""
    killed = []
    for root in roots:
        for path in sorted(Path(root).glob('.qualify-pid-*')):
            pid = host.read_pid(path, limit=0)
            if not pid or not host.pid_alive(pid):
                continue
            facts = host.process_facts(pid)
            if (not facts or facts['started_unix'] is None or facts['started_unix'] < since_unix - 2
                    or Path(facts['command'] or '').name not in TAGGED_COMMANDS):
                continue
            try:
                os.kill(pid, signal.SIGKILL)
                killed.append(pid)
            except ProcessLookupError:
                pass
    return killed


class RecoveryBench(Bench):
    """An independent Runner at one socket, in a directory it creates and removes, and the gateways
    that connect to it, all over one operations database. Without an authority (`NO_AUTHORITY`)
    the Runner registers with nothing; with one, it registers from the credential file it is
    given (its path only) and launches through the helper, and its gateways take part with
    `--devguard register`."""

    def __init__(self, args, live, evidence, writer, work, workspaces, authority, prefix, fixture=None):
        super().__init__(args, live, evidence, writer, 'independent', work, workspaces, authority, prefix=prefix,
                         fixture=fixture)
        self.socket = work / 'rv' / 'runner.sock'
        self.operations = work / 'operations.sqlite'
        self.governed = authority.get('socket') is not None
        self.runners = []
        self.began_unix = time.time()

    def runner_flags(self):
        if not self.governed:
            return []
        authority = self.authority
        return ['--devguard-socket', str(authority['socket']), '--devguard-consumer', authority['consumer'],
                '--devguard-generation', authority['generation'],
                '--devguard-credential-file', str(authority['credential_file']),
                '--devguard-launch-helper', str(authority['helper'])]

    def runner(self, name='runner'):
        index = len(self.runners) + 1
        runner = host.RunnerProcess(self.args.runtime_bin, self.socket, host.minimal_env(self.work),
                                    self.evidence.file(f'{self.prefix}/runner-{index}-{name}.log'),
                                    self.runner_flags())
        self.live.runners.append(runner)
        self.runners.append(runner)
        self.evidence.append(f'{self.prefix}/runners.jsonl', {'name': name, 'pid': runner.pid, 'argv': runner.argv,
                                                              'listening': runner.listening,
                                                              'started_unix': time.time()})
        if not runner.listening:
            raise RuntimeError(f'the independent Runner did not listen: {runner.log()[-500:]}')
        return runner

    def flags(self, intent='detach'):
        flags = ['--runner', 'uds', '--runner-socket', str(self.socket), '--runner-lifecycle', 'independent']
        if intent:
            flags += ['--runner-exit-intent', intent]
        if self.governed:
            authority = self.authority
            flags += host.devguard_flags('register', authority['socket'], authority['credential_file'], None,
                                         authority['consumer'], authority['generation'])
        return flags

    def environment(self, timeout_s=None, database=None):
        """A gateway's environment: the registry, the process timeout and the operations database
        (`database` False: none)."""
        env = {'CODESPACE_CONFIG': str(self.config),
               'CODESPACE_PROCESS_TIMEOUT_SECS': str(timeout_s or RECOVERY_TIMEOUT_S)}
        if database is not False:
            env['CODESPACE_OPERATIONS_DB'] = str(database or self.operations)
        return host.minimal_env(self.work, env)

    def gateway(self, name, intent='detach', timeout_s=None, database=None, initialize=True):
        self.count += 1
        prefix = f'{self.prefix}/{self.count:02d}-{name}'
        transcript = self.evidence.file(prefix + '.mcp.jsonl')
        gateway = host.Gateway(self.args.mcp_bin, self.flags(intent), self.environment(timeout_s, database),
                               self.evidence.file(prefix + '.gateway.log'),
                               record=lambda row: self.writer.write(transcript, row))
        self.live.gateways.append(gateway)
        self.gateways.append(gateway)
        self.evidence.append(f'{self.prefix}/gateways.jsonl', {
            'name': name, 'pid': gateway.pid, 'argv': gateway.argv, 'intent': intent,
            'timeout_s': timeout_s or RECOVERY_TIMEOUT_S, 'started_unix': time.time()})
        if initialize:
            gateway.initialize()
        return gateway

    def end(self, gateway, how, limit=60):
        ended = host.end_gateway(gateway, how, limit)
        self.evidence.append(f'{self.prefix}/gateways.jsonl', {'ended': gateway.pid, **ended})
        return ended

    def refused(self, name, flags=None, env=None, limit=90):
        """A gateway that should not start: its exit status and what it logged. Its input is empty,
        so one that does start detaches at once and exits 0."""
        argv = [str(self.args.mcp_bin), *(self.flags('detach') if flags is None else flags)]
        began = time.monotonic()
        try:
            result = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=limit,
                                    env=env or self.environment(), start_new_session=True)
            returncode, output = result.returncode, result.stderr
        except subprocess.TimeoutExpired as error:
            output = error.stderr.decode(errors='replace') if isinstance(error.stderr, bytes) else (error.stderr or '')
            returncode = None
        self.evidence.file(f'{self.prefix}/{name}.refused.log').write_text(output)
        entry = {'name': name, 'argv': argv, 'returncode': returncode, 'elapsed_s': round(time.monotonic() - began, 3)}
        self.evidence.append(f'{self.prefix}/refused.jsonl', entry)
        return dict(entry, output=output)

    def teardown(self):
        """End whatever a case left: gateways killed, Runners stopped, which ends what they hold,
        and any tagged process a killed Runner left; then the fixture authority and the work
        directory."""
        for gateway in self.gateways:
            if gateway.process.poll() is None:
                gateway.process.kill()
                gateway.process.wait()
            gateway.join_readers()
        stopped = [runner.stop(limit=20) for runner in self.runners if runner.alive()]
        killed = kill_leftovers(self.roots.values(), self.began_unix)
        self.evidence.append(f'{self.prefix}/teardown.jsonl', {'runners_stopped': stopped, 'leftovers_killed': killed})
        if self.fixture is not None:
            self.fixture.stop()
        shutil.rmtree(self.work, ignore_errors=True)


def recovery_workspaces(work, participation):
    """`a`, `b` and `c` of the bench's participation, `confirm` that holds each execution for
    approval, and `off`, which never takes part."""
    resources = host.required(**FIXTURE_REQUEST) if participation == 'required' else None
    spaces = {name: (work / f'ws/{name}', resources) for name in ('a', 'b', 'c')}
    spaces['confirm'] = (work / 'ws/confirm', resources, {'approvals': 'confirm'})
    spaces['off'] = (work / 'ws/off', None)
    return spaces


def recovery_bench(args, live, evidence, writer, participation, label, workspaces=recovery_workspaces):
    """A fresh bench for one scenario: its own work directory, operations database and Runner
    socket; with `required`, its own fixture authority with one instance per consumer (D12)."""
    work = scratch()
    prefix = f'{participation}/{label}'
    if participation == 'off':
        return RecoveryBench(args, live, evidence, writer, work, workspaces(work, participation), dict(NO_AUTHORITY),
                             prefix)
    try:
        fixture = host.FixtureAuthority(args.fixture_bin, work / 'fx', host.minimal_env(work),
                                        evidence.file(f'{prefix}/fixture.log'), limit=1)
    except Exception:
        shutil.rmtree(work, ignore_errors=True)
        raise
    live.fixtures.append(fixture)
    settings = fixture.settings
    if not settings.get('native'):
        fixture.stop()
        shutil.rmtree(work, ignore_errors=True)
        raise NotNative(NOT_NATIVE)
    authority = {'socket': settings['socket'], 'credential_file': settings['credential_file'],
                 'helper': args.fixture_helper, 'consumer': settings['consumer'],
                 'generation': settings['generation'], 'journal': fixture.journal()}
    return RecoveryBench(args, live, evidence, writer, work, workspaces(work, participation), authority, prefix,
                         fixture=fixture)


# -------------------------------- what the scenarios share

def claimed_by(runner, pid, limit=10.0):
    """The Claim the Runner logged granting control to process `pid`, once it is logged."""
    deadline = time.monotonic() + limit
    while True:
        found = [claim for claim in runner.claims() if claim['granted'] and claim['pid'] == pid]
        if found or time.monotonic() >= deadline:
            return found[-1] if found else None
        time.sleep(0.05)


def runner_log_counts(runner):
    text = runner.log()
    return {key: text.count(needle) for key, needle in RUNNER_LINES.items()}


def instance_of(bench, pid):
    """The consumer's instances in the authority's journal, and the active ones that are process `pid`."""
    instances = host.journal_instances(bench.authority['consumer'], bench.database)
    return [entry for entry in instances if entry['state'] == 'active' and entry['pid'] == pid], instances


def wait_registered(bench, runner, limit=30.0):
    deadline = time.monotonic() + limit
    while True:
        active, instances = instance_of(bench, runner.pid)
        if active or time.monotonic() >= deadline:
            return (active[0] if active else None), instances
        time.sleep(0.1)


def wait_retired(bench, pid, limit=60.0):
    deadline = time.monotonic() + limit
    while True:
        active, instances = instance_of(bench, pid)
        if not active or time.monotonic() >= deadline:
            return not active, instances
        time.sleep(0.5)


def registered_checks(bench, runner):
    """The Runner listens and, with an authority, is the consumer's one active instance."""
    checks = [check('the Runner listens', runner.listening, log=runner.log()[-500:])]
    if bench.governed:
        instance, instances = wait_registered(bench, runner)
        active = [entry for entry in instances if entry['state'] == 'active']
        checks.append(check("the Runner is the consumer's one active instance", instance is not None and len(active) == 1,
                            instances=instances))
    return checks


def stopped_checks(bench, runner, label, terminated=None):
    """The Runner stopped, by its controller's Stop or its own SIGTERM: it exited 0, said how many
    processes it terminated, removed its socket and the directory it created for it, and with an
    authority its instance retired."""
    code = runner.wait(30)
    found = re.findall(r'stopped; (\d+) processes were terminated', runner.log())
    checks = [check(f'{label}: the Runner exited 0', code == 0, returncode=code),
              check(f'{label}: it removed its socket and the directory it created',
                    not bench.socket.exists() and not bench.socket.parent.exists())]
    if terminated is not None:
        checks.append(check(f'{label}: it terminated {terminated} processes',
                            bool(found) and int(found[-1]) == terminated, logged=found))
    if bench.governed:
        retired, instances = wait_retired(bench, runner.pid)
        checks.append(check(f'{label}: its DevGuard instance retired', retired, instances=instances))
    return checks


def once_reconciled(call, limit=30.0, period=0.05):
    """`call()` again while it is refused with RECOVERY_PENDING, for up to `limit` seconds: its
    last result, the refusals before it and how long that took (ms)."""
    began = time.monotonic()
    pending = 0
    while True:
        result = call()
        if result.get('code') != 'RECOVERY_PENDING' or time.monotonic() - began >= limit:
            return result, pending, round((time.monotonic() - began) * 1000, 3)
        pending += 1
        time.sleep(period)


def resume_result(gateway, approval, timeout=host.CALL_TIMEOUT_S):
    """operation_resume of a granted approval as an outcome: started, with its process, refused
    with CodeSpace's code, or an error of the transport."""
    try:
        payload, latency = gateway.resume(approval, timeout)
    except ToolError as error:
        return {'outcome': 'error' if error.transport else 'refused', 'code': error.code,
                'message': error.message[:500], 'latency_ms': error.latency_ms}
    started = payload.get('exec_command') or {}
    return {'outcome': 'started' if started.get('process_id') else 'answered', 'process_id': started.get('process_id'),
            'dispatch': started.get('dispatch_status'), 'latency_ms': latency}


def status_of(gateway, process_id, timeout=10):
    try:
        payload, latency = gateway.status(process_id, timeout)
    except ToolError as error:
        return {'error': error.code, 'message': error.message[:500]}
    return dict(payload, latency_ms=latency)


def wait_state(gateway, process_id, accept, limit=15.0, period=0.1):
    """The process's status once `accept(status)` holds, or the last one after `limit` seconds."""
    deadline = time.monotonic() + limit
    while True:
        status = status_of(gateway, process_id)
        if accept(status) or time.monotonic() >= deadline:
            return status
        time.sleep(period)


def exited(status):
    return status.get('state') == 'exited'


def not_running(status):
    return status.get('state') not in (None, 'running') or bool(status.get('error'))


def refused_code(result, code, limit_message=None):
    return result.get('outcome') == 'refused' and result.get('code') == code


def record_of(bench, process_id):
    try:
        return host.process_records(bench.operations).get(process_id)
    except Exception as error:  # noqa: BLE001 - an unreadable record is evidence
        return {'error': f'{type(error).__name__}: {error}'}


def argv_digest(argv):
    """The digest a process record keeps of its argv: SHA-256 of the argv as a compact JSON array."""
    return 'sha256:' + hashlib.sha256(json.dumps(list(argv), separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()


class OutputReader:
    """Reads one process's output on from where it left off, through whichever gateway it is
    given, keeping each answer: the cursor it asked from, the next one, the chunk, and what was
    lost before it (analysis.continuity_metrics)."""

    def __init__(self, process_id):
        self.process_id, self.cursor, self.rows = process_id, 0, []

    def read(self, gateway, timeout=10):
        payload, latency = gateway.call('read_process', {'process_id': self.process_id, 'cursor': self.cursor}, timeout)
        row = {'from': self.cursor, 'to': payload.get('cursor', self.cursor), 'chunk': payload.get('chunk') or '',
               'eof': bool(payload.get('eof')), 'output_lost': bool(payload.get('output_lost')),
               'retained_from': payload.get('retained_from'), 'lost_reason': payload.get('lost_reason'),
               'latency_ms': latency}
        self.rows.append(row)
        self.cursor = row['to']
        return row


def continuity_checks(label, rows, name, declared=False):
    metrics = analysis.continuity_metrics(rows, name)
    checks = [check(f'{label}: the output went on without a gap or a repeat', metrics['passed'], metrics=metrics)]
    if declared:
        checks.append(check(f'{label}: the overflow was declared as ring_overflow',
                            metrics['declared_gaps'] >= 1 and metrics['gap_reasons'] == ['ring_overflow'],
                            metrics=metrics))
    return checks, metrics


def attempt_checks(bench, gateway, process_id, label):
    """With an authority: the execution's attempt as DevGuard settled it once its scope ended."""
    if not bench.governed:
        return []
    attempt = attempt_of(gateway, process_id)
    view = settled_view(bench, attempt)
    return [check(f'{label}: DevGuard released its attempt, holding nothing', released_cleanly(view),
                  attempt_id=attempt, view=view)]

# -------------------------------- recovery-lifecycle (P1R-C01)

# Each gateway's exit intent and how it ends; the last one stops the Runner.
EXIT_STEPS = (('detach', 'eof'), ('detach', 'sigterm'), ('stop', 'sigusr1'), ('detach', 'sigkill'), ('stop', 'eof'))
# The process timeout of the timeout scenario's first gateway.
TIMEOUT_CASE_S = 8


def recovery_exit_kinds(bench, tty):
    """A process runs while its gateway ends in every way a gateway ends, each time followed by
    the next gateway: its input closed or SIGTERM with the exit intent `detach`, SIGUSR1 with
    `stop` (a restart always detaches), SIGKILL, which says nothing; then a gateway with `stop`
    whose input closes stops the Runner. The Runner and the process outlive every gateway but the
    last; each gateway claims the Runner under the next epoch, finds the same process running and
    reads its output on from where the one before stopped; the last one's Stop ends the process,
    and the Runner removes its socket and exits 0 (P1R-C01, P1R-C03)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    gateway = bench.gateway('g1', intent=EXIT_STEPS[0][0])
    first = gateway
    started = gateway.exec('a', tagged(bench.roots['a'], 'beat', BEAT), tty=tty)
    checks.append(started_checks('the process', started))
    if started['outcome'] != 'started':
        return checks, {'started': started}
    process_id = started['process_id']
    pid = tag_pid(bench.roots['a'], 'beat')
    claim = claimed_by(runner, gateway.pid)
    checks.append(check('gateway 1 claimed the Runner under epoch 1', bool(claim and claim['epoch'] == 1), claim=claim))
    reader, steps = OutputReader(process_id), []
    for step, (intent, how) in enumerate(EXIT_STEPS):
        time.sleep(1.0)
        reader.read(gateway)
        label = f'gateway {step + 1} ({intent}, {how})'
        before = runner_log_counts(runner)
        ended = bench.end(gateway, how)
        steps.append({'intent': intent, 'how': how, 'ended': ended})
        checks.append(check(f'{label}: exited as a gateway ended so exits',
                            ended['returncode'] == analysis.GATEWAY_EXIT[how] and not ended['stopped'], ended=ended))
        if step == len(EXIT_STEPS) - 1:
            break
        time.sleep(0.5)
        after = runner_log_counts(runner)
        said = 'lost' if how == 'sigkill' else 'detached'
        logged = 'left without a word' if said == 'lost' else 'detached'
        checks += [check(f'{label}: the Runner logged that its controller {logged}', after[said] == before[said] + 1,
                         before=before, after=after),
                   check(f'{label}: the Runner and the process still run', runner.alive() and host.running(pid),
                         pid=pid)]
        gateway = bench.gateway(f'g{step + 2}', intent=EXIT_STEPS[step + 1][0])
        claim = claimed_by(runner, gateway.pid)
        status = status_of(gateway, process_id)
        checks += [check(f'gateway {step + 2} claimed the Runner under epoch {step + 2}, after a controller with work',
                         bool(claim and claim['epoch'] == step + 2 and claim['previous_controller']), claim=claim),
                   check(f'gateway {step + 2}: the same process runs', status.get('state') == 'running', status=status)]
        time.sleep(0.5)
        reader.read(gateway)
    checks += stopped_checks(bench, runner, 'the last gateway stopped the Runner', terminated=1)
    checks.append(check('the process ended with the Runner', gone(pid, limit=10), pid=pid))
    continuity, metrics = continuity_checks('the process', reader.rows, 'beat')
    checks += continuity
    checks += attempt_checks(bench, first, process_id, 'the process')
    return checks, {'steps': steps, 'claims': runner.claims(), 'continuity': metrics}


def deadline_of(bench, process_id, limit=5.0):
    """The deadline (Unix ms) the process's record holds, once it holds one."""
    deadline = time.monotonic() + limit
    while True:
        record = record_of(bench, process_id) or {}
        if record.get('deadline_at_ms') or time.monotonic() >= deadline:
            return record.get('deadline_at_ms')
        time.sleep(0.05)


def recovery_original_timeout(bench, tty):
    """Two processes reach their timeouts across gateway restarts: the first while no gateway
    runs, the second while a later gateway with a much longer process timeout serves. Each ends at
    the deadline its record held when it started, as the Runner timed it, never at one a gateway
    restarted or lengthened, and is reported timed out (P1R-C01, P1R-C02)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    watch = host.PidWatch()
    detail = {}
    try:
        g1 = bench.gateway('g1', timeout_s=TIMEOUT_CASE_S)
        first = g1.exec('a', tagged(bench.roots['a'], 'first', ['/bin/sleep', '600']), tty=tty)
        checks.append(started_checks('the first process', first))
        if first['outcome'] != 'started':
            return checks, {'first': first}
        watch.watch('first', tag_pid(bench.roots['a'], 'first'))
        d1 = deadline_of(bench, first['process_id'])
        time.sleep(2.0)
        detail['g1'] = bench.end(g1, 'sigkill')
        time.sleep(1.0)
        g2 = bench.gateway('g2', timeout_s=TIMEOUT_CASE_S - 1)
        second, _, _ = once_reconciled(lambda: g2.exec('b', tagged(bench.roots['b'], 'second', ['/bin/sleep', '600']),
                                                       tty=tty))
        checks.append(started_checks('the second process', second))
        if second['outcome'] != 'started' or not d1:
            return checks, dict(detail, second=second, d1=d1)
        watch.watch('second', tag_pid(bench.roots['b'], 'second'))
        d2 = deadline_of(bench, second['process_id'])
        until_unix(d1 / 1000 - 2.0)
        detail['g2'] = bench.end(g2, 'sigterm')
        until_unix(d1 / 1000 + 1.0)
        g3 = bench.gateway('g3', timeout_s=RECOVERY_TIMEOUT_S)
        rows = []
        for key, started, deadline, in_gap in (('first', first, d1, True), ('second', second, d2, False)):
            gone_unix = watch.wait(key, max(0.0, (deadline or 0) / 1000 + 5.0 - time.time()))
            status = wait_state(g3, started['process_id'], exited)
            later = (record_of(bench, started['process_id']) or {}).get('deadline_at_ms')
            rows.append({'tag': key, 'deadline_at_ms': deadline, 'deadline_after_ms': later, 'gone_unix': gone_unix,
                         'termination': status.get('termination'), 'in_gap': in_gap})
            checks.append(check(f"the {key} process timed out at its own deadline{' while no gateway ran' if in_gap else ''}",
                                analysis.timeout_metrics([rows[-1]])['passed'], row=rows[-1], status=status))
            checks += attempt_checks(bench, g1 if key == 'first' else g2, started['process_id'], f'the {key} process')
        detail['timeouts'] = analysis.timeout_metrics(rows)
        detail['rows'] = rows
    finally:
        watch.stop()
    return checks, detail


def until_unix(moment):
    time.sleep(max(0.0, moment - time.time()))


def recovery_runner_sigterm(bench):
    """The Runner's own SIGTERM, as an operator stops it, stops it as a controller's Stop does: it
    terminates its pipe and PTY processes, exits 0 and removes its socket; its gateway, which no
    longer finds it, reports neither process running and refuses new work; with an authority, both
    attempts are released and the instance retires (P1R-C01)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    gateway = bench.gateway('g1')
    started, pids = [], []
    for workspace, tty in (('a', False), ('b', True)):
        entry = gateway.exec(workspace, tagged(bench.roots[workspace], f'beat-{io_name(tty)}', BEAT), tty=tty)
        checks.append(started_checks(f'the {io_name(tty)} process', entry))
        started.append(entry)
        pids.append(tag_pid(bench.roots[workspace], f'beat-{io_name(tty)}') if entry['outcome'] == 'started' else None)
    if any(entry['outcome'] != 'started' for entry in started):
        return checks, {'started': started}
    time.sleep(1.0)
    runner.process.send_signal(signal.SIGTERM)
    checks += stopped_checks(bench, runner, 'SIGTERM', terminated=2)
    statuses = []
    for entry, pid in zip(started, pids):
        status = wait_state(gateway, entry['process_id'], not_running)
        statuses.append(status)
        checks += [check(f"{entry['process_id']}: the process ended", gone(pid, limit=10), pid=pid),
                   check(f"{entry['process_id']}: the gateway no longer reports it running", not_running(status),
                         status=status)]
        checks += attempt_checks(bench, gateway, entry['process_id'], entry['process_id'])
    later = wait_refusal(gateway, 'c', 'RUNNER_DISCONNECTED')
    checks.append(check('new work is refused with RUNNER_DISCONNECTED', refused_code(later, 'RUNNER_DISCONNECTED'),
                        result=later))
    return checks, {'statuses': statuses, 'later': later}


def wait_refusal(gateway, workspace, code, limit=20.0):
    """exec_command in `workspace` until it is refused with `code`, for up to `limit` seconds."""
    deadline = time.monotonic() + limit
    while True:
        result = gateway.exec(workspace, ['/bin/echo', 'refused'])
        if result.get('code') == code or result['outcome'] == 'started' or time.monotonic() >= deadline:
            return result
        time.sleep(0.2)


def recovery_invalid_settings(bench):
    """Settings that do not fit together are refused before anything reaches the Runner: an exit
    intent without the independent lifecycle, the lifecycle without an exit intent, in process,
    with a worker to start, without a socket or without an operations database; a managed gateway
    finds an independent Runner, and an independent gateway a managed worker. A Runner refuses a
    socket directory that is not private, a shared one, no socket, an unknown lifecycle, a handed
    credential and a socket where another Runner listens. The Runner that listens is left as it
    was: it logged no Claim, and a gateway then claims it under epoch 1 and runs a command
    (P1R-C01)."""
    runner = bench.runner()
    socket = str(bench.socket)
    env = bench.environment()
    detach = ['--runner-exit-intent', 'detach']
    independent = ['--runner-lifecycle', 'independent']
    managed_dir = bench.work / 'mw'
    managed_dir.mkdir(mode=0o700)
    managed_socket = managed_dir / 'runner.sock'
    with open(bench.evidence.file(f'{bench.prefix}/managed-worker.log'), 'ab') as log:
        worker = subprocess.Popen([str(bench.args.runtime_bin), str(managed_socket)], stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=log, env=host.minimal_env(bench.work),
                                  start_new_session=True)
    bench.live.workers.append(worker)
    # A managed worker serves the first connection it accepts and no other, so it is not probed.
    deadline = time.monotonic() + 15
    while not managed_socket.exists() and time.monotonic() < deadline and worker.poll() is None:
        time.sleep(0.05)
    gateways = [
        ('an exit intent without the independent lifecycle', ['--runner', 'uds', '--runner-socket', socket, *detach], env,
         '--runner-exit-intent is only for --runner-lifecycle independent'),
        ('the independent lifecycle without an exit intent', ['--runner', 'uds', '--runner-socket', socket, *independent],
         env, 'needs --runner-exit-intent detach or stop'),
        ('in process', ['--runner', 'in-process', *independent, *detach], env, 'needs --runner uds'),
        ('with a worker to start', ['--runner', 'uds', '--runner-socket', socket, '--runtime-bin',
                                    str(bench.args.runtime_bin), *independent, *detach], env,
         '--runtime-bin and --runner-dir are for a worker the gateway starts'),
        ('without a socket', ['--runner', 'uds', *independent, *detach], env, 'needs --runner-socket'),
        ('without an operations database', bench.flags('detach'), bench.environment(database=False),
         'needs --operations-db'),
        ('a managed gateway at the independent Runner', ['--runner', 'uds', '--runner-socket', socket], env,
         'is an independent Runner'),
        ('an independent gateway at a managed worker', ['--runner', 'uds', '--runner-socket', str(managed_socket),
                                                        *independent, *detach], env, 'not an independent Runner')]
    checks, detail = [], {}
    for index, (label, flags, environment, needle) in enumerate(gateways):
        result = bench.refused(f'gateway-{index + 1}', flags, environment, limit=60)
        detail[label] = result
        checks.append(check(f'{label}: refused', result['returncode'] not in (0, None) and needle in result['output'],
                            returncode=result['returncode'], output=result['output'][-600:]))
    if worker.poll() is None:
        worker.terminate()
        worker.wait(timeout=10)
    open_dir = bench.work / 'open'
    open_dir.mkdir(mode=0o755)
    os.chmod(open_dir, 0o755)
    lifecycle = ['--lifecycle', 'independent']
    runners = [
        ('a socket directory that is not private', [str(open_dir / 'runner.sock'), *lifecycle], 1,
         'must be a directory of this user with mode 0700'),
        ('a shared socket directory', [f'/tmp/csq-{os.getpid()}.sock', *lifecycle], 1, 'must not be /, /tmp'),
        ('no socket', lifecycle, 2, "needs the socket's path first"),
        ('an unknown lifecycle', [socket, '--lifecycle', 'forever'], 2, '--lifecycle must be managed or independent'),
        ('a handed credential', [socket, *lifecycle, '--devguard-socket', '/nonexistent/authority.sock',
                                 '--devguard-consumer', 'codespace', '--devguard-generation', 'g1',
                                 '--devguard-credential-fd', '3'], 2, '--devguard-credential-file only'),
        ('a socket where a Runner listens', [socket, *lifecycle], 1, 'already in use')]
    for label, arguments, code, needle in runners:
        try:
            result = subprocess.run([str(bench.args.runtime_bin), *arguments], stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=30, env=host.minimal_env(bench.work),
                                    start_new_session=True)
            returncode, output = result.returncode, result.stderr
        except subprocess.TimeoutExpired:
            returncode, output = None, 'did not exit within 30 s'
        detail[label] = {'returncode': returncode, 'output': output[-600:]}
        checks.append(check(f'a Runner with {label}: refused with exit status {code}',
                            returncode == code and needle in output, returncode=returncode, output=output[-600:]))
    checks += [check('the Runner logged no Claim and still runs', runner.claims() == [] and runner.alive(),
                     claims=runner.claims()),
               check('its socket directory is unchanged', stat_mode(bench.socket.parent) == 0o700,
                     mode=stat_mode(bench.socket.parent))]
    gateway = bench.gateway('g1')
    claim = claimed_by(runner, gateway.pid)
    ran_checks, _, _ = ran(gateway, gateway.exec('a', ['/bin/echo', 'fine']), 'fine\n', 'a command')
    checks.append(check('a gateway then claimed the Runner under epoch 1', bool(claim and claim['epoch'] == 1), claim=claim))
    return checks + ran_checks, detail


def stat_mode(path):
    try:
        return os.stat(path).st_mode & 0o777
    except OSError:
        return None


# -------------------------------- recovery-store (P1R-C02)

def recovery_records(bench, tty):
    """What the operations database holds of an execution sent to the independent Runner, and
    after a gateway restart: the Runner's identity as its Hello states it, the controller epoch,
    the workspace, the argv's digest and never the argv, the root process of a pipe execution (a
    PTY's is not named), the original deadline, and how it ended once a later gateway terminated
    it (P1R-C02)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    probe = host.WireClient(bench.socket)
    try:
        hello = probe.hello()
    finally:
        probe.close()
    identity = (hello.get('data') or {}).get('identity') or {}
    token = f'qualify-argv-{secrets.token_hex(8)}'
    command = tagged(bench.roots['a'], 'record', ['/bin/sh', '-c', 'sleep 600', token])
    g1 = bench.gateway('g1')
    started = g1.exec('a', command, tty=tty)
    checks.append(started_checks('the process', started))
    if started['outcome'] != 'started':
        return checks, {'started': started}
    process_id = started['process_id']
    pid = tag_pid(bench.roots['a'], 'record')
    row = record_of(bench, process_id) or {}
    stored = [name for name, value in row.items() if isinstance(value, str) and token in value]
    database = b''.join(path.read_bytes() for path in bench.work.glob('operations.sqlite*') if path.is_file())
    expected_participation = 'required' if bench.governed else 'off'
    checks += [
        check('the record is running', row.get('phase') == 'running', record=row),
        check("it names the Runner by its Hello's identity",
              row.get('runner_pid') == identity.get('pid') == runner.pid
              and row.get('runner_boot_id') == identity.get('boot_id')
              and row.get('runner_started_at_ms') == identity.get('started_at_ms')
              and row.get('runner_nonce') == identity.get('nonce'), identity=identity),
        check('under controller epoch 1, in its workspace, as sent', row.get('controller_epoch') == 1
              and row.get('workspace_id') == 'a' and bool(row.get('tty')) == tty
              and row.get('participation') == expected_participation, record=row),
        check("it holds the argv's digest", row.get('argv_digest') == argv_digest(command),
              recorded=row.get('argv_digest'), expected=argv_digest(command)),
        check('neither the record nor the database holds the argv', not stored and token.encode() not in database,
              columns=stored),
        check('the root process is the pipe process, and a PTY root is not named',
              row.get('root_pid') == (None if tty else pid), root_pid=row.get('root_pid'), pid=pid),
        check('its deadline is the process timeout from its start',
              row.get('deadline_at_ms') is not None and row.get('created_at_ms') is not None
              and abs(row['deadline_at_ms'] - row['created_at_ms'] - RECOVERY_TIMEOUT_S * 1000) <= 2000, record=row)]
    if bench.governed:
        checks.append(check('it names its DevGuard attempt', row.get('attempt_id') is not None
                            and row.get('attempt_id') == attempt_of(g1, process_id), record=row))
    bench.end(g1, 'sigkill')
    g2 = bench.gateway('g2')
    g2.terminate(process_id)
    status = wait_state(g2, process_id, exited)
    after = record_of(bench, process_id) or {}
    checks += [check('a later gateway terminated it', status.get('termination') == 'terminated', status=status),
               check('its record says it was terminated, its deadline unchanged',
                     after.get('phase') == 'terminal' and after.get('termination') == 'terminated'
                     and after.get('deadline_at_ms') == row.get('deadline_at_ms') and not after.get('lost'), record=after)]
    checks += attempt_checks(bench, g1, process_id, 'the process')
    return checks, {'record': row, 'after': after, 'hello': hello}


def recovery_writer_lock(bench):
    """One gateway at a time writes the operations database (P1R-C02): a second gateway started
    on it while the first serves waits a bounded time and refuses to start, naming the holder,
    and never reaches the Runner; one started while the first ends waits for it and then serves,
    claiming the Runner after it."""
    runner = bench.runner()
    checks = []
    g1 = bench.gateway('g1')
    second = bench.refused('second-writer')
    claims = runner.claims()
    checks += [check('a second gateway on the database refused to start, naming the holder',
                     second['returncode'] not in (0, None) and 'has another writer' in second['output']
                     and str(g1.pid) in second['output'], returncode=second['returncode'], output=second['output'][-600:]),
               check('after waiting a bounded time', 14.0 <= second['elapsed_s'] <= 45.0, elapsed_s=second['elapsed_s']),
               check('it never reached the Runner', [claim['pid'] for claim in claims] == [g1.pid], claims=claims)]
    ran_checks, _, _ = ran(g1, g1.exec('a', ['/bin/echo', 'still']), 'still\n', 'the first gateway')
    checks += ran_checks
    g3 = bench.gateway('g3', initialize=False)
    time.sleep(1.5)
    ended = bench.end(g1, 'eof')
    g3.initialize(timeout=60)
    claim = claimed_by(runner, g3.pid)
    log = g3.log_path.read_text(errors='replace')
    checks += [check('a gateway started while the first one ended waited for it, then served',
                     'waiting for it to end' in log and 'the only writer of the operations database' in log
                     and ended['returncode'] == 0, ended=ended),
               check('it claimed the Runner under epoch 2', bool(claim and claim['epoch'] == 2), claim=claim)]
    later, _, _ = ran(g3, g3.exec('a', ['/bin/echo', 'next']), 'next\n', 'the next gateway')
    return checks + later, {'second': {key: second[key] for key in ('returncode', 'elapsed_s')}}


def sqlite_write(path, statement):
    connection = sqlite3.connect(str(path), timeout=5)
    try:
        connection.execute(statement)
        connection.commit()
    finally:
        connection.close()


def recovery_schema(bench):
    """An operations database the gateway cannot use is refused before anything reaches the
    Runner, and left unchanged: one of a newer schema version, and one that is not a database
    (P1R-C02)."""
    runner = bench.runner()
    g1 = bench.gateway('g1')
    checks, _, _ = ran(g1, g1.exec('a', ['/bin/echo', 'first']), 'first\n', 'the first gateway')
    bench.end(g1, 'eof')
    newer = bench.work / 'newer.sqlite'
    shutil.copy(bench.operations, newer)
    sqlite_write(newer, 'PRAGMA user_version = 2')
    garbage = bench.work / 'garbage.sqlite'
    garbage.write_bytes(secrets.token_bytes(8192))
    before = runner.claims()
    detail = {}
    for label, path, needle in (('a newer schema', newer, 'schema version 2, newer than this'),
                                ('not a database', garbage, '')):
        digest = host.sha256(path)
        result = bench.refused(label.replace(' ', '-'), env=bench.environment(database=path), limit=60)
        detail[label] = {key: result[key] for key in ('returncode', 'elapsed_s')}
        checks += [check(f'{label}: refused', result['returncode'] not in (0, None) and needle in result['output'],
                         returncode=result['returncode'], output=result['output'][-600:]),
                   check(f'{label}: the file is unchanged', host.sha256(path) == digest)]
    checks.append(check('nothing reached the Runner', runner.claims() == before and runner.alive(), claims=runner.claims()))
    g2 = bench.gateway('g2')
    claim = claimed_by(runner, g2.pid)
    checks.append(check('the gateway of the database the Runner knows claimed it under epoch 2',
                        bool(claim and claim['epoch'] == 2), claim=claim))
    later, _, _ = ran(g2, g2.exec('a', ['/bin/echo', 'next']), 'next\n', 'it')
    return checks + later, detail


def recovery_records_command(bench):
    """`codespace-mcp records` (P1R-C05) lists the records that may still hold a workspace and
    releases a record only while no gateway runs, only for an execution that has not ended, and
    not while its Runner's PID runs a process, which may be that Runner."""
    runner = bench.runner()
    g1 = bench.gateway('g1')
    held = g1.exec('a', tagged(bench.roots['a'], 'held', ['/bin/sleep', '600']))
    done = g1.exec('b', tagged(bench.roots['b'], 'done', ['/bin/sh', '-c', 'exit 4']))
    checks = [started_checks('a running process', held), started_checks('an ending process', done)]
    if held['outcome'] != 'started' or done['outcome'] != 'started':
        return checks, {'held': held, 'done': done}
    wait_state(g1, done['process_id'], exited)
    env = host.minimal_env(bench.work)
    listed = host.records_command(bench.args.mcp_bin, bench.operations, env, 'list')
    found = {entry['process_id']: entry for entry in ((listed['report'] or {}).get('records') or [])}
    entry = found.get(held['process_id']) or {}
    while_serving = host.records_command(bench.args.mcp_bin, bench.operations, env, 'release', held['process_id'])
    bench.end(g1, 'eof')
    releases = {label: host.records_command(bench.args.mcp_bin, bench.operations, env, 'release', process_id)
                for label, process_id in (('unknown', 'no-such-process'), ('ended', done['process_id']),
                                          ('running', held['process_id']))}

    def refusal(result):
        return ' '.join(item.get('reason', '') for item in ((result['report'] or {}).get('refused') or []))
    checks += [
        check('records list shows the running one, with its Runner and root, and not the ended one',
              listed['done'] and held['process_id'] in found and done['process_id'] not in found
              and entry.get('phase') == 'running' and (entry.get('runner') or {}).get('pid') == runner.pid
              and (entry.get('root') or {}).get('running') is True, listed=listed['report']),
        check('release is refused while a gateway runs', not while_serving['done']
              and 'only while no gateway runs' in while_serving['stderr'], result=while_serving),
        check('an unknown process is refused', not releases['unknown']['done']
              and 'no record has this process ID' in refusal(releases['unknown']), result=releases['unknown']),
        check('an ended execution is refused', not releases['ended']['done']
              and 'its execution ended' in refusal(releases['ended']), result=releases['ended']),
        check("a running one is refused while its Runner's PID runs", not releases['running']['done']
              and f"Runner's PID {runner.pid} runs" in refusal(releases['running']), result=releases['running'])]
    g2 = bench.gateway('g2')
    status = status_of(g2, held['process_id'])
    checks.append(check('the next gateway still finds it running', status.get('state') == 'running', status=status))
    g2.terminate(held['process_id'])
    return checks, {'listed': listed['report'], 'releases': {key: value['report'] for key, value in releases.items()}}

# -------------------------------- recovery-fence (P1R-C03)

def fence_requests(client, targets, epochs):
    """Requests that would change running processes, under each of `epochs` (None: none): the
    Runner must refuse each with STALE_CONTROLLER and change nothing."""
    found = []
    for epoch in epochs:
        found += [client.request('write_stdin', {'request': {'process_id': targets['write'], 'data': ''}}, epoch=epoch),
                  client.request('resize', {'process_id': targets['resize'], 'rows': 30, 'cols': 90}, epoch=epoch),
                  client.request('terminate', {'process_id': targets['terminate']}, epoch=epoch),
                  client.request('terminate_workspace', {'workspace_id': targets['workspace']}, epoch=epoch),
                  client.request('stop', epoch=epoch)]
    return [{'op': entry['op'], 'epoch': epoch, 'ok': entry['ok'], 'code': entry['code'], 'type': entry['type'],
             'message': (entry['message'] or '')[:300]} for entry, epoch in zip(found, [e for e in epochs for _ in range(5)])]


def stale_checks(label, requests):
    accepted = [request for request in requests if request['ok']]
    unfenced = [request for request in requests if not request['ok'] and request['code'] != 'STALE_CONTROLLER']
    return [check(f'{label}: every request that would change a process was refused as stale, nothing changed',
                  not accepted and not unfenced and bool(requests), accepted=accepted, unfenced=unfenced)]


def two_targets(bench, gateway, tty):
    """A pipe or PTY beat in `a` and a `cat` in `b`, both of the bench's participation."""
    beat = gateway.exec('a', tagged(bench.roots['a'], 'beat', BEAT), tty=tty)
    cat = gateway.exec('b', tagged(bench.roots['b'], 'cat', ['/bin/cat']))
    return beat, cat


def recovery_foreign_connection(bench, tty):
    """While a gateway controls the Runner, another process of the same user connects to its
    socket: it reads the Runner's Hello, its Claim is refused because a controller is connected,
    and every request it sends that would change a process, under the controller's epoch or any
    other, is refused as stale; a Detach changes nothing. The gateway goes on controlling the
    Runner and its processes as before (P1R-C03)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    gateway = bench.gateway('g1')
    beat, cat = two_targets(bench, gateway, tty)
    checks += [started_checks('the beat', beat), started_checks('the cat', cat)]
    if beat['outcome'] != 'started' or cat['outcome'] != 'started':
        return checks, {'beat': beat, 'cat': cat}
    reader = OutputReader(beat['process_id'])
    time.sleep(0.6)
    reader.read(gateway)
    store = host.store_id(bench.operations)
    targets = {'write': cat['process_id'], 'resize': beat['process_id'], 'terminate': beat['process_id'],
               'workspace': 'a'}
    client = host.WireClient(bench.socket)
    try:
        hello = client.hello()
        claim = client.claim(store)
        requests = fence_requests(client, targets, (1, None, 2))
        detach = client.detach()
    finally:
        client.close()
    refusals = [entry for entry in runner.claims() if not entry['granted']]
    checks += [check("it reads the Runner's Hello", hello['ok'] and hello['type'] == 'hello', hello=hello),
               check('its Claim is refused while the controller is connected', claim['type'] == 'claim_refused'
                     and (claim['data'] or {}).get('refusal') == 'controller_active', claim=claim),
               check('the Runner logged the refused Claim with its PID', any(entry['pid'] == os.getpid()
                                                                             and entry['refusal'] == 'controller_active'
                                                                             for entry in refusals), refusals=refusals),
               check('its Detach is answered and changes nothing', detach['ok'], detach=detach)]
    checks += stale_checks('the other connection', requests)
    time.sleep(0.6)
    reader.read(gateway)
    beat_status, cat_status = status_of(gateway, beat['process_id']), status_of(gateway, cat['process_id'])
    gateway.call('write_stdin', {'process_id': cat['process_id'], 'data': 'still-here\n'})
    echoed = gateway.read_until(cat['process_id'], 'still-here')[3]
    checks += [check('both processes still run', beat_status.get('state') == 'running'
                     and cat_status.get('state') == 'running', beat=beat_status, cat=cat_status),
               check('the gateway still writes to its process', echoed),
               check('the Runner logged no other Claim', [entry['pid'] for entry in runner.claims() if entry['granted']]
                     == [gateway.pid], claims=runner.claims())]
    continuity, metrics = continuity_checks('the beat', reader.rows, 'beat')
    checks += continuity
    for started in (beat, cat):
        gateway.terminate(started['process_id'])
        wait_state(gateway, started['process_id'], exited)
        checks += attempt_checks(bench, gateway, started['process_id'], started['process_id'])
    return checks, {'requests': requests, 'continuity': metrics}


def recovery_stale_epoch(bench):
    """Controller epochs across gateways and another process: once two gateways in turn detached,
    a Claim for another operations database is refused; one for the Runner's own, which any
    process of the same user can make while no gateway controls the Runner (the exposure D6
    accepts, logged with its PID), is granted under the next epoch, and that connection's
    requests that would change a process under an earlier epoch or none are refused as stale.
    The next gateway claims the Runner under the epoch after it and finds the same process,
    its output on from where it was (P1R-C03)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    g1 = bench.gateway('g1')
    beat, cat = two_targets(bench, g1, False)
    checks += [started_checks('the beat', beat), started_checks('the cat', cat)]
    if beat['outcome'] != 'started' or cat['outcome'] != 'started':
        return checks, {'beat': beat, 'cat': cat}
    reader = OutputReader(beat['process_id'])
    time.sleep(0.6)
    reader.read(g1)
    bench.end(g1, 'eof')
    g2 = bench.gateway('g2')
    time.sleep(0.6)
    reader.read(g2)
    bench.end(g2, 'sigusr1')
    store = host.store_id(bench.operations)
    targets = {'write': cat['process_id'], 'resize': beat['process_id'], 'terminate': beat['process_id'],
               'workspace': 'a'}
    client = host.WireClient(bench.socket)
    try:
        wrong = client.claim('another-operations-database')
        claim = client.claim(store)
        requests = fence_requests(client, targets, (1, 2, None))
        detach = client.detach()
    finally:
        client.close()
    granted = [entry for entry in runner.claims() if entry['granted']]
    checks += [check('a Claim for another operations database is refused', wrong['type'] == 'claim_refused'
                     and (wrong['data'] or {}).get('refusal') == 'store_mismatch', claim=wrong),
               check("a Claim for the Runner's database while no gateway controls it is granted under epoch 3",
                     claim['type'] == 'claimed' and (claim['data'] or {}).get('epoch') == 3, claim=claim),
               check('the Runner logged that Claim with the PID that made it',
                     bool(granted) and granted[-1]['pid'] == os.getpid() and granted[-1]['epoch'] == 3, claims=granted),
               check('its Detach is answered', detach['ok'] and detach['type'] == 'detached', detach=detach)]
    checks += stale_checks('requests under epochs 1, 2 and none', requests)
    g3 = bench.gateway('g3')
    claim3 = claimed_by(runner, g3.pid)
    status = status_of(g3, beat['process_id'])
    time.sleep(0.6)
    reader.read(g3)
    checks += [check('the next gateway claimed the Runner under epoch 4', bool(claim3 and claim3['epoch'] == 4),
                     claim=claim3),
               check('the same process runs', status.get('state') == 'running', status=status)]
    continuity, metrics = continuity_checks('the beat', reader.rows, 'beat')
    checks += continuity
    for started in (beat, cat):
        g3.terminate(started['process_id'])
        wait_state(g3, started['process_id'], exited)
    return checks, {'requests': requests, 'claims': runner.claims(), 'continuity': metrics}


def recovery_stopped_gateway(bench):
    """A controlling gateway stopped with SIGSTOP keeps its connection open, so nothing pushes it
    out: another process's Claim is refused, and a second gateway waits for the operations
    database's writer and refuses to start. Once the stopped gateway is killed, the next gateway
    claims the Runner under epoch 2 and finds the process running (P1R-C02, P1R-C03)."""
    runner = bench.runner()
    g1 = bench.gateway('g1')
    beat = g1.exec('a', tagged(bench.roots['a'], 'beat', BEAT))
    checks = [started_checks('the beat', beat)]
    if beat['outcome'] != 'started':
        return checks, {'beat': beat}
    reader = OutputReader(beat['process_id'])
    time.sleep(0.6)
    reader.read(g1)
    os.kill(g1.pid, signal.SIGSTOP)
    try:
        client = host.WireClient(bench.socket)
        try:
            claim = client.claim(host.store_id(bench.operations))
        finally:
            client.close()
        second = bench.refused('second-gateway')
    finally:
        os.kill(g1.pid, signal.SIGCONT)
    bench.end(g1, 'sigkill')
    g3 = bench.gateway('g3')
    claim3 = claimed_by(runner, g3.pid)
    status = status_of(g3, beat['process_id'])
    time.sleep(0.6)
    reader.read(g3)
    checks += [check("another process's Claim is refused while the stopped gateway's connection is open",
                     claim['type'] == 'claim_refused' and (claim['data'] or {}).get('refusal') == 'controller_active',
                     claim=claim),
               check('a second gateway refused to start, waiting for the writer', second['returncode'] not in (0, None)
                     and 'has another writer' in second['output'], returncode=second['returncode'],
                     output=second['output'][-600:]),
               check('once the stopped gateway was killed, the next one claimed the Runner under epoch 2',
                     bool(claim3 and claim3['epoch'] == 2), claim=claim3),
               check('and found the process running', status.get('state') == 'running', status=status)]
    continuity, metrics = continuity_checks('the beat', reader.rows, 'beat')
    g3.terminate(beat['process_id'])
    return checks + continuity, {'continuity': metrics}


CONCURRENT_CLAIMS = 8


def recovery_concurrent_claims(bench):
    """While no gateway controls the Runner, eight connections claim it at once for its own
    operations database: exactly one is granted, the others are refused because a controller is
    connected; once it detaches, the next gateway claims the Runner under the epoch after it and
    finds the process (P1R-C03)."""
    runner = bench.runner()
    g1 = bench.gateway('g1')
    beat = g1.exec('a', tagged(bench.roots['a'], 'beat', BEAT))
    checks = [started_checks('the beat', beat)]
    if beat['outcome'] != 'started':
        return checks, {'beat': beat}
    bench.end(g1, 'eof')
    store = host.store_id(bench.operations)
    clients = [host.WireClient(bench.socket) for _ in range(CONCURRENT_CLAIMS)]
    gate = threading.Barrier(CONCURRENT_CLAIMS)
    answers = [None] * CONCURRENT_CLAIMS

    def claim(index):
        gate.wait(timeout=10)
        try:
            answers[index] = clients[index].claim(store)
        except (OSError, ValueError) as error:
            answers[index] = {'type': 'error', 'message': str(error)}
    threads = [threading.Thread(target=claim, args=(index,), daemon=True) for index in range(CONCURRENT_CLAIMS)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=30)
    winners = [index for index, answer in enumerate(answers) if (answer or {}).get('type') == 'claimed']
    refused = [answer for answer in answers if (answer or {}).get('type') == 'claim_refused']
    detached = clients[winners[0]].detach() if len(winners) == 1 else None
    for client in clients:
        client.close()
    g2 = bench.gateway('g2')
    claim2 = claimed_by(runner, g2.pid)
    status = status_of(g2, beat['process_id'])
    checks += [check('exactly one Claim was granted, under epoch 2', len(winners) == 1
                     and (answers[winners[0]]['data'] or {}).get('epoch') == 2, answers=answers),
               check('the others were refused because a controller was connected', len(refused) == CONCURRENT_CLAIMS - 1
                     and all((answer['data'] or {}).get('refusal') == 'controller_active' for answer in refused),
                     refused=refused),
               check('the winner detached', bool(detached and detached['ok']), detach=detached),
               check('the next gateway claimed the Runner under epoch 3', bool(claim2 and claim2['epoch'] == 3), claim=claim2),
               check('and found the process running', status.get('state') == 'running', status=status)]
    g2.terminate(beat['process_id'])
    return checks, {'answers': answers}


def recovery_other_database(bench):
    """A gateway whose operations database is not the one the Runner was first claimed for is
    refused and starts nothing, whether or not a gateway controls the Runner: two gateways with
    two databases never both write to one Runner (P1R-C03)."""
    runner = bench.runner()
    g1 = bench.gateway('g1')
    beat = g1.exec('a', tagged(bench.roots['a'], 'beat', BEAT))
    checks = [started_checks('the beat', beat)]
    if beat['outcome'] != 'started':
        return checks, {'beat': beat}
    other = bench.work / 'other.sqlite'
    during = bench.refused('other-database-while-controlled', env=bench.environment(database=other))
    bench.end(g1, 'eof')
    after = bench.refused('other-database-after', env=bench.environment(database=other))
    g2 = bench.gateway('g2')
    claim2 = claimed_by(runner, g2.pid)
    status = status_of(g2, beat['process_id'])
    for label, result in (('while a gateway controls the Runner', during), ('while none does', after)):
        checks.append(check(f'a gateway with another database is refused {label}',
                            result['returncode'] not in (0, None) and 'store_mismatch' in result['output'],
                            returncode=result['returncode'], output=result['output'][-600:]))
    checks += [check("the Runner's own gateway then claimed it under epoch 2", bool(claim2 and claim2['epoch'] == 2),
                     claim=claim2),
               check('and found the process running', status.get('state') == 'running', status=status),
               check('the other database never controlled the Runner',
                     [entry['pid'] for entry in runner.claims() if entry['granted']] == [g1.pid, g2.pid],
                     claims=runner.claims())]
    g2.terminate(beat['process_id'])
    return checks, {'during': during['returncode'], 'after': after['returncode']}


def recovery_concurrent_gateways(bench):
    """Two gateways started at once on the Runner's operations database: one serves and claims the
    Runner; the other waits for the writer and refuses to start, never reaching the Runner
    (P1R-C02, P1R-C03)."""
    runner = bench.runner()
    g1 = bench.gateway('g1')
    beat = g1.exec('a', tagged(bench.roots['a'], 'beat', BEAT))
    checks = [started_checks('the beat', beat)]
    if beat['outcome'] != 'started':
        return checks, {'beat': beat}
    bench.end(g1, 'eof')
    pair = [bench.gateway(f'pair-{index}', initialize=False) for index in (1, 2)]
    outcomes = [None, None]

    def start(index):
        try:
            pair[index].initialize(timeout=60)
            outcomes[index] = 'serving'
        except ToolError as error:
            outcomes[index] = error.code
    threads = [threading.Thread(target=start, args=(index,), daemon=True) for index in (0, 1)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=90)
    serving = [gateway for gateway, outcome in zip(pair, outcomes) if outcome == 'serving']
    losers = [gateway for gateway, outcome in zip(pair, outcomes) if outcome != 'serving']
    for gateway in losers:
        gateway.process.wait(timeout=60)
    granted = [entry['pid'] for entry in runner.claims() if entry['granted']]
    checks += [check('exactly one of them serves', len(serving) == 1, outcomes=outcomes),
               check('the other refused to start, waiting for the writer',
                     len(losers) == 1 and losers[0].process.returncode not in (0, None)
                     and 'has another writer' in losers[0].log_path.read_text(errors='replace'),
                     returncode=[gateway.process.returncode for gateway in losers]),
               check('only the serving one claimed the Runner, under epoch 2',
                     len(serving) == 1 and granted == [g1.pid, serving[0].pid], claims=runner.claims())]
    if serving:
        status = status_of(serving[0], beat['process_id'])
        checks.append(check('it found the process running', status.get('state') == 'running', status=status))
        serving[0].terminate(beat['process_id'])
    return checks, {'outcomes': outcomes}


# -------------------------------- recovery-reconcile (P1R-C04)

def recovery_reconcile_running(bench, tty):
    """A gateway killed while its process runs: the next gateway refuses new work in that
    workspace with RECOVERY_PENDING until it has reconciled it, then holds the workspace for the
    process again (WORKSPACE_BUSY), while a workspace where no work was left takes new work at
    once; terminating the inherited process frees its workspace. No refused command ran
    (P1R-C04)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    g1 = bench.gateway('g1')
    held = g1.exec('a', tagged(bench.roots['a'], 'held', ['/bin/sleep', '600']), tty=tty)
    checks.append(started_checks('the process', held))
    if held['outcome'] != 'started':
        return checks, {'held': held}
    bench.end(g1, 'sigkill')
    g2 = bench.gateway('g2')
    elsewhere = g2.exec('b', tagged(bench.roots['b'], 'elsewhere', ['/bin/echo', 'elsewhere']), tty=tty)
    busy, pending, waited = once_reconciled(lambda: g2.exec('a', tagged(bench.roots['a'], 'refused', ['/bin/echo', 'no']),
                                                            tty=tty))
    status = status_of(g2, held['process_id'])
    g2.terminate(held['process_id'])
    ended = wait_state(g2, held['process_id'], exited)
    fresh, _, _ = once_reconciled(lambda: g2.exec('a', tagged(bench.roots['a'], 'fresh', ['/bin/echo', 'fresh']), tty=tty))
    fresh_checks, _, _ = ran(g2, fresh, 'fresh\n', 'new work once it was free')
    checks += [check('a workspace where no work was left took new work at once', elsewhere['outcome'] == 'started',
                     result=elsewhere),
               check('new work where the process runs was held for it again (WORKSPACE_BUSY), after reconciling',
                     refused_code(busy, 'WORKSPACE_BUSY'), result=busy, pending_refusals=pending, waited_ms=waited),
               check('the inherited process runs', status.get('state') == 'running', status=status),
               check('terminating it ended it', ended.get('termination') == 'terminated', status=ended),
               check('no refused command ran', 'refused' not in markers(bench.roots['a'])
                     and markers(bench.roots['a']).count('held') == 1, markers=markers(bench.roots['a']))]
    checks += fresh_checks
    checks += attempt_checks(bench, g1, held['process_id'], 'the inherited process')
    return checks, {'pending_refusals': pending, 'waited_ms': waited}


def recovery_reconcile_terminal(bench, tty):
    """A process that ends while no gateway runs: the next gateway learns how it ended from the
    Runner, which kept its end for the controller, records it and frees the workspace (P1R-C04)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    g1 = bench.gateway('g1')
    started = g1.exec('a', tagged(bench.roots['a'], 'ends', ['/bin/sh', '-c', 'sleep 1.5; exit 7']), tty=tty)
    checks.append(started_checks('the process', started))
    if started['outcome'] != 'started':
        return checks, {'started': started}
    bench.end(g1, 'sigkill')
    time.sleep(3.0)
    g2 = bench.gateway('g2')
    status = wait_state(g2, started['process_id'], exited)
    fresh, pending, waited = once_reconciled(lambda: g2.exec('a', tagged(bench.roots['a'], 'fresh', ['/bin/echo', 'fresh']),
                                                             tty=tty))
    fresh_checks, _, _ = ran(g2, fresh, 'fresh\n', 'new work in its workspace')
    record = record_of(bench, started['process_id']) or {}
    checks += [check('the next gateway reports how it ended', status.get('termination') == 'exited'
                     and status.get('exit_code') == 7, status=status),
               check('its record says so', record.get('phase') == 'terminal' and record.get('exit_code') == 7,
                     record=record)]
    checks += fresh_checks
    checks += attempt_checks(bench, g1, started['process_id'], 'the process')
    return checks, {'pending_refusals': pending, 'waited_ms': waited}


KILL_DELAYS_S = (0.0, 0.002, 0.005, 0.01, 0.02, 0.04, 0.08, 0.16)


def recovery_reconcile_approval(bench):
    """Approvals across gateway restarts (P1R-C04): one held and granted before a gateway was
    killed is resumed by the next gateway once, and resuming it again returns the same process;
    one resumed while the gateway was killed, at a range of moments, is resumed by the next
    gateway with the process the Runner created for it, or started then when the Runner proved
    it created none: each command ran exactly once."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    root = bench.roots['confirm']
    gateway = bench.gateway('g1')
    rounds = []
    for index, delay in enumerate((None,) + KILL_DELAYS_S):
        tag = f'approval-{index}'
        held = gateway.exec('confirm', tagged(root, tag, ['/bin/sh', '-c', 'exit 0']))
        approval = held.get('approval_id')
        row = {'tag': tag, 'held': held.get('code'), 'approval_id': approval, 'delay_s': delay}
        rounds.append(row)
        if not approval:
            continue
        gateway.call('approval_resolve', {'approval_id': approval, 'decision': 'grant'})
        if delay is not None:
            flight = {}
            thread = threading.Thread(target=lambda: flight.update(resume_result(gateway, approval)), daemon=True)
            thread.start()
            time.sleep(delay)
        bench.end(gateway, 'sigkill')
        if delay is not None:
            thread.join(timeout=10)
            row['in_flight'] = flight
        gateway = bench.gateway(f'g{index + 2}')
        first, pending, waited = once_reconciled(lambda: resume_result(gateway, approval))
        again = resume_result(gateway, approval)
        if first.get('process_id'):
            wait_state(gateway, first['process_id'], exited)
        row.update(resume=first, again=again, pending_refusals=pending, ready_ms=waited,
                   runs=markers(root).count(tag))
    for row in rounds:
        label = f"approval {row['tag']}"
        if row['delay_s'] is not None:
            label += f" (killed {row['delay_s'] * 1000:.0f} ms into a resume)"
        checks += [check(f'{label}: held for approval', row['held'] == 'APPROVAL_REQUIRED' and row['approval_id'], row=row),
                   check(f'{label}: the next gateway resumed it', bool((row.get('resume') or {}).get('process_id')),
                         row=row),
                   check(f'{label}: resuming again returned the same process',
                         (row.get('again') or {}).get('process_id') == (row.get('resume') or {}).get('process_id'),
                         row=row),
                   check(f'{label}: its command ran exactly once', row.get('runs') == 1, row=row)]
    return checks, {'rounds': rounds}


def recovery_authority_outage(bench):
    """The authority holds its lock while a gateway that was killed is replaced: the inherited
    governed processes answer at once, and their workspaces stay refused with RECOVERY_PENDING
    while the Runner cannot ask the authority about their attempts; ungoverned work runs. Once
    the authority answers, the gateway reconciles them, holds each workspace for its process
    again, and terminating them releases their attempts (P1R-C04)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    g1 = bench.gateway('g1')
    held = [g1.exec('a', tagged(bench.roots['a'], 'pipe', ['/bin/sleep', '600'])),
            g1.exec('b', tagged(bench.roots['b'], 'pty', ['/bin/sleep', '600']), tty=True)]
    for started in held:
        checks.append(started_checks(started.get('process_id') or 'a governed process', started))
    if any(started['outcome'] != 'started' for started in held):
        return checks, {'held': held}
    bench.end(g1, 'sigkill')
    bench.fixture.command('hold 6000')
    hold = time.monotonic()
    g2 = bench.gateway('g2')
    statuses = [status_of(g2, started['process_id']) for started in held]
    during = g2.exec('a', tagged(bench.roots['a'], 'during', ['/bin/echo', 'no']))
    off = g2.exec('off', ['/bin/echo', 'off'])
    off_checks, _, _ = ran(g2, off, 'off\n', 'ungoverned work during the hold')
    during_s = time.monotonic() - hold
    until(hold + 7.0)
    busy, pending, waited = once_reconciled(lambda: g2.exec('a', tagged(bench.roots['a'], 'after', ['/bin/echo', 'no'])),
                                            limit=60)
    checks += [check('the inherited processes answered during the hold', all(status.get('state') == 'running'
                                                                            and status.get('latency_ms', 1e9) < 1000
                                                                            for status in statuses), statuses=statuses),
               check('new work in their workspaces was refused with RECOVERY_PENDING during the hold',
                     refused_code(during, 'RECOVERY_PENDING') and during_s < 6.0, result=during,
                     during_s=round(during_s, 3)),
               check('once the authority answered, the gateway reconciled and held the workspace for its process',
                     refused_code(busy, 'WORKSPACE_BUSY'), result=busy, pending_refusals=pending, waited_ms=waited),
               check('no refused command ran', 'during' not in markers(bench.roots['a'])
                     and 'after' not in markers(bench.roots['a']), markers=markers(bench.roots['a']))]
    checks += off_checks
    for started in held:
        g2.terminate(started['process_id'])
        wait_state(g2, started['process_id'], exited)
        checks += attempt_checks(bench, g1, started['process_id'], started['process_id'])
    return checks, {'statuses': statuses, 'during': during, 'busy': busy}


def recovery_missing_record(bench):
    """A record removed from the operations database while no gateway ran, as a damaged database
    could lose one: the Runner still holds the process, so the next gateway can query it, and
    keeps its workspace refused with RECOVERY_PENDING for an operator rather than start anything
    there; other workspaces take new work (P1R-C04)."""
    bench.runner()
    g1 = bench.gateway('g1')
    held = g1.exec('a', tagged(bench.roots['a'], 'held', ['/bin/sleep', '600']))
    checks = [started_checks('the process', held)]
    if held['outcome'] != 'started':
        return checks, {'held': held}
    bench.end(g1, 'eof')
    sqlite_write(bench.operations, f"DELETE FROM processes WHERE process_id = '{held['process_id']}'")
    g2 = bench.gateway('g2')
    time.sleep(3.0)
    blocked = g2.exec('a', tagged(bench.roots['a'], 'refused', ['/bin/echo', 'no']))
    elsewhere = g2.exec('b', ['/bin/echo', 'elsewhere'])
    elsewhere_checks, _, _ = ran(g2, elsewhere, 'elsewhere\n', 'other work')
    status = status_of(g2, held['process_id'])
    checks += [check('its workspace stays refused with RECOVERY_PENDING for an operator',
                     refused_code(blocked, 'RECOVERY_PENDING') and 'operator' in (blocked.get('message') or ''),
                     result=blocked),
               check('the process the Runner holds can be queried', status.get('state') == 'running', status=status),
               check('no refused command ran', 'refused' not in markers(bench.roots['a']), markers=markers(bench.roots['a']))]
    checks += elsewhere_checks
    g2.terminate(held['process_id'])
    after = wait_state(g2, held['process_id'], exited)
    return checks, {'blocked': blocked, 'terminated': after}


def recovery_kill_during_exec(bench, tty):
    """Gateways killed while exec_command is in flight, at a range of moments: the next gateway
    reconciles what each left, and no command ran more than once: nothing is replayed (P1R-C04)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    gateway = bench.gateway('g1')
    rounds = []
    for index, delay in enumerate(KILL_DELAYS_S):
        tag = f'exec-{index}'
        flight = {}
        thread = threading.Thread(target=lambda: flight.update(gateway.exec('a', tagged(bench.roots['a'], tag,
                                                                                        ['/bin/sleep', '0.3']), tty=tty)),
                                  daemon=True)
        thread.start()
        time.sleep(delay)
        bench.end(gateway, 'sigkill')
        thread.join(timeout=10)
        gateway = bench.gateway(f'g{index + 2}')
        probe_tag = f'probe-{index}'
        probe, pending, waited = once_reconciled(lambda: gateway.exec('a', tagged(bench.roots['a'], probe_tag,
                                                                                 ['/bin/echo', 'probe']), tty=tty))
        deadline = time.monotonic() + 10
        while refused_code(probe, 'WORKSPACE_BUSY') and time.monotonic() < deadline:
            time.sleep(0.2)
            probe_tag = f'probe-{index}-{int(time.monotonic() * 1000)}'
            probe, _, _ = once_reconciled(lambda: gateway.exec('a', tagged(bench.roots['a'], probe_tag,
                                                                          ['/bin/echo', 'probe']), tty=tty))
        if probe['outcome'] == 'started':
            wait_state(gateway, probe['process_id'], exited)
        rounds.append({'tag': tag, 'delay_s': delay, 'in_flight': flight, 'probe': probe.get('outcome'),
                       'probe_code': probe.get('code'), 'pending_refusals': pending, 'ready_ms': waited})
    tags = markers(bench.roots['a'])
    runs = {row['tag']: tags.count(row['tag']) for row in rounds}
    checks += [check('no command ran twice', all(count <= 1 for count in runs.values())
                     and all(tags.count(tag) == 1 for tag in tags), runs=runs, markers=tags),
               check('after each kill, the next gateway reconciled and took new work',
                     all(row['probe'] == 'started' for row in rounds), rounds=rounds)]
    if bench.governed:
        found, charged = host.settle_attempts(gateway_attempts(bench), bench.database, 30, bench.authority['consumer'])
        checks.append(check('nothing of it stays charged', not charged, charged=charged))
    return checks, {'rounds': rounds, 'runs': runs}


def recovery_kill_during_terminate(bench, tty):
    """Gateways killed while terminate_process is in flight, at a range of moments: the next
    gateway reports the process terminated, or still running and then terminates it; each process
    ends once, terminated, and ran once; its workspace takes new work afterwards (P1R-C04)."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    gateway = bench.gateway('g1')
    rounds = []
    for index, delay in enumerate(KILL_DELAYS_S):
        tag = f'held-{index}'
        started, _, _ = once_reconciled(lambda: gateway.exec('a', tagged(bench.roots['a'], tag, ['/bin/sleep', '600']),
                                                             tty=tty))
        if started['outcome'] != 'started':
            rounds.append({'tag': tag, 'delay_s': delay, 'started': started})
            break
        pid = tag_pid(bench.roots['a'], tag)
        flight = {}

        def terminate(process_id=started['process_id'], target=gateway, result=flight):
            try:
                result['ack_ms'] = target.terminate(process_id, timeout=10)
            except ToolError as error:
                result['error'] = error.code
        thread = threading.Thread(target=terminate, daemon=True)
        thread.start()
        time.sleep(delay)
        bench.end(gateway, 'sigkill')
        thread.join(timeout=10)
        gateway = bench.gateway(f'g{index + 2}')
        status = wait_state(gateway, started['process_id'], not_running, limit=3)
        again = None
        if status.get('state') == 'running':
            again = gateway.terminate(started['process_id'])
            status = wait_state(gateway, started['process_id'], exited)
        rounds.append({'tag': tag, 'delay_s': delay, 'in_flight': flight, 'terminated_again': again is not None,
                       'state': status.get('state'), 'termination': status.get('termination'),
                       'pid_gone': gone(pid, limit=10) if pid else None})
    tags = markers(bench.roots['a'])
    checks += [check('each process ended once, terminated, whichever gateway terminated it',
                     len(rounds) == len(KILL_DELAYS_S) and all(row.get('termination') == 'terminated' and row.get('pid_gone')
                                                               for row in rounds), rounds=rounds),
               check('each process ran once', all(tags.count(row['tag']) == 1 for row in rounds), markers=tags)]
    fresh, _, _ = once_reconciled(lambda: gateway.exec('a', ['/bin/echo', 'fresh'], tty=tty))
    fresh_checks, _, _ = ran(gateway, fresh, 'fresh\n', 'new work in its workspace')
    return checks + fresh_checks, {'rounds': rounds}


def gateway_attempts(bench):
    return sorted({attempt for gateway in bench.gateways for attempt in gateway.attempts()})


# -------------------------------- recovery-observation (P1R-C05)

def recovery_ring_overflow(bench, tty):
    """A process writes more than the Runner's 256 KiB output ring while no gateway runs: the next
    gateway reads on from where the last read stopped, says that output was lost before what it
    returns and why (`ring_overflow`), and returns the newest output whole; the status says so
    too (P1R-C05)."""
    bench.runner()
    cue = bench.work / 'cue'
    g1 = bench.gateway('g1')
    started = g1.exec('off', tagged(bench.roots['off'], 'burst', burst(cue)), tty=tty)
    checks = [started_checks('the burst', started)]
    if started['outcome'] != 'started':
        return checks, {'started': started}
    reader = OutputReader(started['process_id'])
    cue.touch()
    time.sleep(1.5)
    first = reader.read(g1)
    bench.end(g1, 'sigterm')
    cue.touch()
    time.sleep(1.5)
    g2 = bench.gateway('g2')
    second = reader.read(g2)
    status = status_of(g2, started['process_id'])
    continuity, metrics = continuity_checks('the burst', reader.rows, 'burst', declared=True)
    checks += [check('the first gateway read the end of its first burst', f'burst {BURST_LINES:08d}' in first['chunk']),
               check('the next gateway says output was lost before what it returns, because the ring overflowed',
                     second['output_lost'] and second['lost_reason'] == 'ring_overflow'
                     and (second['retained_from'] or 0) > second['from'],
                     row={key: second[key] for key in ('from', 'to', 'output_lost', 'retained_from', 'lost_reason')}),
               check('and returns the newest output whole', f'burst {2 * BURST_LINES:08d}' in second['chunk']),
               check('the status says the same', status.get('lost_reason') == 'ring_overflow'
                     and (status.get('output_retained_from') or 0) > 0, status=status)]
    checks += continuity
    g2.terminate(started['process_id'])
    return checks, {'continuity': metrics}


RETENTION_COUNT = 140


def recovery_retention(bench):
    """More ended processes than the Runner keeps (64, then 64 released): once it no longer keeps
    the oldest, process_status and read_process answer from the records, how it ended with no
    output retained and `retention_expired`, also through the next gateway (P1R-C05)."""
    bench.runner()
    g1 = bench.gateway('g1')
    ids = []
    for index in range(RETENTION_COUNT):
        started = g1.exec('off', ['/bin/sh', '-c', f'echo {index}; exit {index % 7}'])
        if started['outcome'] != 'started':
            return [started_checks(f'process {index}', started)], {}
        ids.append(started['process_id'])
        wait_state(g1, started['process_id'], exited, limit=10, period=0.02)
    oldest = ids[0]
    status = status_of(g1, oldest)
    read, _ = g1.call('read_process', {'process_id': oldest, 'cursor': 0})
    bench.end(g1, 'eof')
    g2 = bench.gateway('g2')
    again = status_of(g2, oldest)
    newest = status_of(g2, ids[-1])
    checks = [check('the oldest is answered from its record: how it ended, and why its output is gone',
                    status.get('state') == 'exited' and status.get('exit_code') == 0
                    and status.get('lost_reason') == 'retention_expired', status=status),
              check('reading it returns no output, declared lost for retention',
                    read.get('chunk') == '' and read.get('output_lost') is True
                    and read.get('lost_reason') == 'retention_expired', read=read),
              check('the next gateway answers the same', again.get('lost_reason') == 'retention_expired'
                    and again.get('exit_code') == 0, status=again),
              check('the newest is still kept', newest.get('state') == 'exited' and newest.get('lost_reason') is None
                    and newest.get('exit_code') == (RETENTION_COUNT - 1) % 7, status=newest)]
    return checks, {}


def recovery_runner_loss(bench, tty):
    """The Runner killed with SIGKILL (P1R-C05, D8): its gateway answers from the records, the
    running process as `unknown` and the ended one as it ended, both with `runner_lost`, and
    refuses new work. A pipe process that does not write outlives its killed Runner, as nothing
    ends it (one that writes meets a closed pipe); the operator finds it with `records list`,
    cannot release its record while a gateway runs or while it runs, ends it, and releases the
    record (with an authority, once DevGuard released its attempt and retired the lost Runner's
    instance). A PTY's root is not named, so `release` cannot tell whether it runs: the operator
    ends it first. A new Runner and gateway then take new work in that workspace."""
    runner = bench.runner()
    checks = registered_checks(bench, runner)
    g1 = bench.gateway('g1')
    held = g1.exec('a', tagged(bench.roots['a'], 'held', ['/bin/sleep', '600']), tty=tty)
    done = g1.exec('b', tagged(bench.roots['b'], 'done', ['/bin/sh', '-c', 'exit 5']))
    checks += [started_checks('the running process', held), started_checks('the ending process', done)]
    if held['outcome'] != 'started' or done['outcome'] != 'started':
        return checks, {'held': held, 'done': done}
    wait_state(g1, done['process_id'], exited)
    pid = tag_pid(bench.roots['a'], 'held')
    attempt = attempt_of(g1, held['process_id'])
    runner.kill()
    status = wait_state(g1, held['process_id'], lambda s: s.get('state') == 'unknown', limit=30)
    ended = status_of(g1, done['process_id'])
    read, _ = g1.call('read_process', {'process_id': held['process_id'], 'cursor': 0})
    refused = wait_refusal(g1, 'c', 'RUNNER_DISCONNECTED')
    alive = host.running(pid)
    checks += [check('the running process is unknown, lost with its Runner', status.get('state') == 'unknown'
                     and status.get('lost_reason') == 'runner_lost', status=status),
               check('the ended one is reported as it ended, with runner_lost', ended.get('state') == 'exited'
                     and ended.get('exit_code') == 5 and ended.get('lost_reason') == 'runner_lost', status=ended),
               check('reading the running one says its output was lost with the Runner',
                     read.get('output_lost') is True and read.get('lost_reason') == 'runner_lost', read=read),
               check('new work is refused with RUNNER_DISCONNECTED', refused_code(refused, 'RUNNER_DISCONNECTED'),
                     result=refused)]
    if not tty:
        checks.append(check('the pipe process outlived its killed Runner', alive, pid=pid))
    env = host.minimal_env(bench.work)
    listed = host.records_command(bench.args.mcp_bin, bench.operations, env, 'list')
    serving = host.records_command(bench.args.mcp_bin, bench.operations, env, 'release', held['process_id'])
    bench.end(g1, 'eof')
    running = None
    if alive and not tty:
        running = host.records_command(bench.args.mcp_bin, bench.operations, env, 'release', held['process_id'])
    if host.pid_alive(pid):
        os.kill(pid, signal.SIGKILL)
    checks += [check('records list shows it, its Runner gone', held['process_id'] in
                     [entry['process_id'] for entry in ((listed['report'] or {}).get('records') or [])],
                     report=listed['report']),
               check('release is refused while a gateway runs', not serving['done'], result=serving)]
    if running is not None:
        checks.append(check('release is refused while its root process runs', not running['done'] and
                            'still runs' in json.dumps(running['report']), result=running))
    checks.append(check('the process ended once the operator ended it', gone(pid, limit=10), pid=pid))
    if bench.governed:
        view = settled_view(bench, attempt, limit=60)
        retired, instances = wait_retired(bench, runner.pid)
        checks += [check("DevGuard released the lost process's attempt once its scope ended",
                         bool(view and not view['charged']), view=view),
                   check("DevGuard retired the lost Runner's instance", retired, instances=instances)]
    released = host.records_command(bench.args.mcp_bin, bench.operations, env, 'release', held['process_id'])
    checks.append(check('then the record is released', released['done'] and held['process_id'] in
                        ((released['report'] or {}).get('released') or []), result=released))
    fresh_runner = bench.runner('replacement')
    checks += registered_checks(bench, fresh_runner)
    g2 = bench.gateway('g2')
    fresh, _, _ = once_reconciled(lambda: g2.exec('a', tagged(bench.roots['a'], 'fresh', ['/bin/echo', 'fresh']), tty=tty))
    fresh_checks, _, _ = ran(g2, fresh, 'fresh\n', 'new work in its workspace')
    return checks + fresh_checks, {'status': status, 'ended': ended, 'pipe_outlived': alive, 'listed': listed['report']}


def recovery_runner_replaced(bench):
    """Another Runner answers at the socket after the first was killed: restoring a Runner's
    processes after it restarts is not supported, so the gateway does not take the new Runner
    for the old one: it reports the old Runner's process `unknown` with `runner_lost` and refuses
    new work; the next gateway keeps that workspace refused until an operator releases the
    record, and takes new work elsewhere (P1R-C05)."""
    runner = bench.runner()
    g1 = bench.gateway('g1')
    held = g1.exec('a', tagged(bench.roots['a'], 'held', ['/bin/sleep', '600']))
    checks = [started_checks('the process', held)]
    if held['outcome'] != 'started':
        return checks, {'held': held}
    pid = tag_pid(bench.roots['a'], 'held')
    runner.kill()
    second = bench.runner('replacement')
    status = wait_state(g1, held['process_id'], lambda s: s.get('state') == 'unknown', limit=30)
    refused = wait_refusal(g1, 'b', 'RUNNER_DISCONNECTED')
    bench.end(g1, 'eof')
    if host.pid_alive(pid):
        os.kill(pid, signal.SIGKILL)
    g2 = bench.gateway('g2')
    time.sleep(2.0)
    blocked = g2.exec('a', tagged(bench.roots['a'], 'refused', ['/bin/echo', 'no']))
    elsewhere = g2.exec('b', ['/bin/echo', 'elsewhere'])
    elsewhere_checks, _, _ = ran(g2, elsewhere, 'elsewhere\n', 'work elsewhere')
    checks += [check('the gateway reports the process unknown, lost with its Runner', status.get('state') == 'unknown'
                     and status.get('lost_reason') == 'runner_lost', status=status),
               check('and refuses new work', refused_code(refused, 'RUNNER_DISCONNECTED'), result=refused),
               check("the next gateway keeps the lost process's workspace refused for an operator",
                     refused_code(blocked, 'RECOVERY_PENDING') and 'records' in (blocked.get('message') or ''),
                     result=blocked),
               check('no refused command ran', 'refused' not in markers(bench.roots['a']),
                     markers=markers(bench.roots['a']))]
    return checks + elsewhere_checks, {'status': status, 'blocked': blocked, 'second_runner': second.pid}


# What each unit's suite runs: (name, IO kinds or None, participations, function).
RECOVERY_CASES = {
    'recovery-lifecycle': [('exit-kinds', IO, PARTICIPATIONS, recovery_exit_kinds),
                           ('original-timeout', IO, PARTICIPATIONS, recovery_original_timeout),
                           ('runner-sigterm', None, PARTICIPATIONS, recovery_runner_sigterm),
                           ('invalid-settings', None, ('off',), recovery_invalid_settings)],
    'recovery-store': [('records', IO, PARTICIPATIONS, recovery_records),
                       ('writer-lock', None, ('off',), recovery_writer_lock),
                       ('schema', None, ('off',), recovery_schema),
                       ('records-command', None, ('off',), recovery_records_command)],
    'recovery-fence': [('foreign-connection', IO, PARTICIPATIONS, recovery_foreign_connection),
                       ('stale-epoch', None, ('off',), recovery_stale_epoch),
                       ('stopped-gateway', None, ('off',), recovery_stopped_gateway),
                       ('concurrent-claims', None, ('off',), recovery_concurrent_claims),
                       ('other-database', None, ('off',), recovery_other_database),
                       ('concurrent-gateways', None, ('off',), recovery_concurrent_gateways)],
    'recovery-reconcile': [('running', IO, PARTICIPATIONS, recovery_reconcile_running),
                           ('terminal', IO, PARTICIPATIONS, recovery_reconcile_terminal),
                           ('approval', None, PARTICIPATIONS, recovery_reconcile_approval),
                           ('authority-outage', None, ('required',), recovery_authority_outage),
                           ('missing-record', None, ('off',), recovery_missing_record),
                           ('kill-during-exec', IO, PARTICIPATIONS, recovery_kill_during_exec),
                           ('kill-during-terminate', IO, PARTICIPATIONS, recovery_kill_during_terminate)],
    'recovery-observation': [('ring-overflow', IO, ('off',), recovery_ring_overflow),
                             ('retention', None, ('off',), recovery_retention),
                             ('runner-loss', IO, PARTICIPATIONS, recovery_runner_loss),
                             ('runner-replaced', None, ('off',), recovery_runner_replaced)]}
# Gaps the functional suites state instead of passing over: why each cannot run here.
RECOVERY_GAPS = {
    'recovery-observation': [
        ('host-reboot', 'a host restart cannot be made during a run; CodeSpace tests it with a boot ID fixture '
                        '(crates/server/src/mcp.rs), so a host restart stays unqualified on the Mac'),
        ('runner-restart-restore', "restoring a Runner's processes after the Runner itself restarts is out of P1-RECOVERY's "
                                   'scope and unsupported: runner-replaced shows the gateway refusing to take a new Runner '
                                   'for the old one')]}


def run_recovery_case(results, args, live, evidence, writer, suite, participation, io, name, function):
    """One scenario on its own bench, torn down however it ends; `required` off macOS is not run."""
    label = f'{suite}/{name}' if io is None else f'{suite}/{name}-{io}'
    if participation == 'required' and platform.system() != 'Darwin':
        return run_scenario(results, evidence, 'independent', io, name, None, suite=suite, participation=participation,
                            not_run=NOT_NATIVE)
    holder = {}

    def case():
        bench = holder['bench'] = recovery_bench(args, live, evidence, writer, participation, label)
        return function(bench) if io is None else function(bench, io == 'pty')
    try:
        return run_scenario(results, evidence, 'independent', io, name, case, suite=suite, participation=participation)
    finally:
        if 'bench' in holder:
            holder['bench'].teardown()


def recovery_functional(args, live, evidence, suites):
    """The scenarios of `suites` for each chosen participation, then the gaps they state."""
    writer = host.Writer()
    results = []
    try:
        for suite in suites:
            for name, ios, participations, function in RECOVERY_CASES[suite]:
                for participation in args.participation:
                    if participation not in participations:
                        continue
                    for tty in (ios or (None,)):
                        run_recovery_case(results, args, live, evidence, writer, suite, participation,
                                          None if tty is None else io_name(tty), name, function)
            for name, reason in RECOVERY_GAPS.get(suite, []):
                run_scenario(results, evidence, 'independent', None, name, None, suite=suite, not_run=reason)
    finally:
        evidence.write_json('functional-writer.json', writer.close())
    return results


def scenario_summary(results):
    return [{key: entry.get(key) for key in ('suite', 'scenario', 'participation', 'io', 'verdict')}
            for entry in results]


def recovery_unit(suite):
    """The command of one unit's suite: its functional scenarios alone."""
    def run(args, live, evidence):
        results = recovery_functional(args, live, evidence, [suite])
        return {'verdict': analysis.suite_verdict([entry['verdict'] for entry in results]),
                'scenarios': scenario_summary(results)}
    return run

# -------------------------------- recovery: the measurement (P1R-C06)
# The gateway restarted again and again while governed pipe and PTY processes run for a whole
# interval, idle and under DG-1's development load, against the installed service (D7): a fresh
# independent Runner per interval, registered as the codespace consumer's one instance (D12) and
# drained with its controller's Stop at the interval's end. Each restart ends the serving gateway
# in one of the four ways a gateway ends and starts the next, which must claim the Runner under
# the next epoch and find every long-running process running. Through whichever gateway serves,
# the control path is sampled as the control suite samples it; a slot that falls in a restart is
# recorded `restarting`, not as a sample, and the samples expected follow the time a gateway
# served. Every restart also has another process probe the epoch fence, a rival gateway start on
# the same operations database, an approval granted before it resumed after it (with a resume in
# flight when the gateway is killed), and a timeout target reach its deadline while no gateway
# runs or while one serves; now and then a burst overflows a pipe process's output ring while no
# gateway reads it. Without the installed service (Linux, or `off` only) the same run is a
# self-test of the harness, never a qualification.

RECOVERY_PROTOCOL = {'idle_s': 600, 'load_s': 1800, 'repetitions': 3, 'combinations': ['independent']}
RECOVERY_REHEARSAL = {'idle_s': 120, 'load_s': 180, 'repetitions': 1, 'combinations': ['independent']}
RECOVERY_PERIODS = dict(PERIODS, read=2.0)
# Every gateway after the first starts its executions with this process timeout, so that each
# restart's timeout target reaches its deadline around the restart; the first starts the
# processes that run the whole interval with the control suite's.
CYCLE_TIMEOUT_S = 20
FIRST_CYCLE_S = 20
# A restart cycle's length as planned, from which follow the restarts an interval should hold.
PLANNED_CYCLE_S = 25
# A timeout target whose deadline falls in the restart: its gateway ends this long before the
# deadline and the next one starts this long after that. Otherwise the gateway ends this long
# after the deadline and the next one starts at once.
IN_GAP_LEAD_S = 2.0
GAP_S = 4.0
CONNECTED_LAG_S = 3.0
# The approval resumed after a restart is held and granted this long before it.
APPROVAL_LEAD_S = 2.0
# A timeout target's end is awaited this long past its deadline.
TIMEOUT_WAIT_S = 10.0
# Refusals a tagged command meets while capacity, a reconciliation or the workspace's last
# process stands in its way: it is tried again under a new tag.
RETRIED = ('RESOURCE_UNAVAILABLE', 'RECOVERY_PENDING', 'WORKSPACE_BUSY')
# The workspace and terminal of each restart's timeout target, in turn.
TIMEOUT_KINDS = (('timeout-a', False), ('timeout-off', False), ('timeout-a', True), ('timeout-off', True))
RECOVERY_SAMPLES = analysis.SAMPLE_FILES + ('read_beat', 'read_burst')
RECOVERY_RECORDS = ('restarts', 'timeouts', 'approvals', 'execs')


def measurement_workspaces(work, governed, capacity=None):
    """`status-pipe`, `status-pty`, `term`, `timeout-a` and `confirm` (which holds every execution
    for approval) take part with `required` when the run is governed; `hog` asks for more than the
    whole work capacity; `beat-pty`, `burst-pipe` and `timeout-off` never take part."""
    resources = host.required(**CONTROL_REQUEST) if governed else None
    spaces = {name: (work / f'ws/{name}', resources) for name in ('status-pipe', 'status-pty', 'term', 'timeout-a')}
    spaces['confirm'] = (work / 'ws/confirm', resources, {'approvals': 'confirm'})
    spaces.update({name: (work / f'ws/{name}', None) for name in ('beat-pty', 'burst-pipe', 'timeout-off')})
    if governed:
        hog = dict(CONTROL_REQUEST, cpu_milli=((capacity or {}).get('cpu_milli') or 1_000_000) + 1)
        spaces['hog'] = (work / 'ws/hog', host.required(**hog))
    return spaces


class Serving:
    """The gateway that serves now. A sampled action enters while one serves; a restart closes the
    gate and waits for the actions in flight to leave before it ends the gateway, and opens it
    for the next one. The time the gate stood open is the time the interval was served."""

    def __init__(self):
        self.condition = threading.Condition()
        self.gateway = None
        self.inflight = 0
        self.opened = None
        self.served_s = 0.0

    def serve(self, gateway):
        with self.condition:
            self.gateway = gateway
            self.opened = time.monotonic()

    def enter(self):
        with self.condition:
            if self.gateway is None:
                return None
            self.inflight += 1
            return self.gateway

    def leave(self):
        with self.condition:
            self.inflight -= 1
            self.condition.notify_all()

    def close(self, limit=30.0):
        """No action enters any more; the actions still in flight after `limit` seconds."""
        with self.condition:
            self.gateway = None
            if self.opened is not None:
                self.served_s += time.monotonic() - self.opened
                self.opened = None
            deadline = time.monotonic() + limit
            while self.inflight and time.monotonic() < deadline:
                self.condition.wait(max(0.0, deadline - time.monotonic()))
            return self.inflight


def gated(serving, action):
    """`action(gateway, slot)` through the gateway that serves; a slot while none does is
    `restarting`."""
    def run(slot):
        gateway = serving.enter()
        if gateway is None:
            return {'outcome': 'restarting'}
        try:
            return dict(action(gateway, slot), gateway=gateway.pid)
        finally:
            serving.leave()
    return run


def status_sample(process_id):
    def sample(gateway, slot):
        payload, latency = gateway.status(process_id, timeout=10)
        return {'outcome': 'answered', 'latency_ms': latency, 'state': payload.get('state')}
    return sample


class GatedEcho:
    """Echo through whichever gateway serves: the cursor is the Runner's, so each gateway reads on
    from where the one before stopped."""

    def __init__(self, process_id):
        self.process_id, self.cursor = process_id, 0

    def __call__(self, gateway, slot):
        line = f'io-{slot}\n'
        began = time.monotonic()
        gateway.call('write_stdin', {'process_id': self.process_id, 'data': line}, timeout=10)
        _, self.cursor, eof, found = gateway.read_until(self.process_id, line, limit=10, period=0.01,
                                                        cursor=self.cursor, timeout=10)
        return {'outcome': 'echoed' if found else 'not_echoed',
                'latency_ms': round((time.monotonic() - began) * 1000, 3), 'eof': eof}


def read_sample(reader):
    """A read of a long-running process's output on from the last one, kept whole for its
    continuity (analysis.continuity_metrics)."""
    def sample(gateway, slot):
        return dict(reader.read(gateway, timeout=10), outcome='read')
    return sample


class Execs:
    """Every tagged command an interval sent, its tag and outcome kept as raw evidence
    (`execs.jsonl`) against the workspaces' marker files (analysis.marker_metrics)."""

    def __init__(self, bench, writer, path):
        self.bench, self.writer, self.path = bench, writer, path

    def run(self, gateway, workspace, tag, command, tty=False, retry=0.0, approval=False):
        """exec_command of a tagged command. Refused for capacity, for a reconciliation or for its
        workspace's last process, it is sent again under a new tag for up to `retry` seconds; the
        result names the tag that was sent last."""
        deadline = time.monotonic() + retry
        for attempt in itertools.count():
            name = tag if attempt == 0 else f'{tag}-{attempt}'
            result = gateway.exec(workspace, tagged(self.bench.roots[workspace], name, command), tty=tty)
            self.writer.write(self.path, {'tag': name, 'workspace': workspace, 'outcome': result['outcome'],
                                          'code': result.get('code'), 'process_id': result.get('process_id'),
                                          'approval': approval, 'gateway': gateway.pid, 'unix': time.time()})
            if result.get('code') not in RETRIED or time.monotonic() >= deadline:
                return dict(result, tag=name)
            time.sleep(0.2)


class GatedTerminations:
    """Each slot starts a fresh target in `term` through the gateway that serves, alternating pipe
    and PTY, terminates it and times it as Terminations does: a governed target's end by its
    scope's process group, an ungoverned one's by its own PID, its release by the watcher."""

    def __init__(self, bench, execs, watcher):
        self.bench, self.execs, self.watcher = bench, execs, watcher

    def __call__(self, gateway, slot):
        tty = bool(slot % 2)
        started = self.execs.run(gateway, 'term', f'term-{slot}', ['/bin/sleep', '600'], tty=tty)
        if started['outcome'] != 'started':
            code = started.get('code')
            return {'outcome': 'not_started' if code == 'RESOURCE_UNAVAILABLE' else 'error', 'code': code,
                    'attempt_id': started.get('attempt_id'), 'latency_ms': started.get('latency_ms'), 'tty': tty}
        process_id = started['process_id']
        entry = (gateway.execution(process_id) or {}) if self.bench.governed else {}
        root, attempt = entry.get('scope_root_pid'), entry.get('attempt_id')
        pid = tag_pid(self.bench.roots['term'], started['tag'], limit=5)
        asked = time.monotonic()
        ack = gateway.terminate(process_id, timeout=10)
        if attempt and self.watcher is not None:
            self.watcher.watch(slot, attempt, asked)
        status = gateway.wait_exited(process_id, limit=30, period=0.02)
        exited = round((time.monotonic() - asked) * 1000, 3) if status else None
        ended = host.wait_group_gone(root, limit=30) is not None if root else bool(pid) and gone(pid, limit=30)
        return {'outcome': 'acknowledged', 'tty': tty, 'ack_ms': ack, 'termination': (status or {}).get('termination'),
                'exited_ms': exited, 'scope_exit_ms': round((time.monotonic() - asked) * 1000, 3) if ended else None,
                'attempt_id': attempt, 'scope_root_pid': root, 'pid': pid}


def gated_admission(execs):
    """A governed execution in `hog`, which the authority must refuse for capacity; a command it
    refused must never run (its tag never in the marker file)."""
    def sample(gateway, slot):
        result = execs.run(gateway, 'hog', f'hog-{slot}', ['/usr/bin/true'])
        row = {'latency_ms': result.get('latency_ms'), 'code': result.get('code'), 'attempt_id': result.get('attempt_id')}
        if result['outcome'] == 'started':
            row.update(outcome='admitted', attempt_id=attempt_of(gateway, result['process_id']))
            gateway.wait_exited(result['process_id'], limit=30)
        else:
            row['outcome'] = 'refused' if result['outcome'] == 'refused' else 'error'
        return row
    return sample


def runner_footprint(runner, serving):
    def sample():
        gateway = serving.gateway
        return {'owner': host.process_footprint(runner.pid),
                'gateway': host.process_footprint(gateway.pid) if gateway is not None else None}
    return sample


class Rival:
    """A second gateway started on the serving gateway's operations database: it must wait for the
    writer and give up, never reaching the Runner. One still waiting when its cycle ends is
    killed before the serving gateway ends, so that it never takes the writer's place."""

    def __init__(self, bench, name):
        self.log_path = bench.evidence.file(f'{bench.prefix}/{name}.gateway.log')
        argv = [str(bench.args.mcp_bin), *bench.flags('detach')]
        with open(self.log_path, 'ab') as log:
            self.process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=log,
                                            env=bench.environment(CYCLE_TIMEOUT_S), start_new_session=True)
        bench.live.workers.append(self.process)
        self.pid = self.process.pid

    def finish(self, runner, limit=0.0):
        killed = False
        try:
            self.process.wait(timeout=max(0.0, limit))
        except subprocess.TimeoutExpired:
            killed = True
            self.process.kill()
            self.process.wait()
        try:
            self.process.stdin.close()
        except OSError:
            pass
        text = self.log_path.read_text(errors='replace')
        return {'pid': self.pid, 'returncode': self.process.returncode, 'killed': killed,
                'another_writer': 'has another writer' in text,
                'claims': [claim for claim in runner.claims() if claim['pid'] == self.pid]}


class Gateways:
    """The attempts and launches all of an interval's gateways logged, as one gateway's."""

    def __init__(self, gateways):
        self.gateways = gateways

    def attempts(self):
        return sorted({attempt for gateway in self.gateways for attempt in gateway.attempts()})

    def launches(self):
        found = {}
        for gateway in self.gateways:
            for process_id, count in gateway.launches().items():
                found[process_id] = found.get(process_id, 0) + count
        return found


def recovery_journal(database, gateways, rows, launched_before):
    """journal_findings over every gateway of an interval. A launch the Runner made for a gateway
    killed before it logged it is in the journal and in no log, so the difference is reported,
    not counted as a duplicate: the marker files judge whether a command ran twice."""
    merged = Gateways(gateways)
    found = journal_findings(database, merged, rows, launched_before)
    if 'error' not in found:
        found['duplicate_launches'] = sum(1 for count in merged.launches().values() if count > 1)
        found['unlogged_launches'] = max(0, (found['launched_in_journal'] or 0) - found['launched_in_log'])
    return found


class RecoveryInterval:
    """One interval of the measurement; see the section's comment."""

    def __init__(self, context, kind, seconds, key):
        self.context, self.kind, self.seconds, self.key = context, kind, seconds, key
        self.args, self.live, self.evidence = context['args'], context['live'], context['evidence']
        self.governed = context['governed']
        self.prefix = f'{key}/{kind}'
        self.raw = self.evidence.file(self.prefix + '/x').parent
        self.record = {'kind': kind, 'combination': context['combination'], 'governed': self.governed, 'invalid': [],
                       'planned_restarts': max(1, int(seconds / PLANNED_CYCLE_S))}
        self.invalid = self.record['invalid']
        self.writer = host.Writer()
        self.serving = Serving()
        self.watch = host.PidWatch()
        self.bench = self.runner = self.gateway = self.execs = self.cue = None
        self.serving_ms = None
        self.targets, self.readers = {}, {}
        self.pending_timeouts, self.pending_approval = [], None
        self.number = 0
        self.store_id = None

    # ---------------------------------------------------------- the interval

    def run(self):
        record, invalid, args = self.record, self.invalid, self.args
        launched_before = self.journal_before()
        directory = scratch()
        capacity = host.work_capacity(args.service_log) if self.governed else None
        authority = installed_authority(args) if self.governed else dict(NO_AUTHORITY)
        self.bench = RecoveryBench(args, self.live, self.evidence, self.writer, directory,
                                   measurement_workspaces(directory, self.governed, capacity), authority, self.prefix)
        self.execs = Execs(self.bench, self.writer, self.raw / 'execs.jsonl')
        self.cue = self.bench.work / 'cue'
        schedules, samplers, load, watcher, rows, sampled = {}, {}, [], None, {}, {}
        try:
            self.runner = self.bench.runner()
            if self.governed:
                record['instance'], _ = wait_registered(self.bench, self.runner)
                if record['instance'] is None:
                    invalid.append("the Runner did not register as the consumer's instance")
            if not invalid:
                self.start_gateway(CONTROL_TIMEOUT_S)
                self.start_targets()
            if not invalid:
                watcher = ReleaseWatcher(self.bench, self.writer, self.raw / 'releases.jsonl') if self.governed else None
                if watcher is not None:
                    watcher.start()
                begin = time.monotonic() + 0.5
                schedules = self.schedules(watcher, begin)
                samplers = self.samplers()
                if self.kind == 'load' and self.context.get('measure') is not None:
                    load = self.start_load(begin)
                until(begin)
                self.serving.serve(self.gateway)
                record['started_unix'] = time.time()
                for thread in list(schedules.values()) + list(samplers.values()):
                    thread.start()
                for consumer in load:
                    consumer.start()
                self.end_at = begin + self.seconds
                self.cycles()
                until(self.end_at)
                if load:
                    self.join_load(load, begin)
                record['ended_unix'] = time.time()
                record['seconds'] = round(time.monotonic() - begin, 3)
                rows = {name: schedules[name].stop() if name in schedules else [] for name in RECOVERY_SAMPLES
                        if name != 'releases'}
                sampled = {name: sampler.stop() for name, sampler in samplers.items()}
                self.serving.close()
                record['served_s'] = round(self.serving.served_s, 3)
                self.conclude_targets()
                rows['releases'] = watcher.finish(limit=60) if watcher is not None else []
                self.end_last()
                record['drain'] = self.drain()
        except BaseException:
            for thread in list(schedules.values()) + list(samplers.values()) + ([watcher] if watcher else []):
                thread.halt.set()
            if load:
                self.context['measure'].settle_consumers(load)
            self.watch.stop()
            self.writer.close(limit=10)
            self.bench.teardown()
            raise
        self.watch.stop()
        self.evidence.write_json(self.prefix + '/markers.json',
                                 {name: markers(root) for name, root in self.bench.roots.items()})
        self.bench.teardown()
        record.setdefault('served_s', round(self.serving.served_s, 3))
        if self.governed and 'started_unix' in record:
            record['journal'] = recovery_journal(args.journal, self.bench.gateways, rows, launched_before)
            if 'error' in record['journal']:
                invalid.append(f"the journal could not be read: {record['journal']['error']}")
        runs = [run for consumer in load for run in consumer.runs]
        if load:
            self.load_record(load, runs)
        written = self.writer.close()
        record['evidence_writer'] = written
        if written['error_count']:
            invalid.append(f"raw evidence could not be written: {written['errors'][:3]}")
        sampled = {name: read_rows(self.raw, name) for name in ('host', 'service', 'footprint')}
        release, service_pid = self.context.get('release'), self.context.get('service_pid')
        if self.governed and 'started_unix' in record:
            service_invalid, _ = analysis.service_findings(sampled.get('service') or [], release.id, service_pid)
            invalid += service_invalid
        record['raw'] = raw_hashes(self.raw)
        if 'started_unix' in record:
            records = {name: read_rows(self.raw, name) for name in RECOVERY_RECORDS}
            found = read_json(self.raw / 'markers.json')
            metrics = recovery_interval_metrics(record, {name: read_rows(self.raw, name) for name in RECOVERY_SAMPLES},
                                                sampled, records, found, runs, release.id if release else None,
                                                service_pid, self.context.get('reservation'))
            record['verdict'], record['reasons'] = analysis.recovery_interval_verdict(self.kind, invalid, metrics)
            record['metrics'] = metrics
        else:
            record['verdict'], record['reasons'] = 'inconclusive', list(invalid) or ['the interval did not begin']
        self.evidence.write_json(self.prefix + '/interval.json', record)
        print(f"qualify: {self.key} {self.kind}: {record['verdict']} {record['reasons']}", flush=True)
        return record

    def journal_before(self):
        if not self.governed:
            return None
        try:
            before = host.journal_charged(self.args.journal)
            launched = launched_count(self.args.journal)
            active = [entry for entry in host.journal_instances(host.CONSUMER, self.args.journal)
                      if entry['state'] == 'active']
        except Exception as error:  # noqa: BLE001 - an unreadable journal invalidates the interval
            self.record['journal_before'] = {'error': str(error)}
            self.invalid.append(f'the journal could not be read: {error}')
            return None
        self.record['journal_before'] = dict(before, active_instances=active)
        if sum(before['attempts'].values()) or before['leases']:
            self.invalid.append("work other than the harness's was charged when the interval began")
        if active:
            self.invalid.append('a codespace instance was active when the interval began')
        return launched

    def start_gateway(self, timeout_s):
        """The next gateway, serving once initialized; the time that took."""
        self.number += 1
        began = time.monotonic()
        self.gateway = self.bench.gateway(f'g{self.number:03d}', timeout_s=timeout_s)
        self.serving_ms = round((time.monotonic() - began) * 1000, 3)
        return self.gateway

    def start_targets(self):
        """The processes that run the whole interval, started by the first gateway: a `cat` (status
        and I/O) and a PTY `sleep` (status), governed when the run is, and an ungoverned PTY beat
        and pipe burst, whose output is read across every restart."""
        commands = {'status-pipe': (['/bin/cat'], False), 'status-pty': (['/bin/sleep', '86400'], True),
                    'beat-pty': (BEAT, True), 'burst-pipe': (burst(self.cue), False)}
        for name, (command, tty) in commands.items():
            started = self.execs.run(self.gateway, name, name, command, tty=tty, retry=30.0)
            if started['outcome'] != 'started':
                self.invalid.append(f"{name} could not be started: {started.get('code')}")
                continue
            process_id = started['process_id']
            governed = self.bench.requests.get(name) is not None
            self.targets[name] = {
                'process_id': process_id, 'tag': started['tag'], 'tty': tty, 'governed': governed,
                'pid': tag_pid(self.bench.roots[name], started['tag']),
                'scope_root_pid': (self.gateway.execution(process_id) or {}).get('scope_root_pid') if governed else None}
        self.record['long_running'] = self.targets
        self.readers = {name: OutputReader(self.targets[name]['process_id']) for name in ('beat-pty', 'burst-pipe')
                        if name in self.targets}

    def schedules(self, watcher, begin):
        writer, raw, serving, targets = self.writer, self.raw, self.serving, self.targets
        periods = RECOVERY_PERIODS
        found = {
            'status_pipe': Schedule(writer, raw / 'status_pipe.jsonl',
                                    gated(serving, status_sample(targets['status-pipe']['process_id'])), begin,
                                    period=periods['status']),
            'status_pty': Schedule(writer, raw / 'status_pty.jsonl',
                                   gated(serving, status_sample(targets['status-pty']['process_id'])), begin,
                                   period=periods['status']),
            'io': Schedule(writer, raw / 'io.jsonl', gated(serving, GatedEcho(targets['status-pipe']['process_id'])),
                           begin + 0.25, period=periods['io']),
            'terminations': Schedule(writer, raw / 'terminations.jsonl',
                                     gated(serving, GatedTerminations(self.bench, self.execs, watcher)),
                                     begin + periods['termination'] / 2, period=periods['termination']),
            'read_beat': Schedule(writer, raw / 'read_beat.jsonl', gated(serving, read_sample(self.readers['beat-pty'])),
                                  begin + 0.4, period=periods['read']),
            'read_burst': Schedule(writer, raw / 'read_burst.jsonl',
                                   gated(serving, read_sample(self.readers['burst-pipe'])), begin + 1.4,
                                   period=periods['read'])}
        if self.governed:
            found['refusals'] = Schedule(writer, raw / 'refusals.jsonl', gated(serving, gated_admission(self.execs)),
                                         begin + periods['refusal'] / 3, period=periods['refusal'])
        return found

    def samplers(self):
        found = {'host': host.Sampler(self.writer, self.raw / 'host.jsonl', HOST_PERIOD_S, host.host_sample),
                 'footprint': host.Sampler(self.writer, self.raw / 'footprint.jsonl', FOOTPRINT_PERIOD_S,
                                           runner_footprint(self.runner, self.serving))}
        if self.governed:
            found['service'] = host.Sampler(self.writer, self.raw / 'service.jsonl', SERVICE_PERIOD_S,
                                            self.context['release'].state)
        return found

    def start_load(self, begin):
        measure, args = self.context['measure'], self.args
        (self.raw / 'runs').mkdir()
        return measure.consumers(SimpleNamespace(qualify_bin=Path(args.qualify_bin)), self.context['dg_release'],
                                 args.load, self.context['work'], self.raw / 'runs', begin + self.seconds, self.key,
                                 args.rehearsal)

    def join_load(self, load, begin):
        measure = self.context['measure']
        limit = begin + self.seconds + measure.LOAD_COMPLETION_LIMIT_S
        for consumer in load:
            consumer.join(timeout=max(0.0, limit - time.monotonic()))
        if any(consumer.is_alive() for consumer in load):
            self.invalid.append('the load did not complete within the limit')
            measure.settle_consumers(load)

    def load_record(self, load, runs):
        measure, record = self.context['measure'], self.record
        (self.raw / 'runs.json').write_text(json.dumps(runs, indent=1) + '\n')
        record['consumers'] = [consumer.name for consumer in load]
        errors = {consumer.name: consumer.errors for consumer in load if consumer.errors}
        record['load_issues'] = ['a consumer stopped with an error'] if errors else []
        record['consumer_errors'] = errors
        ids = {attempt for run in runs for attempt in run.get('attempt_ids') or []}
        record['load_journal'] = measure.settle_journal(ids, runs)
        record['load_failures'] = (['load attempts still charged after the interval']
                                   if record['load_journal'].get('still_charged') else [])

    # ---------------------------------------------------------- restarts

    def cycles(self):
        """Restart after restart while a whole cycle still fits in the interval; the last gateway
        serves to its end."""
        while True:
            restart = self.end_at - time.monotonic() > PLANNED_CYCLE_S + 5
            if not self.cycle(restart) or not restart:
                return

    def cycle(self, restart):
        """The serving gateway's cycle: its Claim and what it inherited, the fence probe and a
        rival, the approval granted before the restart resumed, the timeout target that ended
        around the restart concluded; then, when it restarts, the next timeout target started,
        the next approval held and granted, the gateway ended as this cycle's number says and
        the next one started after the gap. Whether a gateway serves afterwards."""
        number, gateway = self.number, self.gateway
        row = {'cycle': number, 'gateway': gateway.pid, 'expected_epoch': number, 'serving_ms': self.serving_ms,
               'restart': restart}
        rival = None
        end_at = time.monotonic() + PLANNED_CYCLE_S
        try:
            row['claim'] = claimed_by(self.runner, gateway.pid)
            if number > 1:
                row['inherited'] = {name: status_of(gateway, target['process_id']).get('state')
                                    for name, target in self.targets.items()}
            row['fence'] = self.fence(number)
            rival = Rival(self.bench, f'{number:03d}-rival')
            if self.pending_approval is not None:
                self.resume_approval(gateway)
            self.conclude_timeouts(gateway)
            if not restart:
                until(self.end_at)
                return True
            if number == 1:
                end_at, gap, target = time.monotonic() + FIRST_CYCLE_S, 0.0, None
                row['timeout'] = {'alignment': None}
            else:
                end_at, gap, target = self.timeout_target(gateway, number, row)
            until(end_at - APPROVAL_LEAD_S)
            approval = self.hold_approval(gateway, number)
            until(end_at)
            how = host.GATEWAY_ENDS[number % len(host.GATEWAY_ENDS)]
            row['how'] = how
            row['inflight_at_close'] = self.serving.close()
            row['rival'] = rival.finish(self.runner)
            rival = None
            if row['timeout'].get('alignment') == 'in_gap' and (number // 8) % 4 == number % 4:
                self.cue.touch()
                row['burst'] = True
            flight = self.resume_in_flight(gateway, approval, number) if how == 'sigkill' and approval else None
            row['ended'] = self.bench.end(gateway, how)
            if target is not None:
                target['gateway_ended_unix'] = time.time()
            if flight is not None:
                thread, result, delay = flight
                thread.join(timeout=30)
                self.pending_approval.update(killed_during_resume=True, kill_delay_s=delay, in_flight=dict(result))
            time.sleep(gap)
            serves = self.next_gateway(row)
            if serves and target is not None:
                target['next_served_unix'] = time.time()
            return serves
        except Exception as error:  # noqa: BLE001 - a cycle that breaks is evidence, and the run goes on
            row['error'] = f'{type(error).__name__}: {error}'
            row['trace'] = traceback.format_exc()[-2000:]
            until(min(end_at, self.end_at))
            if self.gateway is gateway and gateway.process.poll() is None:
                self.serving.serve(gateway)
                return True
            return self.serving.gateway is not None or self.next_gateway(row)
        finally:
            if rival is not None:
                row['rival'] = rival.finish(self.runner, limit=30)
            self.writer.write(self.raw / 'restarts.jsonl', row)

    def next_gateway(self, row):
        """The next gateway, serving; tried twice. Whether one serves."""
        for attempt in range(2):
            try:
                self.start_gateway(CYCLE_TIMEOUT_S)
                self.serving.serve(self.gateway)
                return True
            except Exception as error:  # noqa: BLE001 - recorded with the restart
                row.setdefault('start_errors', []).append(f'{type(error).__name__}: {error}')
                row['error'] = row.get('error') or 'the next gateway did not start'
                time.sleep(2.0)
        return False

    def fence(self, epoch):
        """Another process of the same user at the Runner's socket while the gateway controls it:
        its Claim, and its requests that would change a long-running process or the Runner under
        the previous, the current and no epoch."""
        targets = {'write': self.targets['status-pipe']['process_id'],
                   'resize': self.targets['status-pty']['process_id'],
                   'terminate': self.targets['beat-pty']['process_id'], 'workspace': 'beat-pty'}
        try:
            client = host.WireClient(self.bench.socket, timeout=10)
            try:
                claim = client.claim(self.store())
                requests = fence_requests(client, targets, (epoch - 1, epoch, None))
            finally:
                client.close()
        except (OSError, ValueError) as error:
            return {'claim': None, 'requests': [], 'error': f'{type(error).__name__}: {error}'}
        refusal = (claim.get('data') or {}).get('refusal') if claim.get('type') == 'claim_refused' else claim.get('type')
        return {'claim': refusal, 'requests': requests}

    def store(self):
        if self.store_id is None:
            self.store_id = host.store_id(self.bench.operations)
        return self.store_id

    def timeout_target(self, gateway, number, row):
        """This restart's timeout target, when the gateway ends and the gap before the next: the
        gateway ends before the deadline, so that it falls while no gateway runs (`in_gap`), or
        after it (`connected`). A target that could not be started leaves the restart a fixed
        length."""
        workspace, tty = TIMEOUT_KINDS[(number + number // 8) % len(TIMEOUT_KINDS)]
        alignment = 'in_gap' if (number // 4) % 2 else 'connected'
        started = self.execs.run(gateway, workspace, f'timeout-{number:03d}', ['/bin/sleep', '600'], tty=tty,
                                 retry=10.0)
        row['timeout'] = {'tag': started['tag'], 'workspace': workspace, 'tty': tty, 'alignment': alignment,
                          'outcome': started['outcome'], 'code': started.get('code')}
        if started['outcome'] != 'started':
            return time.monotonic() + CYCLE_TIMEOUT_S, 0.0, None
        deadline = deadline_of(self.bench, started['process_id'])
        pid = tag_pid(self.bench.roots[workspace], started['tag'])
        if pid:
            self.watch.watch(started['tag'], pid)
        target = {'tag': started['tag'], 'cycle': number, 'workspace': workspace, 'tty': tty,
                  'process_id': started['process_id'], 'pid': pid, 'deadline_at_ms': deadline, 'alignment': alignment}
        self.pending_timeouts.append(target)
        if not deadline:
            return time.monotonic() + CYCLE_TIMEOUT_S, 0.0, target
        remaining = deadline / 1000 - time.time()
        if alignment == 'in_gap':
            return time.monotonic() + remaining - IN_GAP_LEAD_S, GAP_S, target
        return time.monotonic() + remaining + CONNECTED_LAG_S, 0.0, target

    def conclude_timeouts(self, gateway, final=False):
        """Each timeout target whose deadline has passed: when its process was seen gone, how the
        gateway that serves now reports it, and the deadline its record holds now."""
        for entry in list(self.pending_timeouts):
            deadline = entry['deadline_at_ms']
            if not final and deadline and time.time() < deadline / 1000:
                continue
            self.pending_timeouts.remove(entry)
            limit = max(0.0, (deadline or 0) / 1000 + TIMEOUT_WAIT_S - time.time())
            gone_unix = self.watch.wait(entry['tag'], limit) if entry['pid'] else None
            status = wait_state(gateway, entry['process_id'], exited, limit=15)
            later = (record_of(self.bench, entry['process_id']) or {}).get('deadline_at_ms')
            ended, served = entry.get('gateway_ended_unix'), entry.get('next_served_unix')
            entry.update(gone_unix=gone_unix, termination=status.get('termination'), status_error=status.get('error'),
                         deadline_after_ms=later, reported_by=gateway.pid,
                         in_gap=bool(deadline and ended and served and ended < deadline / 1000 < served))
            self.writer.write(self.raw / 'timeouts.jsonl', entry)

    def hold_approval(self, gateway, number):
        """An approval held and granted before the restart, for the next gateway to resume."""
        held = self.execs.run(gateway, 'confirm', f'approval-{number:03d}', ['/bin/sh', '-c', 'exit 0'], retry=5.0,
                              approval=True)
        approval = held.get('approval_id')
        entry = {'tag': held['tag'], 'cycle': number, 'approval_id': approval, 'held': held.get('code')}
        if approval:
            try:
                gateway.call('approval_resolve', {'approval_id': approval, 'decision': 'grant'})
            except ToolError as error:
                entry['grant_error'] = error.code
        if approval and 'grant_error' not in entry:
            self.pending_approval = entry
            return approval
        self.writer.write(self.raw / 'approvals.jsonl', entry)
        return None

    def resume_in_flight(self, gateway, approval, number):
        """The approval resumed in a thread, the gateway then killed after a delay of the range."""
        delay = KILL_DELAYS_S[(number // 4) % len(KILL_DELAYS_S)]
        result = {}
        thread = threading.Thread(target=lambda: result.update(resume_result(gateway, approval, timeout=30)),
                                  daemon=True)
        thread.start()
        time.sleep(delay)
        return thread, result, delay

    def resume_approval(self, gateway):
        """The approval granted before this restart, resumed once this gateway reconciled its
        workspace, and resumed again: the same process both times, its command run once."""
        entry, self.pending_approval = self.pending_approval, None
        approval = entry['approval_id']
        first, pending, waited = once_reconciled(lambda: resume_result(gateway, approval))
        deadline = time.monotonic() + 10
        while first.get('code') == 'RESOURCE_UNAVAILABLE' and time.monotonic() < deadline:
            time.sleep(0.5)
            first = resume_result(gateway, approval)
        again = resume_result(gateway, approval) if first.get('process_id') else None
        if first.get('process_id'):
            wait_state(gateway, first['process_id'], exited)
        entry.update(resumed_by=gateway.pid, resume=first, again=again, pending_refusals=pending, ready_ms=waited,
                     not_admitted=first.get('code') == 'RESOURCE_UNAVAILABLE')
        self.writer.write(self.raw / 'approvals.jsonl', entry)

    # ---------------------------------------------------------- the interval's end

    def conclude_targets(self):
        """Through the last gateway: the final reads, the pending approval and timeout target, then
        every long-running process terminated, effective when reported terminated and its process
        (a governed one's scope) gone."""
        gateway = self.gateway
        if gateway is None or gateway.process.poll() is not None:
            self.record['targets'] = []
            return
        for name, path in (('beat-pty', 'read_beat'), ('burst-pipe', 'read_burst')):
            try:
                row = self.readers[name].read(gateway, timeout=10)
                self.writer.write(self.raw / f'{path}.jsonl', dict(row, slot='final', outcome='read',
                                                                   gateway=gateway.pid))
            except ToolError as error:
                self.writer.write(self.raw / f'{path}.jsonl', {'slot': 'final', 'outcome': 'error', 'code': error.code})
        if self.pending_approval is not None:
            self.resume_approval(gateway)
        self.conclude_timeouts(gateway, final=True)
        self.record['targets'] = [self.end_target(gateway, name) for name in self.targets]

    def end_target(self, gateway, name):
        target = self.targets[name]
        try:
            ack = gateway.terminate(target['process_id'], timeout=10)
            status = gateway.wait_exited(target['process_id'], limit=30)
        except ToolError as error:
            return {'name': name, 'process_id': target['process_id'], 'error': error.code, 'effective': False}
        if target['scope_root_pid']:
            ended = host.wait_group_gone(target['scope_root_pid'], limit=30) is not None
        else:
            ended = bool(target['pid']) and gone(target['pid'], limit=30)
        termination = (status or {}).get('termination')
        return {'name': name, 'process_id': target['process_id'], 'ack_ms': ack, 'termination': termination,
                'scope_gone': ended, 'effective': termination == 'terminated' and ended}

    def end_last(self):
        """The last gateway ends with its input closed and detaches, as the others did."""
        gateway = self.gateway
        if gateway is None or gateway.process.poll() is not None:
            return
        ended = self.bench.end(gateway, 'eof')
        self.writer.write(self.raw / 'restarts.jsonl', {
            'cycle': self.number, 'gateway': gateway.pid, 'last': True, 'how': 'eof', 'ended': ended})

    def drain(self):
        """The Runner drained as an operator drains it: a gateway with the exit intent `stop`
        claims it under the next epoch, and its input closed stops the Runner, which exits 0 and
        removes its socket; with an authority, its instance retires and nothing of the consumer
        stays charged."""
        drain = {'governed': self.governed, 'expected_epoch': self.number + 1}
        try:
            gateway = self.bench.gateway('drain', intent='stop', timeout_s=CYCLE_TIMEOUT_S)
            claim = claimed_by(self.runner, gateway.pid)
            drain['claim_epoch'] = claim['epoch'] if claim else None
            drain['gateway'] = self.bench.end(gateway, 'eof')
        except Exception as error:  # noqa: BLE001 - a failed drain is evidence
            drain['error'] = f'{type(error).__name__}: {error}'
        drain['runner_exit'] = self.runner.wait(30)
        drain['socket_left'] = self.bench.socket.exists()
        found = re.findall(r'stopped; (\d+) processes were terminated', self.runner.log())
        drain['terminated'] = int(found[-1]) if found else None
        if self.governed:
            try:
                drain['retired'], drain['instances'] = wait_retired(self.bench, self.runner.pid)
                deadline = time.monotonic() + 60
                while True:
                    charged = host.journal_charged(self.args.journal)['attempts'].get(host.CONSUMER, 0)
                    if not charged or time.monotonic() >= deadline:
                        break
                    time.sleep(1.0)
                drain['charged'] = charged
            except (OSError, sqlite3.Error) as error:
                drain['journal_error'] = f'{type(error).__name__}: {error}'
                self.invalid.append(f"the journal could not be read: {drain['journal_error']}")
        return drain


def recovery_interval_metrics(record, rows, sampled, records, found, runs, release_id, service_pid, reservation):
    """A recovery interval's metrics from its raw files, as the run computes them and `verify`
    recomputes them: the control path over the time a gateway served, and the recovery evidence."""
    governed = record['governed']
    metrics = analysis.control_metrics(rows, record['served_s'], RECOVERY_PERIODS, release_expected=governed)
    targets = record.get('targets') or []
    metrics['termination']['status_target_effective'] = (all(t.get('effective') for t in targets)
                                                         if targets else None)
    if governed:
        _, failure = analysis.service_findings(sampled.get('service') or [], release_id, service_pid)
        metrics['service'] = {'rows': len(sampled.get('service') or []), 'failure': failure}
    footprints = sampled.get('footprint') or []
    metrics['host'] = analysis.host_peaks(sampled.get('host') or [])
    metrics['footprint'] = {
        'runner': analysis.footprint_metrics([r['owner'] for r in footprints if r.get('owner')], reservation),
        'gateway': analysis.footprint_metrics([r['gateway'] for r in footprints if r.get('gateway')], None)}
    metrics['journal'] = record.get('journal') or {}
    if record['kind'] == 'load':
        if record.get('consumers') is None:
            metrics['load'] = {'failures': [], 'issues': ['no load ran: there is no DevGuard service to run it']}
        else:
            metrics['load'] = analysis.load_metrics(runs, record['started_unix'], record['ended_unix'],
                                                    record['consumers'])
            metrics['load']['issues'] += record.get('load_issues') or []
            metrics['load']['failures'] += record.get('load_failures') or []
    metrics['recovery'] = {
        'restarts': analysis.restart_metrics([row for row in records['restarts'] if not row.get('last')]),
        'timeouts': analysis.timeout_metrics(records['timeouts']),
        'approvals': analysis.approval_metrics(records['approvals'], found),
        'markers': analysis.marker_metrics(found, records['execs']),
        'continuity': {'beat_pty': analysis.continuity_metrics(rows['read_beat'], 'beat'),
                       'burst_pipe': analysis.continuity_metrics(rows['read_burst'], 'burst')},
        'last_gateway': next((row.get('ended') for row in records['restarts'] if row.get('last')), None),
        'drain': record.get('drain'), 'planned_restarts': record['planned_restarts']}
    return metrics


def recovery_interval(context, kind, seconds, key):
    return RecoveryInterval(context, kind, seconds, key).run()


def recovery_repetition_outcome(report, idle, load):
    """A repetition's verdict: as a control repetition's when governed, otherwise from its two
    intervals alone."""
    if report.get('governed'):
        return repetition_outcome(report, idle, load)
    verdict, reasons = analysis.repetition_verdict(idle, load)
    if report['rehearsal']:
        verdict, reasons = 'inconclusive', reasons + ['rehearsal durations are below the protocol']
    return verdict, reasons


def recovery_repetition(context, plan, repetition):
    args, evidence, governed, release = (context[name] for name in ('args', 'evidence', 'governed', 'release'))
    key = f"{context['combination']}-{repetition}"
    report = {'schema': 'codespace-recovery-repetition/v1', 'combination': context['combination'],
              'repetition': repetition, 'rehearsal': args.rehearsal, 'governed': governed,
              'load': args.load if governed else None, 'service_before': release.state() if governed else None}

    def conclude(outcome):
        report['verdict'], report['reasons'] = outcome
        evidence.write_json(f'{key}/report.json', report)
        print(f"qualify: {key} {report['verdict']} {report['reasons']}", flush=True)
        return report

    if governed:
        context['service_pid'] = report['service_before']['service_pid']
        if report['service_before']['runs_release'] and args.load == 'warm':
            report['prewarm'] = context['measure'].prewarm(SimpleNamespace(qualify_bin=Path(args.qualify_bin)),
                                                           context['dg_release'], context['work'],
                                                           evidence.file(f'{key}/x').parent)
        early = early_outcome(report)
        if early:
            report['service_after'] = release.state()
            return conclude(early)
    idle = recovery_interval(context, 'idle', plan['idle_s'], key)
    load = recovery_interval(context, 'load', plan['load_s'], key)
    report['intervals'] = {'idle': {k: idle[k] for k in ('verdict', 'reasons')},
                           'load': {k: load[k] for k in ('verdict', 'reasons')}}
    if governed:
        for target in sorted((context['work'] / 'targets.noindex').glob(f'cold-{key}-*')):
            shutil.rmtree(target, ignore_errors=True)
        report['service_after'] = release.state()
    return conclude(recovery_repetition_outcome(report, idle, load))


def active_instances(args):
    return [entry for entry in host.journal_instances(host.CONSUMER, args.journal) if entry['state'] == 'active']


def recovery_measurement(args, live, evidence):
    """The measurement: governed against the installed service on macOS with `required`, a
    self-test of the harness otherwise."""
    governed = platform.system() == 'Darwin' and 'required' in args.participation
    combination = 'independent' if governed else 'independent-off'
    plan = dict(RECOVERY_REHEARSAL if args.rehearsal else RECOVERY_PROTOCOL, combinations=[combination],
                load=args.load if governed else None, rehearsal=args.rehearsal)
    if args.repetitions is not None:
        plan['repetitions'] = args.repetitions
    context = {'args': args, 'live': live, 'evidence': evidence, 'governed': governed, 'combination': combination,
               'release': None, 'measure': None, 'reservation': None}
    run = {'schema': 'codespace-recovery-run/v1', 'argv': sys.argv, 'plan': plan, 'protocol': RECOVERY_PROTOCOL,
           'periods': RECOVERY_PERIODS, 'request': CONTROL_REQUEST, 'governed': governed,
           'cycle': {'timeout_s': CYCLE_TIMEOUT_S, 'first_s': FIRST_CYCLE_S, 'planned_s': PLANNED_CYCLE_S,
                     'gap_s': GAP_S, 'in_gap_lead_s': IN_GAP_LEAD_S, 'connected_lag_s': CONNECTED_LAG_S,
                     'ends': list(host.GATEWAY_ENDS), 'kill_delays_s': list(KILL_DELAYS_S)},
           'targets': {'status_p99_ms': analysis.STATUS_P99_MS, 'termination_ack_p99_ms': analysis.TERMINATION_P99_MS},
           'host': host.host_facts(), 'harness': {'commit': git('rev-parse', 'HEAD'), 'dirty': dirty()}}
    if governed:
        measure = load_measure(args.devguard_checkout)
        live.measure = measure
        release = host.Release(args.release)
        problems = control_preconditions(args, release, measure)
        if active_instances(args):
            problems.append('a codespace instance is active: drain the independent Runner or stop the gateway first')
        work = Path(args.work).resolve()
        if work.exists():
            problems.append(f'{work} already exists')
        if problems:
            return {'verdict': 'inconclusive', 'problems': problems}
        work.mkdir(parents=True)
        manifest = json.loads(release.manifest.read_text())
        measure.export_source(manifest['source']['commit'], work)
        reservation = consumer_settings(Path(args.host_config).read_text()).get('control_reservation')
        context.update(measure=measure, release=release, dg_release=measure.Release(args.release), work=work,
                       reservation=reservation)
        run.update(release={'id': release.id, **release.artifacts(), 'state': release.state()},
                   policy=policy(release, args), work_capacity=host.work_capacity(args.service_log),
                   reservation=reservation)
        run['harness'].update(measure_sha256=host.sha256(Path(measure.__file__)),
                              devguard_checkout=git('rev-parse', 'HEAD', root=measure.ROOT),
                              qualify_sha256=host.sha256(args.qualify_bin), rustc=host.run_text('rustc', '--version'))
    run['started_unix'] = time.time()
    evidence.write_json('run.json', run)
    awake = None
    if platform.system() == 'Darwin':
        # The display and the system stay awake for the run; caffeinate ends with this process.
        awake = subprocess.Popen(['/usr/bin/caffeinate', '-d', '-i', '-w', str(os.getpid())],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        live.workers.append(awake)
    try:
        reports = [recovery_repetition(context, plan, repetition) for repetition in range(1, plan['repetitions'] + 1)]
    finally:
        if awake is not None:
            awake.terminate()
    combinations = {combination: {
        'verdict': analysis.combination_verdict([report['verdict'] for report in reports], plan['repetitions']),
        'repetitions': [{'repetition': report['repetition'], 'verdict': report['verdict'], 'reasons': report['reasons'],
                         'report_sha256': host.sha256(evidence.path / f"{combination}-{report['repetition']}/report.json")}
                        for report in reports]}}
    return {'verdict': analysis.overall_verdict(combinations, plan, RECOVERY_PROTOCOL, args.rehearsal), 'plan': plan,
            'combinations': combinations}


def recovery(args, live, evidence):
    """Every unit's functional scenarios, then the measurement: qualified only when both are."""
    results = recovery_functional(args, live, evidence, RECOVERY_UNITS)
    functional = {'verdict': analysis.suite_verdict([entry['verdict'] for entry in results]),
                  'scenarios': scenario_summary(results)}
    measurement = recovery_measurement(args, live, evidence)
    return {'verdict': analysis.recovery_verdict(functional['verdict'], measurement['verdict']),
            'functional': functional, 'measurement': measurement}


# ---------------------------------------------------------------- verify

def normalized(value):
    """A value as its JSON evidence holds it."""
    return json.loads(json.dumps(value))


def read_json(path):
    return json.loads(Path(path).read_text())


def compare(problems, label, recorded, recomputed):
    """Record a problem when a recorded value differs from its recomputation."""
    recorded, recomputed = normalized(recorded), normalized(recomputed)
    if recorded != recomputed:
        problems.append(f'{label}: recorded {json.dumps(recorded)[:400]}, recomputed {json.dumps(recomputed)[:400]}')


def verify_candidate(directory, problems):
    record = read_json(directory / 'candidate.json')
    found = candidate_problems(record)
    report = read_json(directory / 'report.json')
    if 'result' in report:
        compare(problems, 'candidate problems', report['result'].get('problems'), found)
    return 'failed' if found else 'passed'


def verify_scenarios(directory, problems):
    """matrix, faults and the recovery units: each scenario's verdict from its recorded checks."""
    verdicts = []
    for entry in read_rows(directory, 'scenarios'):
        verdict = analysis.scenario_verdict(entry['checks'], entry.get('not_run'))
        compare(problems, f'{scenario_label(entry)} verdict', entry['verdict'], verdict)
        verdicts.append(verdict)
    return analysis.suite_verdict(verdicts)


def verify_authority(directory, problems):
    """Each authority phase's checks, metrics and verdict from its raw samples."""
    verdicts = []
    for path in sorted(directory.glob('*/authority/*/phase.json')):
        record = read_json(path)
        label = f"{record['mode']} authority {record['phase']}"
        if record['phase'] == 'setup':
            compare(problems, f'{label} verdict', [record['verdict'], record['reasons']], ['inconclusive', SETUP_FAILED])
            verdicts.append('inconclusive')
            continue
        rows = {name: read_rows(path.parent, name) for name in analysis.SAMPLE_FILES}
        compare(problems, f'{label} raw files', record['raw'], raw_hashes(path.parent))
        compare(problems, f'{label} checks', record['checks'], phase_checks(record['expectation'], rows['refusals']))
        verdict, reasons, metrics = phase_verdict(record, rows)
        compare(problems, f'{label} metrics', record['metrics'], metrics)
        compare(problems, f'{label} verdict', [record['verdict'], record['reasons']], [verdict, reasons])
        verdicts.append(verdict)
    return analysis.suite_verdict(verdicts)


def verify_interval(directory, key, kind, release_id, service_pid, reservation, problems):
    """A control interval's metrics and verdict from its raw samples."""
    raw = directory / key / kind
    record = read_json(raw / 'interval.json')
    label = f'{key} {kind}'
    rows = {name: read_rows(raw, name) for name in analysis.SAMPLE_FILES}
    sampled = {name: read_rows(raw, name) for name in ('host', 'service', 'footprint')}
    runs = read_json(raw / 'runs.json') if (raw / 'runs.json').exists() else []
    compare(problems, f'{label} raw files', record['raw'], raw_hashes(raw))
    service_invalid, _ = analysis.service_findings(sampled['service'], release_id, service_pid)
    for reason in service_invalid:
        if reason not in record['invalid']:
            problems.append(f'{label}: the service samples make it invalid ({reason}), the record does not say so')
    metrics = interval_metrics(record, rows, sampled, runs, release_id, service_pid, reservation)
    verdict, reasons = analysis.interval_verdict(kind, record['invalid'], metrics)
    compare(problems, f'{label} metrics', record['metrics'], metrics)
    compare(problems, f'{label} verdict', [record['verdict'], record['reasons']], [verdict, reasons])
    return {'verdict': verdict, 'reasons': reasons}


def verify_control(directory, problems):
    """Every interval from its raw samples, then each repetition, combination and the run."""
    if not (directory / 'run.json').exists():
        # The run stopped at its preconditions, before any interval.
        return 'inconclusive'
    run = read_json(directory / 'run.json')
    plan, release_id, reservation = run['plan'], run['release']['id'], run['reservation']
    compare(problems, 'the run protocol', run['protocol'], CONTROL_PROTOCOL)
    compare(problems, 'the targets', run['targets'], {'status_p99_ms': analysis.STATUS_P99_MS,
                                                      'termination_ack_p99_ms': analysis.TERMINATION_P99_MS})
    combinations = {}
    for mode in plan['combinations']:
        verdicts = []
        for repetition in range(1, plan['repetitions'] + 1):
            key = f'{mode}-{repetition}'
            if not (directory / key / 'report.json').exists():
                continue
            report = read_json(directory / key / 'report.json')
            if 'intervals' in report:
                service_pid = report['service_before']['service_pid']
                idle = verify_interval(directory, key, 'idle', release_id, service_pid, reservation, problems)
                load = verify_interval(directory, key, 'load', release_id, service_pid, reservation, problems)
                outcome = repetition_outcome(report, idle, load)
            else:
                outcome = early_outcome(report) or ('inconclusive', ['the repetition has no intervals'])
            compare(problems, f'{key} verdict', [report['verdict'], report['reasons']], list(outcome))
            verdicts.append(outcome[0])
        combinations[mode] = {'verdict': analysis.combination_verdict(verdicts, plan['repetitions'])}
    report = read_json(directory / 'report.json')
    if 'result' in report:
        compare(problems, 'combination verdicts',
                {mode: entry['verdict'] for mode, entry in report['result']['combinations'].items()},
                {mode: entry['verdict'] for mode, entry in combinations.items()})
        for mode, entry in report['result']['combinations'].items():
            for repetition in entry['repetitions']:
                path = directory / f"{mode}-{repetition['repetition']}" / 'report.json'
                if not path.exists() or host.sha256(path) != repetition['report_sha256']:
                    problems.append(f"{mode}-{repetition['repetition']}: the report differs from the one the run recorded")
    return analysis.overall_verdict(combinations, plan, CONTROL_PROTOCOL, plan['rehearsal'])


def verify_recovery_interval(directory, key, kind, run, report, problems):
    """A recovery interval's metrics and verdict from its raw files."""
    raw = directory / key / kind
    record = read_json(raw / 'interval.json')
    label = f'{key} {kind}'
    compare(problems, f'{label} raw files', record['raw'], raw_hashes(raw))
    if 'metrics' not in record:
        compare(problems, f'{label} verdict', record['verdict'], 'inconclusive')
        return {'verdict': record['verdict'], 'reasons': record['reasons']}
    rows = {name: read_rows(raw, name) for name in RECOVERY_SAMPLES}
    sampled = {name: read_rows(raw, name) for name in ('host', 'service', 'footprint')}
    records = {name: read_rows(raw, name) for name in RECOVERY_RECORDS}
    found = read_json(raw / 'markers.json')
    runs = read_json(raw / 'runs.json') if (raw / 'runs.json').exists() else []
    release_id = (run.get('release') or {}).get('id')
    service_pid = (report.get('service_before') or {}).get('service_pid')
    if record['governed']:
        service_invalid, _ = analysis.service_findings(sampled['service'], release_id, service_pid)
        for reason in service_invalid:
            if reason not in record['invalid']:
                problems.append(f'{label}: the service samples make it invalid ({reason}), the record does not say so')
    metrics = recovery_interval_metrics(record, rows, sampled, records, found, runs, release_id, service_pid,
                                        run.get('reservation'))
    verdict, reasons = analysis.recovery_interval_verdict(kind, record['invalid'], metrics)
    compare(problems, f'{label} metrics', record['metrics'], metrics)
    compare(problems, f'{label} verdict', [record['verdict'], record['reasons']], [verdict, reasons])
    return {'verdict': verdict, 'reasons': reasons}


def verify_measurement(directory, problems):
    """Every recovery interval from its raw files, then each repetition, the combination and the
    measurement."""
    if not (directory / 'run.json').exists():
        # The measurement stopped at its preconditions, before any interval.
        return 'inconclusive'
    run = read_json(directory / 'run.json')
    plan = run['plan']
    compare(problems, 'the recovery protocol', run['protocol'], RECOVERY_PROTOCOL)
    combinations = {}
    for combination in plan['combinations']:
        verdicts = []
        for repetition in range(1, plan['repetitions'] + 1):
            key = f'{combination}-{repetition}'
            if not (directory / key / 'report.json').exists():
                continue
            report = read_json(directory / key / 'report.json')
            if 'intervals' in report:
                idle = verify_recovery_interval(directory, key, 'idle', run, report, problems)
                load = verify_recovery_interval(directory, key, 'load', run, report, problems)
                outcome = recovery_repetition_outcome(report, idle, load)
            else:
                outcome = early_outcome(report) or ('inconclusive', ['the repetition has no intervals'])
            compare(problems, f'{key} verdict', [report['verdict'], report['reasons']], list(outcome))
            verdicts.append(outcome[0])
        combinations[combination] = {'verdict': analysis.combination_verdict(verdicts, plan['repetitions'])}
    measured = (read_json(directory / 'report.json').get('result') or {}).get('measurement') or {}
    if 'combinations' in measured:
        compare(problems, 'combination verdicts', {name: entry['verdict'] for name, entry in measured['combinations'].items()},
                {name: entry['verdict'] for name, entry in combinations.items()})
        for name, entry in measured['combinations'].items():
            for repetition in entry['repetitions']:
                path = directory / f"{name}-{repetition['repetition']}" / 'report.json'
                if not path.exists() or host.sha256(path) != repetition['report_sha256']:
                    problems.append(f"{name}-{repetition['repetition']}: the report differs from the one the run recorded")
    return analysis.overall_verdict(combinations, plan, RECOVERY_PROTOCOL, plan['rehearsal'])


def verify_recovery(directory, problems):
    """The recovery suite: its functional scenarios and its measurement."""
    functional = verify_scenarios(directory, problems)
    measurement = verify_measurement(directory, problems)
    result = read_json(directory / 'report.json').get('result') or {}
    if result:
        compare(problems, 'functional verdict', (result.get('functional') or {}).get('verdict'), functional)
        compare(problems, 'measurement verdict', (result.get('measurement') or {}).get('verdict'), measurement)
    return analysis.recovery_verdict(functional, measurement)


VERIFIERS = {'candidate': verify_candidate, 'matrix': verify_scenarios, 'faults': verify_scenarios,
             'authority': verify_authority, 'control': verify_control, 'recovery': verify_recovery,
             **{unit: verify_scenarios for unit in RECOVERY_UNITS}}
HARNESS = ('qualify-devguard.py', 'qualify_analysis.py', 'qualify_host.py')


def harness():
    """The harness that ran: its files' SHA-256, the CodeSpace commit, and the open-file limit it
    and every process it started ran under (soft and hard; a macOS terminal's soft limit is 256)."""
    found = {'files': {name: host.sha256(ROOT / 'scripts' / name) for name in HARNESS},
             'open_files_limit': list(resource.getrlimit(resource.RLIMIT_NOFILE))}
    try:
        found.update(commit=git('rev-parse', 'HEAD'), dirty=dirty())
    except (OSError, subprocess.CalledProcessError):
        found.update(commit=None, dirty=None)
    return found


def verify(directory):
    """Check a sealed run's files against its MANIFEST.json and recompute its verdicts from the
    raw evidence with this harness's analysis. Writes nothing."""
    directory = Path(directory).resolve()
    if not (directory / 'MANIFEST.json').is_file() or not (directory / 'report.json').is_file():
        return {'directory': str(directory), 'verdict': 'failed',
                'problems': ['not a sealed run: MANIFEST.json or report.json is missing']}
    problems = host.verify_manifest(directory)
    report = read_json(directory / 'report.json')
    suite = report.get('suite')
    if suite not in VERIFIERS:
        return {'directory': str(directory), 'verdict': 'failed', 'problems': problems + [f'unknown suite {suite!r}']}
    try:
        recomputed = VERIFIERS[suite](directory, problems)
    except (OSError, ValueError, KeyError, TypeError) as error:
        problems.append(f'the evidence cannot be recomputed: {type(error).__name__}: {error}')
        recomputed = None
    recorded = report.get('verdict')
    if 'result' in report and recomputed is not None and recomputed != recorded:
        problems.append(f'verdict: recorded {recorded}, recomputed {recomputed}')
    ran = (report.get('harness') or {}).get('files')
    return {'directory': str(directory), 'suite': suite, 'recorded_verdict': recorded,
            'recomputed_verdict': recomputed, 'same_harness': ran == harness()['files'],
            'verdict': 'failed' if problems else 'verified', 'problems': problems}


# ---------------------------------------------------------------- command line

SUITES = {'candidate': candidate, 'matrix': matrix, 'faults': faults, 'authority': authority, 'control': control,
          'recovery': recovery, **{unit: recovery_unit(unit) for unit in RECOVERY_UNITS}}


def subset(allowed, name):
    def parse(text):
        chosen = [part.strip() for part in text.split(',') if part.strip()]
        if not chosen or any(part not in allowed for part in chosen):
            raise argparse.ArgumentTypeError(f"{name} are a comma-separated subset of {', '.join(allowed)}")
        return chosen
    return parse


modes = subset(MODES, 'modes')
participations = subset(PARTICIPATIONS, 'participations')
RECOVERY_HELP = {
    'recovery-lifecycle': 'the independent Runner across every way a gateway ends (P1R-C01)',
    'recovery-store': "process records and the operations database's one writer (P1R-C02)",
    'recovery-fence': 'Claims, controller epochs and other processes at the socket (P1R-C03)',
    'recovery-reconcile': 'reconciliation after a restart, its races and an authority outage (P1R-C04)',
    'recovery-observation': 'output gaps, retention and a lost Runner (P1R-C05)'}


def installed_options(required):
    """The installed DevGuard service's options: required for the CS-RG suites, and for `recovery`
    only when it measures against the service."""
    options = argparse.ArgumentParser(add_help=False)
    options.add_argument('--release', required=required, help='the installed DevGuard release ID')
    options.add_argument('--socket', type=Path, default=host.default_socket())
    options.add_argument('--credential-file', type=Path, default=host.CREDENTIAL_FILE,
                         help="the codespace consumer's credential file; its path is passed, never read here")
    options.add_argument('--journal', type=Path, default=host.JOURNAL)
    options.add_argument('--service-log', type=Path, default=host.SERVICE_LOG)
    options.add_argument('--host-config', type=Path, default=host.HOST_CONFIG)
    return options


def measurement_options(parser, required):
    """DG-1's load and the run's length, as `control` and `recovery` measure."""
    parser.add_argument('--work', type=Path, required=required,
                        help="a new directory for the load's DevGuard source and target directories")
    parser.add_argument('--qualify-bin', type=Path, required=required,
                        help="DevGuard's devguard-qualify, as DG-1's measurement uses it")
    parser.add_argument('--devguard-checkout', type=Path, required=required,
                        help="a DevGuard checkout holding the release's source commit, for scripts/measure.py")
    parser.add_argument('--load', choices=('cold', 'warm'), default='cold')
    parser.add_argument('--rehearsal', action='store_true', help='short intervals to try the run; never qualifies')
    parser.add_argument('--repetitions', type=int, help='repetitions (protocol: 3)')


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n', 1)[0],
                                     epilog='docs/cs-rg-qualification.md and docs/recovery-qualification.md describe '
                                            'each suite and its evidence.')
    suites = parser.add_subparsers(dest='suite', metavar='suite')
    suites.required = True

    base = argparse.ArgumentParser(add_help=False)
    base.add_argument('--out', type=Path, required=True, help='a new evidence directory')
    base.add_argument('--mcp-bin', type=Path, required=True, help='codespace-mcp built with --features devguard')
    base.add_argument('--runtime-bin', type=Path, required=True,
                      help='codespace-codex-runtime built with --features devguard (the uds worker, or the '
                           'independent Runner)')
    common = argparse.ArgumentParser(add_help=False, parents=[base])
    common.add_argument('--modes', type=modes, default=list(MODES), help='in-process, uds or both (default both)')
    installed = installed_options(True)
    fixture = argparse.ArgumentParser(add_help=False)
    fixture.add_argument('--fixture-bin', type=Path, required=True,
                         help="DevGuard's fixture authority at CodeSpace's pin (examples/fixture_authority)")
    fixture.add_argument('--fixture-helper', type=Path, required=True,
                         help="devguard-launch built from CodeSpace's DevGuard pin")

    one = suites.add_parser('candidate', parents=[common, installed], help='record the combination under test')
    for name in ('--fixture-bin', '--fixture-helper', '--qualify-bin'):
        one.add_argument(name, type=Path, help='record this binary too')
    suites.add_parser('matrix', parents=[common, installed], help='the claimed modes against the installed service')
    suites.add_parser('faults', parents=[common, fixture], help="fault cases against DevGuard's fixture authority")
    suites.add_parser('authority', parents=[common, fixture],
                      help='control-path latency while the fixture authority stalls, pauses, restarts and stops')
    control_parser = suites.add_parser('control', parents=[common, installed],
                                       help="control-path latency under DG-1's load against the installed service")
    measurement_options(control_parser, required=True)
    recovering = argparse.ArgumentParser(add_help=False, parents=[base])
    recovering.add_argument('--participation', type=participations, default=list(PARTICIPATIONS),
                            help="off, required or both (default both); required runs on macOS only, against "
                                 "DevGuard's fixture authority")
    recovering.add_argument('--fixture-bin', type=Path,
                            help="DevGuard's fixture authority at CodeSpace's pin, for required (macOS)")
    recovering.add_argument('--fixture-helper', type=Path,
                            help="devguard-launch built from CodeSpace's DevGuard pin, for required (macOS)")
    for unit, text in RECOVERY_HELP.items():
        suites.add_parser(unit, parents=[recovering], help=text)
    recovery_parser = suites.add_parser(
        'recovery', parents=[recovering, installed_options(False)],
        help='every recovery suite, then gateway restarts measured against the installed service (required, '
             'macOS) or as a self-test of the harness')
    measurement_options(recovery_parser, required=False)
    check_parser = suites.add_parser('verify', help='check a sealed run and recompute its verdicts')
    check_parser.add_argument('directory', type=Path)
    args = parser.parse_args(argv)
    if args.suite in (*RECOVERY_UNITS, 'recovery') and platform.system() == 'Darwin' and 'required' in args.participation:
        needed = ['fixture_bin', 'fixture_helper']
        if args.suite == 'recovery':
            needed += ['release', 'work', 'qualify_bin', 'devguard_checkout']
        missing = ['--' + name.replace('_', '-') for name in needed if getattr(args, name) is None]
        if missing:
            parser.error(f"{args.suite} with required participation needs {', '.join(missing)}")
    return args


def interrupted(number, frame):
    raise KeyboardInterrupt(f'signal {number}')


def main(argv=None):
    args = arguments(argv)
    if args.suite == 'verify':
        result = verify(args.directory)
        print(json.dumps(result, indent=2))
        return 0 if result['verdict'] == 'verified' else 1
    evidence = host.Evidence(args.out)
    live = Live()
    for number in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(number, interrupted)
    report = {'schema': 'codespace-qualification-report/v1', 'suite': args.suite,
              'argv': sys.argv if argv is None else [sys.argv[0], *argv], 'started_unix': time.time(),
              'harness': harness()}
    status = EXIT_CRASH
    try:
        result = SUITES[args.suite](args, live, evidence)
        report.update(result=result, verdict=result['verdict'])
        status = EXIT[result['verdict']]
    except KeyboardInterrupt:
        report['verdict'] = 'interrupted'
        status = 130
    except Exception as error:  # noqa: BLE001 - a harness failure is recorded and sealed too
        report.update(verdict='harness_failed', error=f'{type(error).__name__}: {error}',
                      trace=traceback.format_exc()[-5000:])
    finally:
        report['settlement'] = live.settle()
        report['ended_unix'] = time.time()
        evidence.write_json('report.json', report)
        manifest = evidence.seal()
        print(f"qualify: {args.suite}: {report['verdict']}; evidence {evidence.path}, MANIFEST.json SHA-256 {manifest}",
              flush=True)
    return status


if __name__ == '__main__':
    sys.exit(main())

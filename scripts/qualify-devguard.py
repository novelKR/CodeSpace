#!/usr/bin/env python3
"""CSRG-U6 qualification of CodeSpace with DevGuard; docs/cs-rg-qualification.md is the protocol.

  candidate  record the exact combination under test
  matrix     the claimed modes against the installed DevGuard service, each launch checked
             against DevGuard's own record of its attempt
  faults     the cases that would leave the installed service with Suspect or charged work,
             against DevGuard's fixture authority at CodeSpace's pin
  control    control-path latency under DG-1's development load, against the installed service
  authority  control-path latency while the fixture authority stalls, pauses, restarts and stops
  verify     check a sealed run and recompute its verdicts from its raw evidence

Every suite but verify writes a new evidence directory and seals it; verify writes nothing.
Nothing here edits DevGuard's configuration, restarts its service or reads its credential: the
gateway is given that file's path and reads it itself, and the fault cases use DevGuard's fixture
authority and throwaway credentials in their own scratch directories. Exit status: 0 passed or
qualified, 1 failed, 2 incomplete or inconclusive, 3 the harness failed, 130 interrupted; verify
exits 0 when the sealed run checks out and 1 when it does not.
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
import secrets
import shutil
import signal
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
]


# ---------------------------------------------------------------- what a run starts

class Live:
    """What a run started, settled however it ends: every process a gateway started is ended and
    waited for before the gateway's input closes, then workers and fixture authorities stop, and
    DG-1's load owners are asked to settle their scopes."""

    def __init__(self):
        self.gateways, self.fixtures, self.workers = [], [], []
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


def run_scenario(results, evidence, mode, io, name, function, *args):
    """Run one scenario and keep its checks; a scenario that breaks is a failed check."""
    began = time.time()
    try:
        checks, detail = function(*args)
    except Exception as error:  # noqa: BLE001 - every broken scenario is evidence
        checks = [check('the scenario completed', False, error=f'{type(error).__name__}: {error}',
                        trace=traceback.format_exc()[-3000:])]
        detail = {}
    entry = {'scenario': name, 'mode': mode, 'io': io, 'verdict': analysis.scenario_verdict(checks),
             'checks': checks, 'detail': detail, 'started_unix': began, 'ended_unix': time.time()}
    results.append(entry)
    evidence.append('scenarios.jsonl', entry)
    print(f"qualify: {mode} {io or '-'} {name}: {entry['verdict']}", flush=True)
    return entry


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
    # The pinned Codex PTY reports a signal death as exit code 1 (docs/agent-integration.md).
    death = {'termination': 'exited', 'exit_code': 1} if tty else {'termination': 'signaled', 'signal': 15}
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
    worker = subprocess.Popen([str(bench.args.runtime_bin), str(socket)], stdin=subprocess.DEVNULL,
                              stdout=subprocess.DEVNULL, stderr=open(bench.evidence.file(f'{bench.mode}/standalone-worker.log'), 'ab'),
                              env=host.minimal_env(bench.work), start_new_session=True)
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


def settled_checks(bench, label):
    """After a mode: nothing of the consumer is charged, and its instances are not active."""
    deadline = time.monotonic() + 60
    while True:
        charged = host.journal_charged(bench.database)
        mine = charged['attempts'].get(bench.authority['consumer'], 0)
        if not mine or time.monotonic() >= deadline:
            break
        time.sleep(1)
    instances = host.journal_instances(bench.authority['consumer'], bench.database)
    return [check(f'{label}: nothing of the consumer stays charged', mine == 0, charged=charged),
            check(f'{label}: no instance stays active', not any(entry['state'] == 'active' for entry in instances),
                  instances=instances)]


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
                    print('qualify: work stayed charged; the matrix stops here', flush=True)
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
    """One phase: its termination targets started before the fault, the fault applied, the
    schedules run for PHASE_S seconds, then the fault lifted where it can be. The samples have
    their own writer, closed before their files are hashed."""
    raw = evidence.file(f'{mode}/authority/{phase}/x').parent
    samples = host.Writer()
    schedules, watcher, invalid = {}, None, []
    try:
        pool = [dict(gateway.exec(f'term-{n}', ['/bin/sleep', '600'], tty=bool(n % 2)), tty=bool(n % 2))
                for n in range(len(PHASE_TERMINATIONS))]
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
    """matrix and faults: each scenario's verdict from its recorded checks."""
    verdicts = []
    for entry in read_rows(directory, 'scenarios'):
        verdict = analysis.scenario_verdict(entry['checks'])
        compare(problems, f"{entry['mode']} {entry['io'] or '-'} {entry['scenario']} verdict", entry['verdict'], verdict)
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


VERIFIERS = {'candidate': verify_candidate, 'matrix': verify_scenarios, 'faults': verify_scenarios,
             'authority': verify_authority, 'control': verify_control}
HARNESS = ('qualify-devguard.py', 'qualify_analysis.py', 'qualify_host.py')


def harness():
    """The harness that ran: its files' SHA-256 and the CodeSpace commit."""
    found = {'files': {name: host.sha256(ROOT / 'scripts' / name) for name in HARNESS}}
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

SUITES = {'candidate': candidate, 'matrix': matrix, 'faults': faults, 'authority': authority, 'control': control}


def modes(text):
    chosen = [part.strip() for part in text.split(',') if part.strip()]
    unknown = [mode for mode in chosen if mode not in MODES]
    if unknown or not chosen:
        raise argparse.ArgumentTypeError(f"modes are a comma-separated subset of {', '.join(MODES)}")
    return chosen


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n', 1)[0],
                                     epilog='docs/cs-rg-qualification.md describes each suite and its evidence.')
    suites = parser.add_subparsers(dest='suite', metavar='suite')
    suites.required = True

    common = argparse.ArgumentParser(add_help=False)
    common.add_argument('--out', type=Path, required=True, help='a new evidence directory')
    common.add_argument('--mcp-bin', type=Path, required=True, help='codespace-mcp built with --features devguard')
    common.add_argument('--runtime-bin', type=Path, required=True,
                        help='codespace-codex-runtime built with --features devguard (the uds worker)')
    common.add_argument('--modes', type=modes, default=list(MODES), help='in-process, uds or both (default both)')
    installed = argparse.ArgumentParser(add_help=False)
    installed.add_argument('--release', required=True, help='the installed DevGuard release ID')
    installed.add_argument('--socket', type=Path, default=host.default_socket())
    installed.add_argument('--credential-file', type=Path, default=host.CREDENTIAL_FILE,
                           help="the codespace consumer's credential file; its path is passed, never read here")
    installed.add_argument('--journal', type=Path, default=host.JOURNAL)
    installed.add_argument('--service-log', type=Path, default=host.SERVICE_LOG)
    installed.add_argument('--host-config', type=Path, default=host.HOST_CONFIG)
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
    control_parser.add_argument('--work', type=Path, required=True,
                                help="a new directory for the load's DevGuard source and target directories")
    control_parser.add_argument('--qualify-bin', type=Path, required=True,
                                help="DevGuard's devguard-qualify, as DG-1's measurement uses it")
    control_parser.add_argument('--devguard-checkout', type=Path, required=True,
                                help="a DevGuard checkout holding the release's source commit, for scripts/measure.py")
    control_parser.add_argument('--load', choices=('cold', 'warm'), default='cold')
    control_parser.add_argument('--rehearsal', action='store_true',
                                help='short intervals to try the run; never qualifies')
    control_parser.add_argument('--repetitions', type=int, help='repetitions per combination (protocol: 3)')
    check_parser = suites.add_parser('verify', help='check a sealed run and recompute its verdicts')
    check_parser.add_argument('directory', type=Path)
    return parser.parse_args(argv)


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

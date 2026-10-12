"""CSRG-U6 qualification on the host: CodeSpace's gateway driven over its MCP stdio, DevGuard's
installed service and fixture authority observed, and the evidence written and sealed.

Nothing here reads, prints or passes a credential. A gateway is given the credential file's path
and opens the file itself; DevGuard's journal is opened read-only and only the fields a check
needs are copied from it, never a permit's hash or a lease ID. Gateways and fixture authorities
run in a minimal environment (HOME, PATH, TMPDIR, LANG and CodeSpace's own settings) in their own
sessions, so a terminal's interrupt reaches only the harness, which then settles what it started.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import re
import sqlite3
import subprocess
import threading
import time

from qualify_analysis import TRANSPORT

SUPPORT = Path.home() / 'Library/Application Support/DevGuard'
JOURNAL = SUPPORT / 'state/authority.sqlite'
SERVICE_LOG = Path.home() / 'Library/Logs/DevGuard/devguardd.log'
HOST_CONFIG = Path.home() / '.config/devguard/host.toml'
# The consumer the owner provisions for CodeSpace in the installed service's configuration.
CONSUMER = 'codespace'
GENERATION = 'codespace-g1'
CREDENTIAL_FILE = SUPPORT / 'credentials/codespace.secret'
MINIMAL_PATH = '/usr/bin:/bin:/usr/sbin:/sbin'
MCP_PROTOCOL = '2025-11-25'
CALL_TIMEOUT_S = 30.0
# The code of a call that got no answer within its timeout; a lost connection is TRANSPORT.
NO_ANSWER = 'no_answer'
# Codes that are the transport's, not CodeSpace's answer.
TRANSPORT_CODES = (TRANSPORT, NO_ANSWER)


def default_socket():
    return Path(f'/private/tmp/devguard-{os.getuid()}/authority.sock')


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def run_text(*args, timeout=30, env=None, cwd=None):
    """A command's standard output, or None when it cannot run, fails or does not finish in time."""
    try:
        result = subprocess.run([str(arg) for arg in args], capture_output=True, text=True, timeout=timeout,
                                env=env, cwd=cwd)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def run_json(*args, timeout=60):
    text = run_text(*args, timeout=timeout)
    try:
        return json.loads(text) if text else None
    except ValueError:
        return None


def minimal_env(tmpdir, extra=None):
    """The environment of every process the harness starts: no inherited variable reaches it."""
    env = {'HOME': str(Path.home()), 'PATH': MINIMAL_PATH, 'TMPDIR': str(tmpdir), 'LANG': 'C'}
    env.update(extra or {})
    return env


# ---------------------------------------------------------------- evidence

class Evidence:
    """A new evidence directory. Raw files are written as the run goes; `seal` records every
    file's SHA-256 in MANIFEST.json and makes the files read-only, and nothing is written after."""

    def __init__(self, path):
        self.path = Path(path).resolve()
        if self.path.exists():
            raise SystemExit(f'qualify: {self.path} already exists')
        self.path.mkdir(parents=True, mode=0o700)
        self.lock = threading.Lock()
        self.sealed = None

    def file(self, name):
        path = self.path / name
        path.parent.mkdir(parents=True, exist_ok=True)
        return path

    def write_json(self, name, value):
        with self.lock:
            self.file(name).write_text(json.dumps(value, indent=2) + '\n')

    def append(self, name, row):
        with self.lock:
            with open(self.file(name), 'a') as handle:
                handle.write(json.dumps(row) + '\n')

    def seal(self):
        if self.sealed is None:
            manifest = self.path / 'MANIFEST.json'
            files = sorted(path for path in self.path.rglob('*') if path.is_file() and path != manifest)
            manifest.write_text(json.dumps({
                'schema': 'codespace-qualification-manifest/v1', 'sealed_unix': time.time(),
                'files': {str(path.relative_to(self.path)): sha256(path) for path in files}}, indent=1) + '\n')
            for path in files + [manifest]:
                os.chmod(path, 0o400)
            self.sealed = sha256(manifest)
        return self.sealed


def verify_manifest(directory):
    """The files of a sealed directory that are missing, changed or not listed."""
    directory = Path(directory)
    manifest = directory / 'MANIFEST.json'
    listed = json.loads(manifest.read_text())['files']
    present = {str(path.relative_to(directory)) for path in directory.rglob('*')
               if path.is_file() and path != manifest}
    problems = [f'missing {name}' for name in sorted(set(listed) - present)]
    problems += [f'not listed {name}' for name in sorted(present - set(listed))]
    problems += [f'changed {name}' for name in sorted(set(listed) & present)
                 if sha256(directory / name) != listed[name]]
    return problems


class Writer(threading.Thread):
    """Appends JSON lines on its own thread, as DevGuard's measure.py does, so no sampler waits on
    a disk write: a disk the load saturates delays the evidence, never a sample."""

    def __init__(self):
        super().__init__(daemon=True)
        self.lines = queue.Queue()
        self.files = {}
        self.errors = []
        self.written = 0
        self.lag_max = 0.0
        self.report = None
        self.start()

    def write(self, path, row):
        self.lines.put((time.monotonic(), Path(path), json.dumps(row) + '\n'))

    def run(self):
        ending = False
        while not ending:
            batch = [self.lines.get()]
            while True:
                try:
                    batch.append(self.lines.get_nowait())
                except queue.Empty:
                    break
            ending = None in batch
            batch = [item for item in batch if item is not None]
            for _, path, line in batch:
                try:
                    if path not in self.files:
                        path.parent.mkdir(parents=True, exist_ok=True)
                        self.files[path] = open(path, 'a')
                    self.files[path].write(line)
                    self.written += 1
                except OSError as error:
                    self.errors.append(f'{path.name}: {error}')
            for path, handle in self.files.items():
                try:
                    handle.flush()
                except OSError as error:
                    self.errors.append(f'{path.name}: {error}')
            if batch:
                self.lag_max = max(self.lag_max, time.monotonic() - batch[0][0])

    def close(self, limit=120):
        """Write every line handed over and close the files; a disk that does not take them within
        `limit` seconds is reported, never waited for without end."""
        if self.report is None:
            self.lines.put(None)
            self.join(timeout=limit)
            if self.is_alive():
                self.errors.append(f'the evidence writer did not finish within {limit} s')
            for path, handle in list(self.files.items()):
                try:
                    handle.close()
                except OSError as error:
                    self.errors.append(f'{path.name}: {error}')
            self.report = {'lines': self.written, 'lag_max_ms': round(self.lag_max * 1000, 3),
                           'errors': self.errors[:20], 'error_count': len(self.errors)}
        return self.report


class Sampler(threading.Thread):
    """Calls `sample` every `period` seconds until stopped and hands each row to the writer; a
    sample that raises is kept as an error row."""

    def __init__(self, writer, path, period, sample):
        super().__init__(daemon=True)
        self.writer, self.path, self.period, self.sample = writer, path, period, sample
        self.halt = threading.Event()
        self.rows = []

    def run(self):
        next_at = time.monotonic()
        while not self.halt.is_set():
            try:
                row = {'unix': time.time(), **self.sample()}
            except Exception as error:  # every failed sample is kept as evidence
                row = {'unix': time.time(), 'error': f'{type(error).__name__}: {error}'}
            self.rows.append(row)
            self.writer.write(self.path, row)
            next_at += self.period
            self.halt.wait(max(0.0, next_at - time.monotonic()))

    def stop(self):
        self.halt.set()
        self.join()
        return self.rows


# ---------------------------------------------------------------- MCP

class ToolError(Exception):
    """A call that did not succeed: CodeSpace's error code and message, or the transport's
    (`connection_lost`, `no_answer`, `rpc_<code>`), with the round trip when an answer came."""

    def __init__(self, code, message, body=None, latency_ms=None):
        super().__init__(f'{code}: {message}')
        self.code, self.message, self.body, self.latency_ms = code, message, body or {}, latency_ms

    @property
    def transport(self):
        return self.code in TRANSPORT_CODES or str(self.code).startswith('rpc_')


class McpClient:
    """Newline-delimited JSON-RPC over a gateway's stdio, as rmcp speaks it. Calls may come from
    several threads at once; each answer is matched by its id and timed when the reader thread
    takes it off the pipe. A connection that ends fails every call in flight and every later one
    with `connection_lost`."""

    def __init__(self, process, record=None):
        self.process = process
        self.record = record
        self.write_lock = threading.Lock()
        self.lock = threading.Lock()
        self.pending = {}
        self.next_id = 0
        self.lost = None
        self.malformed = 0
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        reason = 'the gateway closed its output'
        try:
            for line in self.process.stdout:
                arrived = time.monotonic()
                try:
                    message = json.loads(line)
                except ValueError:
                    self.malformed += 1
                    continue
                if self.record:
                    self.record({'dir': 'in', 'mono': arrived, 'message': message})
                if not isinstance(message, dict):
                    self.malformed += 1
                elif 'method' in message:
                    self._answer_server(message)
                else:
                    with self.lock:
                        waiter = self.pending.pop(message.get('id'), None)
                    if waiter:
                        waiter['message'], waiter['arrived'] = message, arrived
                        waiter['event'].set()
        except (OSError, ValueError) as error:
            reason = f'{type(error).__name__}: {error}'
        with self.lock:
            self.lost = reason
            waiters = list(self.pending.values())
            self.pending.clear()
        for waiter in waiters:
            waiter['event'].set()

    def _answer_server(self, message):
        if 'id' not in message:
            return
        if message['method'] == 'ping':
            reply = {'jsonrpc': '2.0', 'id': message['id'], 'result': {}}
        else:
            reply = {'jsonrpc': '2.0', 'id': message['id'], 'error': {'code': -32601, 'message': 'method not found'}}
        try:
            self._send(reply)
        except (OSError, ValueError):
            pass

    def _send(self, message):
        data = (json.dumps(message) + '\n').encode()
        with self.write_lock:
            self.process.stdin.write(data)
            self.process.stdin.flush()

    def notify(self, method, params=None):
        message = {'jsonrpc': '2.0', 'method': method}
        if params is not None:
            message['params'] = params
        self._send(message)
        if self.record:
            self.record({'dir': 'out', 'mono': time.monotonic(), 'message': message})

    def request(self, method, params, timeout=CALL_TIMEOUT_S):
        """The answer's result and its round trip in milliseconds."""
        waiter = {'event': threading.Event(), 'message': None, 'arrived': None}
        with self.lock:
            if self.lost is not None:
                raise ToolError(TRANSPORT, self.lost)
            self.next_id += 1
            ident = self.next_id
            self.pending[ident] = waiter
        message = {'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params}
        sent = time.monotonic()
        try:
            self._send(message)
        except (OSError, ValueError) as error:
            with self.lock:
                self.pending.pop(ident, None)
            raise ToolError(TRANSPORT, f'cannot write to the gateway: {error}')
        if self.record:
            self.record({'dir': 'out', 'mono': sent, 'message': message})
        if not waiter['event'].wait(timeout):
            with self.lock:
                self.pending.pop(ident, None)
            raise ToolError(NO_ANSWER, f'no answer within {timeout} s')
        if waiter['message'] is None:
            raise ToolError(TRANSPORT, self.lost or 'the connection ended')
        latency = round((waiter['arrived'] - sent) * 1000, 3)
        answer = waiter['message']
        if 'error' in answer:
            error = answer.get('error') or {}
            raise ToolError(f"rpc_{error.get('code')}", str(error.get('message')), latency_ms=latency)
        return answer.get('result') or {}, latency

    def call(self, name, arguments, timeout=CALL_TIMEOUT_S):
        """A tool's payload and round trip in milliseconds; a tool error raises ToolError with
        CodeSpace's code and the round trip."""
        result, latency = self.request('tools/call', {'name': name, 'arguments': arguments}, timeout)
        text = ''.join(part.get('text', '') for part in result.get('content') or []
                       if isinstance(part, dict) and part.get('type') == 'text')
        if result.get('isError'):
            try:
                body = json.loads(text)
            except ValueError:
                body = None
            body = body if isinstance(body, dict) else {}
            raise ToolError(body.get('code') or 'TOOL_ERROR', body.get('message') or text, body, latency)
        payload = result.get('structuredContent')
        if payload is None:
            try:
                payload = json.loads(text) if text else {}
            except ValueError:
                payload = {'text': text}
        return payload, latency


# ---------------------------------------------------------------- the gateway

def launch_line(text):
    """The governed execution a gateway log line reports, if it reports one: its attempt and
    process IDs, whether it was launched, and the scope's root (the helper's PID)."""
    if 'DevGuard managed launch' not in text and 'DevGuard preparation' not in text:
        return None
    attempt = re.search(r'\battempt_id=(cs-attempt-[0-9a-f]+)', text)
    process = re.search(r'\bprocess_id=(\S+)', text)
    if not (attempt and process):
        return None
    entry = {'attempt_id': attempt[1], 'process_id': process[1]}
    if 'the command was not started' in text or 'DevGuard preparation' in text:
        entry['launched'] = False
        outcome = re.search(r'\boutcome=(\w+)', text)
        entry['outcome'] = outcome[1] if outcome else None
    else:
        entry['launched'] = True
        root = re.search(r'\bscope_root_pid=Some\((\d+)\)', text)
        dispatch = re.search(r'\bdispatch=(\w+)', text)
        entry['scope_root_pid'] = int(root[1]) if root else None
        entry['dispatch'] = dispatch[1] if dispatch else None
    return entry


def refusal_ids(message):
    """The attempt and process IDs a governed refusal names."""
    match = re.search(r'attempt `(cs-attempt-[0-9a-f]+)` for process_id `([^`]+)`', message or '')
    return (match[1], match[2]) if match else (None, None)


def registry(workspaces):
    """A workspace registry: {id: (root, resources or None[, other settings])}, each root a
    directory it creates."""
    entries = {}
    for name, value in workspaces.items():
        root, resources = value[0], value[1]
        Path(root).mkdir(parents=True, exist_ok=True)
        entry = {'root': str(root), 'profile': 'workspace-write'}
        if resources:
            entry['resources'] = resources
        if len(value) > 2:
            entry.update(value[2])
        entries[name] = entry
    return {'workspaces': entries}


def required(cpu_milli, memory_bytes, tasks, minimum=None):
    request = {'cpu_milli': cpu_milli, 'memory_bytes': memory_bytes, 'tasks': tasks}
    if minimum:
        request['minimum'] = minimum
    return {'participation': 'required', 'request': request}


def devguard_flags(mode, socket, credential_file, launch_helper=None, consumer=CONSUMER, generation=GENERATION):
    """The gateway's DevGuard settings. The credential file is named, never read here."""
    flags = ['--devguard', mode, '--devguard-socket', str(socket), '--devguard-consumer', consumer,
             '--devguard-generation', generation, '--devguard-credential-file', str(credential_file)]
    if launch_helper:
        flags += ['--devguard-launch-helper', str(launch_helper)]
    return flags


def runner_flags(mode, runtime_bin=None, runner_dir=None, runner_socket=None):
    if mode == 'in-process':
        return ['--runner', 'in-process']
    if runner_socket:
        return ['--runner', 'uds', '--runner-socket', str(runner_socket)]
    return ['--runner', 'uds', '--runtime-bin', str(runtime_bin), '--runner-dir', str(runner_dir)]


class Gateway:
    """A codespace-mcp process on stdio in its own session. Its log is kept as evidence, and the
    governed executions it reports are indexed by process ID, every report kept, so that a
    second launch of one execution shows. Every process it starts through `exec` or a resumed
    approval is tracked, so `settle` can end them before the gateway goes."""

    def __init__(self, binary, flags, env, log_path, record=None, cwd=None):
        self.argv = [str(binary), *flags]
        self.log_path = Path(log_path)
        self.executions = {}
        self.started = set()
        self.lock = threading.Lock()
        self.process = subprocess.Popen(self.argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, env=env, cwd=cwd, start_new_session=True)
        self.pid = self.process.pid
        self.log_reader = threading.Thread(target=self._log, daemon=True)
        self.log_reader.start()
        self.client = McpClient(self.process, record)
        self.info = None

    def _log(self):
        with open(self.log_path, 'ab') as log:
            for line in self.process.stderr:
                log.write(line)
                log.flush()
                entry = launch_line(line.decode('utf-8', 'replace'))
                if entry:
                    with self.lock:
                        self.executions.setdefault(entry['process_id'], []).append(entry)

    def initialize(self, timeout=60):
        result, latency = self.client.request('initialize', {
            'protocolVersion': MCP_PROTOCOL, 'capabilities': {},
            'clientInfo': {'name': 'codespace-qualify', 'version': '1'}}, timeout)
        self.client.notify('notifications/initialized')
        self.info = {'result': result, 'latency_ms': latency}
        return result

    def execution(self, process_id, limit=5.0):
        """What the gateway's log last said of a governed execution, waiting briefly for it."""
        deadline = time.monotonic() + limit
        while True:
            with self.lock:
                entries = self.executions.get(process_id)
            if entries or time.monotonic() >= deadline:
                return entries[-1] if entries else None
            time.sleep(0.02)

    def launches(self):
        """{process ID: number of launches the log reports} for every governed execution."""
        with self.lock:
            return {pid: sum(1 for entry in entries if entry.get('launched'))
                    for pid, entries in self.executions.items()}

    def attempts(self):
        """Every attempt the log names, launched or not."""
        with self.lock:
            return sorted({entry['attempt_id'] for entries in self.executions.values() for entry in entries})

    def call(self, name, arguments, timeout=CALL_TIMEOUT_S):
        return self.client.call(name, arguments, timeout)

    def exec(self, workspace, command, tty=False, timeout=CALL_TIMEOUT_S):
        """exec_command: the outcome (`started`, `refused` with CodeSpace's code, or `error` for the
        transport's), the process ID, the dispatch status, the attempt a refusal names and the
        round trip."""
        arguments = {'workspace_id': workspace, 'command': [str(part) for part in command]}
        if tty:
            arguments['tty'] = True
        try:
            payload, latency = self.client.call('exec_command', arguments, timeout)
        except ToolError as error:
            attempt, process = refusal_ids(error.message)
            return {'outcome': 'error' if error.transport else 'refused', 'code': error.code,
                    'message': error.message, 'latency_ms': error.latency_ms, 'attempt_id': attempt,
                    'process_id': process, 'approval_id': error.body.get('approval_id')}
        process_id = payload.get('process_id')
        with self.lock:
            self.started.add(process_id)
        return {'outcome': 'started', 'process_id': process_id, 'dispatch': payload.get('dispatch_status'),
                'latency_ms': latency}

    def resume(self, approval_id, timeout=CALL_TIMEOUT_S):
        """operation_resume of a granted approval; the execution it returns is tracked."""
        payload, latency = self.client.call('operation_resume', {'approval_id': approval_id}, timeout)
        process_id = (payload.get('exec_command') or {}).get('process_id')
        if process_id:
            with self.lock:
                self.started.add(process_id)
        return payload, latency

    def status(self, process_id, timeout=CALL_TIMEOUT_S):
        return self.client.call('process_status', {'process_id': process_id}, timeout)

    def wait_exited(self, process_id, limit=30.0, period=0.05):
        """The status once the process has exited, or None after `limit` seconds."""
        deadline = time.monotonic() + limit
        while True:
            payload, _ = self.status(process_id)
            if payload.get('state') == 'exited':
                return payload
            if time.monotonic() >= deadline:
                return None
            time.sleep(period)

    def read_until(self, process_id, wanted=None, limit=10.0, period=0.02, cursor=0, timeout=CALL_TIMEOUT_S):
        """Read output from `cursor` until `wanted` appears in it (None: until the output ends),
        the output ends or `limit` seconds pass: (text, next cursor, eof, found). A terminal's
        line ends are read as plain newlines."""
        deadline, text, eof = time.monotonic() + limit, '', False
        while True:
            payload, _ = self.client.call('read_process', {'process_id': process_id, 'cursor': cursor}, timeout)
            text += (payload.get('chunk') or '').replace('\r\n', '\n')
            cursor, eof = payload.get('cursor', cursor), bool(payload.get('eof'))
            found = wanted is not None and wanted in text
            if found or eof or time.monotonic() >= deadline:
                return text, cursor, eof, found
            time.sleep(period)

    def terminate(self, process_id, timeout=CALL_TIMEOUT_S):
        _, latency = self.client.call('terminate_process', {'process_id': process_id}, timeout)
        return latency

    def settle(self, limit=30.0):
        """End every process this harness started that still runs, and wait for each to exit."""
        with self.lock:
            started = sorted(pid for pid in self.started if pid)
        left = []
        for process_id in started:
            try:
                payload, _ = self.status(process_id, timeout=10)
                if payload.get('state') != 'exited':
                    self.terminate(process_id, timeout=10)
                    if self.wait_exited(process_id, limit=limit) is None:
                        left.append(process_id)
            except ToolError as error:
                if error.transport:
                    left.append(process_id)
        return left

    def close(self, limit=60):
        """Close the gateway's input, as a client that goes away does, and wait for it to exit; one
        that does not exit in time is stopped. Its log is complete afterwards."""
        try:
            self.process.stdin.close()
        except OSError:
            pass
        stopped = None
        try:
            self.process.wait(timeout=limit)
        except subprocess.TimeoutExpired:
            stopped = 'SIGTERM'
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                stopped = 'SIGKILL'
                self.process.kill()
                self.process.wait()
        self.log_reader.join(timeout=10)
        return {'returncode': self.process.returncode, 'stopped': stopped,
                'malformed_lines': self.client.malformed}


# ---------------------------------------------------------------- processes and descriptors

def group_alive(pgid):
    """Whether any process is still in the process group, a zombie included."""
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def wait_group_gone(pgid, limit=30.0, period=0.01):
    """Seconds until the group was observed empty, or None after `limit`."""
    began = time.monotonic()
    while group_alive(pgid):
        if time.monotonic() - began >= limit:
            return None
        time.sleep(period)
    return time.monotonic() - began


def descriptors(pid):
    """The numbered descriptors a process holds, from lsof, or None when they cannot be read."""
    text = run_text('lsof', '-nP', '-a', '-p', str(pid), '-d', '0-65535', '-F', 'f', timeout=30)
    if text is None:
        return None
    return sorted(int(line[1:]) for line in text.splitlines() if line.startswith('f') and line[1:].isdigit())


def children(pid):
    text = run_text('pgrep', '-P', str(pid), timeout=10) or ''
    return [int(value) for value in text.split()]


def process_footprint(pid):
    """RSS, threads and CPU share of one process, from ps; None when it is gone."""
    line = run_text('ps', '-o', 'rss=,%cpu=', '-p', str(pid), timeout=10)
    if not line:
        return None
    fields = line.split()
    if platform.system() == 'Darwin':
        listing = run_text('ps', '-M', '-p', str(pid), timeout=10)
        threads = max(0, len(listing.splitlines()) - 1) if listing else None
    else:
        text = run_text('ps', '-o', 'nlwp=', '-p', str(pid), timeout=10)
        threads = int(text) if text else None
    return {'pid': pid, 'rss_bytes': int(fields[0]) * 1024, 'cpu_percent': float(fields[1]), 'threads': threads}


def host_sample():
    """Memory pressure, swap and load, as DevGuard's measure.py samples them."""
    row = {'load_average': list(os.getloadavg())}
    if platform.system() == 'Darwin':
        swap = run_text('/usr/sbin/sysctl', '-n', 'vm.swapusage') or ''
        used = re.search(r'used = ([\d.]+)M', swap)
        row['pressure_level'] = run_text('/usr/sbin/sysctl', '-n', 'kern.memorystatus_vm_pressure_level')
        row['swap_used_mib'] = float(used[1]) if used else None
    return row


def host_facts():
    """The host's identity and condition for the candidate record."""
    def sysctl(name):
        return run_text('/usr/sbin/sysctl', '-n', name)
    facts = {'platform': platform.platform(), 'machine': platform.machine(), 'kernel': platform.release(),
             'python': platform.python_version()}
    if platform.system() == 'Darwin':
        facts.update({'macos': platform.mac_ver()[0], 'build': run_text('/usr/bin/sw_vers', '-buildVersion'),
                      'model': sysctl('hw.model'), 'cpu': sysctl('machdep.cpu.brand_string'),
                      'logical_cpus': sysctl('hw.logicalcpu'), 'memory_bytes': sysctl('hw.memsize'),
                      'boot_session': sysctl('kern.bootsessionuuid'),
                      'power': (run_text('/usr/bin/pmset', '-g', 'batt') or '').splitlines()[:1]})
    else:
        facts.update({'logical_cpus': str(os.cpu_count())})
    return facts


# ---------------------------------------------------------------- the installed DevGuard service

class Release:
    """An installed DevGuard release as the service runs it: its own directory, or its recovery
    copy (DevGuard's measure.py finds it the same way)."""

    def __init__(self, release_id, support=SUPPORT):
        self.id = release_id
        directory = support / 'releases' / release_id
        for candidate in (directory, support / 'recovery' / release_id):
            status = run_json(candidate / 'bin/devguardd', 'status')
            executable = Path(((status or {}).get('running') or {}).get('executable') or '')
            if executable.name == 'devguardd' and executable.parent.parent.name == release_id:
                directory = executable.parent.parent
                break
        self.directory = directory
        self.manifest = directory / 'MANIFEST.json'
        self.devguardd = directory / 'bin/devguardd'
        self.devguard = directory / 'bin/devguard'
        self.helper = directory / 'bin/devguard-launch'

    def state(self):
        """Whether the service runs this release, and its process."""
        manifest = json.loads(self.manifest.read_text())
        status = run_json(self.devguardd, 'status') or {}
        running = status.get('running') or {}
        current = (status.get('selection') or {}).get('current')
        return {'healthy': bool(status.get('healthy')), 'current': current,
                'running_sha256': running.get('sha256'), 'service_pid': running.get('service_pid'),
                'runs_release': bool(status.get('healthy') and current == self.id
                                     and running.get('sha256') == manifest['artifacts']['devguardd']['sha256'])}

    def doctor(self):
        report = run_json(self.devguard, 'doctor', '--require', 'admission,registration,macos-cooperative') or {}
        service = ((report.get('checks') or {}).get('service') or {}).get('detail') or {}
        return {'satisfied': report.get('satisfied'), 'requirements': report.get('requirements'),
                'protocol': service.get('protocol'), 'capabilities': service.get('capabilities'),
                'configuration_fingerprint': (service.get('status') or {}).get('configuration_fingerprint'),
                'authority_pid': (service.get('authority') or {}).get('pid')}

    def artifacts(self):
        """The manifest's artifacts and whether each installed binary still matches it."""
        manifest = json.loads(self.manifest.read_text())
        found = {}
        for name, entry in sorted(manifest.get('artifacts', {}).items()):
            path = self.directory / 'bin' / name
            found[name] = {'sha256': entry.get('sha256'),
                           'matches': path.is_file() and sha256(path) == entry.get('sha256')}
        return {'manifest_sha256': sha256(self.manifest), 'source': manifest.get('source'), 'artifacts': found}


def service_events(path=SERVICE_LOG, offset=0, kinds=None):
    """The service's JSON receipts from `offset` on, optionally only those of the given kinds."""
    events = []
    try:
        with open(path, 'rb') as log:
            log.seek(offset)
            for raw in log:
                start = raw.find(b'{')
                if start < 0:
                    continue
                try:
                    event = json.loads(raw[start:])
                except ValueError:
                    continue
                if isinstance(event, dict) and (kinds is None or event.get('event') in kinds):
                    events.append(event)
    except OSError:
        return []
    return events


def log_size(path=SERVICE_LOG):
    try:
        return Path(path).stat().st_size
    except OSError:
        return None


def pressure_state(path=SERVICE_LOG, window=4 << 20):
    """The service's latest pressure state, from its receipts."""
    size = log_size(path) or 0
    events = service_events(path, max(0, size - window), ('pressure', 'pressure_control_lag'))
    return events[-1].get('state') if events else None


def work_capacity(path=SERVICE_LOG):
    """The work capacity the service reported when it last started."""
    events = service_events(path, 0, ('native_host',))
    return events[-1].get('work_capacity') if events else None


def journal_query(query, parameters=(), database=None):
    connection = sqlite3.connect(f'file:{database or JOURNAL}?mode=ro', uri=True, timeout=5)
    try:
        return connection.execute(query, parameters).fetchall()
    finally:
        connection.close()


def attempt_view(record):
    """What the checks need from one attempt record; never its permit hash or lease ID."""
    reservation = record.get('reservation') or {}
    plan = record.get('plan') or {}
    applied = record.get('applied') or {}
    scope = record.get('scope') or {}
    return {'phase': record.get('phase'), 'reserved': reservation.get('quantities'),
            'plan': {name: (plan.get(name) or {}).get('level') for name in ('cpu', 'memory', 'pids')} if plan else None,
            'applied': {name: applied.get(name) for name in ('cpu', 'memory', 'pids')} if applied else None,
            'applied_quantities': applied.get('quantities'), 'scope_kind': scope.get('kind'),
            'scope_root_pid': (scope.get('root') or {}).get('pid'), 'denial': record.get('denial'),
            'tracking_lost': record.get('tracking_lost'), 'release_reason': record.get('release_reason'),
            'owner_pid': ((record.get('owner') or {}).get('process') or {}).get('pid'),
            'policy_revision': record.get('policy_revision')}


def journal_attempts(attempts=None, database=None, consumer=CONSUMER):
    """A consumer's attempts in DevGuard's journal, read-only: all of them, or the given IDs."""
    query = ('SELECT attempt, generation, charged, launch_hash IS NOT NULL, record FROM attempts '
             'WHERE consumer = ?')
    rows = []
    if attempts is None:
        rows = journal_query(query, (consumer,), database)
    else:
        ids = sorted(set(attempts))
        for start in range(0, len(ids), 500):
            chunk = ids[start:start + 500]
            rows += journal_query(f"{query} AND attempt IN ({','.join('?' * len(chunk))})", (consumer, *chunk),
                                  database)
    found = {}
    for attempt, generation, charged, committed, record in rows:
        view = attempt_view(json.loads(record))
        # A committed grant later released as never started (no helper existed) did not run.
        launched = bool(committed) and not (view['phase'] == 'released'
                                            and view['release_reason'] == 'no_helper_created')
        found[attempt] = dict(view, generation=generation, charged=bool(charged), launched=launched)
    return found


def journal_charged(database=None):
    """What still holds resources, by consumer."""
    attempts = journal_query('SELECT consumer, COUNT(*) FROM attempts WHERE charged = 1 GROUP BY consumer',
                             (), database)
    leases = journal_query('SELECT COUNT(*) FROM leases WHERE charged = 1', (), database)[0][0]
    return {'attempts': {consumer: count for consumer, count in attempts}, 'leases': leases}


def journal_instances(consumer=CONSUMER, database=None):
    rows = journal_query('SELECT generation, instance, state, identity FROM instances WHERE consumer = ?',
                         (consumer,), database)
    found = []
    for generation, instance, state, identity in rows:
        process = (json.loads(identity) or {}).get('process') or {}
        found.append({'generation': generation, 'instance': instance, 'state': state, 'pid': process.get('pid')})
    return found


def settle_attempts(ids, database=None, limit=60.0, consumer=CONSUMER):
    """Wait until none of `ids` is charged: their views and those still charged."""
    deadline = time.monotonic() + limit
    while True:
        found = journal_attempts(ids, database, consumer)
        charged = sorted(attempt for attempt, view in found.items() if view['charged'])
        if not charged or time.monotonic() >= deadline:
            return found, charged
        time.sleep(0.5)


# ---------------------------------------------------------------- DevGuard's fixture authority

class FixtureAuthority:
    """DevGuard's fixture authority at CodeSpace's pin (crates/devguard/examples/fixture_authority.rs)
    in its own process and session, under a private base directory. It provisions a `codespace`
    consumer itself; its credential stays in its file, whose path the gateway is given."""

    def __init__(self, binary, base, env, log_path, limit=2, timeout=60):
        self.base = Path(base)
        self.base.mkdir(parents=True, mode=0o700, exist_ok=True)
        self.log = open(log_path, 'ab')
        self.process = subprocess.Popen([str(binary), str(base), str(limit)], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.log, env=env, text=True,
                                        start_new_session=True)
        self.lines = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.settings = self._answer(timeout)
        if self.settings is None:
            self.kill()
            raise RuntimeError('the fixture authority did not start')

    def _read(self):
        for line in self.process.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def _answer(self, timeout):
        try:
            line = self.lines.get(timeout=timeout)
        except queue.Empty:
            return None
        return json.loads(line) if line else None

    def command(self, line, timeout=60):
        self.process.stdin.write(line + '\n')
        self.process.stdin.flush()
        return self._answer(timeout)

    def charged(self):
        return self.command('charged')

    def instances(self):
        return self.command('instances')

    def journal(self):
        """The fixture's journal, which the harness reads read-only as the installed service's."""
        return next(iter(sorted(self.base.rglob('authority.sqlite'))), None)

    def kill(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()

    def stop(self, limit=30):
        try:
            self.process.stdin.close()
        except OSError:
            pass
        try:
            self.process.wait(timeout=limit)
        except subprocess.TimeoutExpired:
            self.kill()
        self.log.close()
        return self.process.returncode

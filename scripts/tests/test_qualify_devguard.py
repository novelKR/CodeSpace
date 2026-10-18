"""The CSRG-U6 and P1R-C06 harness without a host: its analysis, evidence, journal reads, MCP and
Runner clients and verdict recomputation, on synthetic inputs (docs/cs-rg-qualification.md,
docs/recovery-qualification.md)."""
import contextlib
import importlib.util
import io
import json
import math
import os
from pathlib import Path
import resource
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
from types import SimpleNamespace
import unittest
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import qualify_analysis as analysis  # noqa: E402
import qualify_host as host  # noqa: E402
spec = importlib.util.spec_from_file_location('qualify', SCRIPTS / 'qualify-devguard.py')
qualify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualify)

GIT_ENV = dict(os.environ, GIT_AUTHOR_NAME='t', GIT_AUTHOR_EMAIL='t@example.invalid',
               GIT_COMMITTER_NAME='t', GIT_COMMITTER_EMAIL='t@example.invalid',
               GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1')
RESERVATION = {'cpu_milli': 150, 'memory_bytes': 536870912, 'tasks': 40}


def answered(count, latency=5.0, state='running'):
    return [{'slot': n, 'outcome': 'answered', 'latency_ms': latency, 'state': state} for n in range(count)]


def terminated(count, ack=20.0, released=300.0):
    rows = [{'slot': n, 'outcome': 'acknowledged', 'ack_ms': ack, 'termination': 'terminated',
             'exited_ms': ack + 10, 'scope_exit_ms': ack + 15, 'attempt_id': f'cs-attempt-{n:02x}'}
            for n in range(count)]
    releases = [{'slot': n, 'attempt_id': f'cs-attempt-{n:02x}', 'released_ms': released, 'phase': 'released',
                 'release_reason': 'scope_terminated'} for n in range(count)]
    return rows, releases


def echoed(count, latency=3.0):
    return [{'slot': n, 'outcome': 'echoed', 'latency_ms': latency, 'eof': False} for n in range(count)]


def refused(count, code='RESOURCE_UNAVAILABLE', latency=40.0):
    return [{'slot': n, 'outcome': 'refused', 'code': code, 'latency_ms': latency} for n in range(count)]


def admitted(count, latency=60.0):
    return [{'slot': n, 'outcome': 'admitted', 'latency_ms': latency, 'attempt_id': f'cs-attempt-a{n}'}
            for n in range(count)]


def stream(label, first, last, per_read=3, cursor=0):
    """Reads of a numbered output stream, `per_read` lines each, as the measurement records them."""
    rows = []
    for start in range(first, last + 1, per_read):
        chunk = ''.join(f'{label} {n:08d}\n' for n in range(start, min(start + per_read, last + 1)))
        rows.append({'slot': len(rows), 'outcome': 'read', 'from': cursor, 'to': cursor + len(chunk), 'chunk': chunk,
                     'output_lost': False, 'retained_from': cursor, 'lost_reason': None})
        cursor += len(chunk)
    return rows


def lost(row, retained_from, reason='ring_overflow'):
    """A read that asked from before what the Runner still retains."""
    return dict(row, output_lost=True, retained_from=retained_from, lost_reason=reason)


def cycles(count):
    """A recovery interval's gateway cycles: each claimed under its epoch, inherited its targets,
    saw every fence request refused and a rival give up; all but the last restart, ending in turn
    as GATEWAY_ENDS says; then the last gateway's end."""
    rows = []
    for number in range(1, count + 1):
        row = {'cycle': number, 'gateway': 100 + number, 'expected_epoch': number, 'serving_ms': 30.0,
               'restart': number < count,
               'claim': {'granted': True, 'pid': 100 + number, 'epoch': number, 'previous_controller': number > 1},
               'fence': {'claim': 'controller_active', 'requests': [
                   {'op': op, 'epoch': epoch, 'ok': False, 'code': 'STALE_CONTROLLER'}
                   for op in ('write_stdin', 'terminate', 'stop') for epoch in (number - 1, number, None)]},
               'rival': {'pid': 200 + number, 'returncode': 1, 'killed': False, 'another_writer': True, 'claims': []}}
        if number > 1:
            row['inherited'] = {'status-pipe': 'running', 'beat-pty': 'running'}
        if row['restart']:
            how = host.GATEWAY_ENDS[number % len(host.GATEWAY_ENDS)]
            row.update(how=how, ended={'how': how, 'returncode': analysis.GATEWAY_EXIT[how], 'stopped': None})
        rows.append(row)
    rows.append({'cycle': count, 'gateway': 100 + count, 'last': True, 'how': 'eof',
                 'ended': {'how': 'eof', 'returncode': 0, 'stopped': None}})
    return rows


def timed_out(deadline_s, gone_s, in_gap=False, **changes):
    deadline = int(deadline_s * 1000)
    return dict({'tag': 'timeout', 'deadline_at_ms': deadline, 'deadline_after_ms': deadline, 'gone_unix': gone_s,
                 'termination': 'timeout', 'in_gap': in_gap}, **changes)


def resumed(tag, process_id, killed=False, **changes):
    resume = {'outcome': 'started', 'process_id': process_id}
    return dict({'tag': tag, 'resume': resume, 'again': dict(resume), 'ready_ms': 12.0, 'killed_during_resume': killed,
                 'not_admitted': False}, **changes)


class AnalysisTests(unittest.TestCase):
    def test_a_missing_sample_ranks_above_every_value(self):
        self.assertEqual(analysis.nearest_rank([10, 20], 0.99, missing=1), math.inf)
        self.assertEqual(analysis.number(math.inf), 'missing')
        self.assertEqual(analysis.nearest_rank([30, 10, 20], 0.5), 20)
        self.assertIsNone(analysis.nearest_rank([], 0.99))

    def test_status_target_is_the_99th_percentile(self):
        fast = answered(99, 100.0) + answered(1, 600.0)
        self.assertTrue(analysis.status_metrics(fast, 100)['passed'])
        slow = answered(98, 100.0) + answered(2, 600.0)
        self.assertFalse(analysis.status_metrics(slow, 100)['passed'])

    def test_an_answer_about_an_ended_target_is_missing(self):
        metrics = analysis.status_metrics(answered(99) + answered(1, state='exited'), 100)
        self.assertEqual(metrics['not_running'], 1)
        self.assertEqual(metrics['p99_ms'], 5.0)
        metrics = analysis.status_metrics(answered(98) + answered(2, state='exited'), 100)
        self.assertEqual(metrics['p99_ms'], 'missing')
        self.assertFalse(metrics['passed'])

    def test_a_lost_connection_fails_whatever_the_latency(self):
        rows = answered(200) + [{'slot': 200, 'outcome': 'error', 'code': analysis.TRANSPORT}]
        self.assertFalse(analysis.status_metrics(rows, 201)['passed'])
        self.assertTrue(analysis.connection_lost([], rows))

    def test_terminations(self):
        rows, releases = terminated(10)
        joined = analysis.with_releases(rows, releases)
        self.assertTrue(analysis.termination_metrics(joined, 10)['passed'])
        ineffective = joined[:9] + [dict(joined[9], termination='exited')]
        self.assertFalse(analysis.termination_metrics(ineffective, 10)['passed'])
        unreleased = analysis.with_releases(rows, releases[:9])
        self.assertFalse(analysis.termination_metrics(unreleased, 10)['passed'])
        reported = analysis.termination_metrics(unreleased, 10, release_expected=False)
        self.assertTrue(reported['passed'])
        self.assertEqual(reported['unreleased'], 1)
        refused_for_capacity = joined + [{'slot': 10, 'outcome': 'not_started', 'code': 'RESOURCE_UNAVAILABLE'}]
        metrics = analysis.termination_metrics(refused_for_capacity, 11)
        self.assertEqual((metrics['samples'], metrics['not_started']), (10, 1))

    def test_control_metrics_counts_both_status_targets(self):
        rows, releases = terminated(3)
        metrics = analysis.control_metrics(
            {'status_pipe': answered(60), 'status_pty': answered(60), 'terminations': rows, 'releases': releases,
             'io': echoed(12), 'refusals': refused(2)}, 60.4, qualify.PERIODS)
        self.assertEqual((metrics['status']['samples'], metrics['status']['expected']), (120, 120))
        self.assertEqual(metrics['termination']['expected'], 3)
        self.assertEqual(metrics['termination']['release_p99_ms'], 300.0)
        self.assertEqual(metrics['io']['expected'], 12)
        self.assertEqual(metrics['refusal']['codes'], {'RESOURCE_UNAVAILABLE': 2})
        self.assertFalse(metrics['connection_lost'])

    def test_io_and_refusals(self):
        self.assertFalse(analysis.io_metrics(echoed(4) + [{'slot': 4, 'outcome': 'not_echoed'}], 5)['passed'])
        self.assertTrue(analysis.io_metrics(echoed(5), 5)['passed'])
        self.assertIsNone(analysis.refusal_metrics([])['passed'])
        metrics = analysis.refusal_metrics(refused(2) + admitted(1))
        self.assertEqual((metrics['refused'], metrics['admitted'], metrics['passed']), (2, 1, True))

    def interval(self, **changes):
        rows, releases = terminated(30)
        metrics = analysis.control_metrics(
            {'status_pipe': answered(600), 'status_pty': answered(600), 'terminations': rows, 'releases': releases,
             'io': echoed(120), 'refusals': refused(20)}, 600.2, qualify.PERIODS)
        metrics.update(service={'rows': 10, 'failure': None}, journal={'still_charged': [], 'duplicate_launches': 0})
        metrics['termination']['status_target_effective'] = True
        for key, value in changes.items():
            metrics[key] = value
        return metrics

    def test_interval_verdicts(self):
        self.assertEqual(analysis.interval_verdict('idle', [], self.interval()), ('pass', []))
        self.assertEqual(analysis.interval_verdict('idle', ['the journal could not be read'], self.interval())[0],
                         'inconclusive')
        failing = self.interval(service={'failure': 'the release restarted or was unhealthy during the interval'})
        self.assertEqual(analysis.interval_verdict('idle', [], failing)[0], 'fail')
        duplicate = self.interval(journal={'duplicate_launches': 1})
        self.assertIn('an execution was launched more than once', analysis.interval_verdict('idle', [], duplicate)[1])
        short = self.interval()
        short['status'] = analysis.status_metrics(answered(1000), 1200)
        self.assertEqual(analysis.interval_verdict('idle', [], short)[0], 'inconclusive')
        target = self.interval()
        target['termination'] = dict(target['termination'], status_target_effective=False)
        self.assertEqual(analysis.interval_verdict('idle', [], target)[0], 'fail')

    def test_a_failing_baseline_is_inconclusive_and_repetitions_are_not_pooled(self):
        verdict, _ = analysis.repetition_verdict({'verdict': 'fail', 'reasons': ['x']}, {'verdict': 'pass', 'reasons': []})
        self.assertEqual(verdict, 'inconclusive')
        self.assertEqual(analysis.combination_verdict(['pass', 'pass', 'pass'], 3), 'qualified')
        self.assertEqual(analysis.combination_verdict(['pass', 'pass'], 3), 'inconclusive')
        self.assertEqual(analysis.combination_verdict(['pass', 'fail', 'pass'], 3), 'failed')
        plan = dict(qualify.CONTROL_PROTOCOL)
        qualified = {mode: {'verdict': 'qualified'} for mode in plan['combinations']}
        self.assertEqual(analysis.overall_verdict(qualified, plan, qualify.CONTROL_PROTOCOL, False), 'qualified')
        self.assertEqual(analysis.overall_verdict(qualified, plan, qualify.CONTROL_PROTOCOL, True), 'inconclusive')
        one = {'uds': {'verdict': 'qualified'}}
        self.assertEqual(analysis.overall_verdict(one, dict(plan, combinations=['uds']), qualify.CONTROL_PROTOCOL,
                                                  False), 'inconclusive')

    def test_scenarios_and_suites(self):
        check = analysis.check
        self.assertEqual(analysis.scenario_verdict([check('a', True), check('b', True)]), 'pass')
        self.assertEqual(analysis.scenario_verdict([check('a', True), check('b', None)]), 'inconclusive')
        self.assertEqual(analysis.scenario_verdict([check('a', True), check('b', False)]), 'fail')
        self.assertEqual(analysis.scenario_verdict([]), 'inconclusive')
        self.assertEqual(analysis.suite_verdict(['pass', 'not_run']), 'passed')
        self.assertEqual(analysis.suite_verdict(['pass', 'inconclusive']), 'incomplete')
        self.assertEqual(analysis.suite_verdict(['pass', 'fail']), 'failed')
        self.assertEqual(analysis.suite_verdict([]), 'incomplete')

    def test_load_shares_and_failures(self):
        self.assertEqual(analysis.covered([(0, 4), (2, 6), (8, 9)], 0, 10), 0.7)
        runs = [{'consumer': 'cargo', 'result': 'completed', 'exit_code': 0, 'running_from': 0, 'ended': 8},
                {'consumer': 'tests', 'result': 'uncertain', 'running_from': 1, 'ended': 2}]
        metrics = analysis.load_metrics(runs, 0, 10, ('cargo', 'tests', 'idle'))
        self.assertEqual(metrics['applied_fraction'], 0.8)
        self.assertEqual(metrics['failures'], ['1 load runs are uncertain'])
        self.assertIn('idle: no successful run', metrics['issues'])
        self.assertIn('tests: ran for 10% of the interval', metrics['issues'])

    def test_service_findings(self):
        good = [{'runs_release': True, 'current': 'r1', 'service_pid': 7}]
        self.assertEqual(analysis.service_findings(good, 'r1', 7), ([], None))
        self.assertEqual(analysis.service_findings(good, 'r1', 8)[1],
                         'the release restarted or was unhealthy during the interval')
        self.assertEqual(analysis.service_findings([{'current': 'r2'}], 'r1', 7)[0],
                         ['another release was selected during the interval'])
        self.assertEqual(analysis.service_findings([], 'r1', 7)[0], ['the service was not observed'])


LAUNCHED = ('2026-10-12T16:00:00.000001Z  INFO codespace_server::devguard: DevGuard managed launch '
            'attempt_id=cs-attempt-1f2e process_id=proc-7c1d dispatch=Confirmed phases=[Prepared, Committed] '
            'applied=Some(Applied) scope_root_pid=Some(4242)')
NOT_STARTED = ('2026-10-12T16:00:01.000001Z  INFO codespace_server::devguard: DevGuard managed launch; the '
               'command was not started attempt_id=cs-attempt-3a4b process_id=proc-9e8f outcome=ExecFailed attempt=x')


class HostTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.dir = Path(self.scratch.name)

    def tearDown(self):
        for path in self.dir.rglob('*'):
            os.chmod(path, 0o700 if path.is_dir() else 0o600)
        self.scratch.cleanup()

    def test_launch_lines(self):
        self.assertEqual(host.launch_line(LAUNCHED), {'attempt_id': 'cs-attempt-1f2e', 'process_id': 'proc-7c1d',
                                                      'launched': True, 'scope_root_pid': 4242,
                                                      'dispatch': 'Confirmed'})
        self.assertEqual(host.launch_line(NOT_STARTED), {'attempt_id': 'cs-attempt-3a4b', 'process_id': 'proc-9e8f',
                                                         'launched': False, 'outcome': 'ExecFailed'})
        self.assertIsNone(host.launch_line('INFO codespace_server: started'))

    def test_refusal_ids(self):
        message = 'attempt `cs-attempt-00ff` for process_id `proc-1`: denied; nothing was started'
        self.assertEqual(host.refusal_ids(message), ('cs-attempt-00ff', 'proc-1'))
        self.assertEqual(host.refusal_ids('busy'), (None, None))

    def test_registry_and_flags(self):
        spaces = {'gov': (self.dir / 'ws/gov', host.required(25, 16 << 20, 2)),
                  'off': (self.dir / 'ws/off', None),
                  'confirm': (self.dir / 'ws/confirm', host.required(25, 16 << 20, 2), {'approvals': 'confirm'})}
        registry = host.registry(spaces)['workspaces']
        self.assertTrue((self.dir / 'ws/gov').is_dir())
        self.assertEqual(registry['gov']['resources'], {'participation': 'required', 'request': {
            'cpu_milli': 25, 'memory_bytes': 16 << 20, 'tasks': 2}})
        self.assertNotIn('resources', registry['off'])
        self.assertEqual(registry['confirm']['approvals'], 'confirm')
        flags = host.devguard_flags('register', '/s.sock', '/c.secret', '/bin/helper')
        self.assertEqual(flags[flags.index('--devguard-credential-file') + 1], '/c.secret')
        self.assertEqual(flags[-2:], ['--devguard-launch-helper', '/bin/helper'])
        self.assertEqual(host.runner_flags('in-process'), ['--runner', 'in-process'])
        self.assertIn('--runner-socket', host.runner_flags('uds', runner_socket='/w.sock'))

    def test_sealed_evidence_shows_every_change(self):
        evidence = host.Evidence(self.dir / 'run')
        evidence.write_json('a/report.json', {'verdict': 'pass'})
        evidence.append('a/rows.jsonl', {'slot': 0})
        manifest = evidence.seal()
        self.assertEqual(evidence.seal(), manifest)
        self.assertEqual(host.verify_manifest(evidence.path), [])
        os.chmod(evidence.path / 'a/rows.jsonl', 0o600)
        (evidence.path / 'a/rows.jsonl').write_text('{"slot": 1}\n')
        os.chmod(evidence.path / 'a', 0o700)
        (evidence.path / 'a/extra.json').write_text('{}\n')
        (evidence.path / 'a/report.json').unlink()
        self.assertEqual(host.verify_manifest(evidence.path),
                         ['missing a/report.json', 'not listed a/extra.json', 'changed a/rows.jsonl'])
        with self.assertRaises(SystemExit):
            host.Evidence(self.dir / 'run')

    def test_writer(self):
        writer = host.Writer()
        for n in range(100):
            writer.write(self.dir / 'w/rows.jsonl', {'n': n})
        report = writer.close()
        self.assertEqual((report['lines'], report['error_count']), (100, 0))
        self.assertEqual(writer.close(), report)
        lines = (self.dir / 'w/rows.jsonl').read_text().splitlines()
        self.assertEqual([json.loads(line)['n'] for line in lines], list(range(100)))

    def test_the_writer_keeps_few_files_open(self):
        writer = host.Writer()
        paths = [self.dir / f'w/{n:02d}.jsonl' for n in range(3 * host.WRITER_OPEN_FILES)]
        for number in range(4):
            for path in paths:
                writer.write(path, {'round': number})
        deadline = time.monotonic() + 30
        while writer.written < 4 * len(paths) and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertLessEqual(len(writer.files), host.WRITER_OPEN_FILES)
        report = writer.close()
        self.assertEqual((report['lines'], report['error_count']), (4 * len(paths), 0))
        for path in paths:
            self.assertEqual([json.loads(line)['round'] for line in path.read_text().splitlines()], [0, 1, 2, 3])

    def journal(self):
        database = self.dir / 'authority.sqlite'
        connection = sqlite3.connect(database)
        connection.executescript("""
            CREATE TABLE attempts (attempt TEXT, consumer TEXT, generation TEXT, charged INTEGER, launch_hash TEXT,
                                   record TEXT);
            CREATE TABLE leases (lease TEXT, charged INTEGER);
            CREATE TABLE instances (consumer TEXT, generation TEXT, instance TEXT, state TEXT, identity TEXT);
        """)

        def record(phase, reason=None, root=None):
            return json.dumps({
                'phase': phase, 'reservation': {'quantities': {'cpu_milli': 25, 'memory_bytes': 1, 'tasks': 2}},
                'applied': {'cpu': 'applied', 'memory': 'applied', 'pids': 'applied',
                            'quantities': {'cpu_milli': 25, 'memory_bytes': 1, 'tasks': 2}},
                'scope': {'kind': 'process_group', 'root': {'pid': root}}, 'tracking_lost': False,
                'release_reason': reason, 'owner': {'process': {'pid': 7}}, 'permit_sha256': 'f' * 64,
                'lease_id': 'lease-secret'})
        rows = [('cs-attempt-1', 'codespace', 'g1', 0, 'h', record('released', 'scope_terminated', 99)),
                ('cs-attempt-2', 'codespace', 'g1', 1, 'h', record('suspect', None, 98)),
                ('cs-attempt-3', 'codespace', 'g1', 0, 'h', record('released', 'no_helper_created')),
                ('cs-attempt-4', 'codespace', 'g1', 0, None, record('denied')),
                ('cs-attempt-5', 'dev-cli', 'g0', 1, 'h', record('committed', None, 97))]
        connection.executemany('INSERT INTO attempts VALUES (?, ?, ?, ?, ?, ?)', rows)
        connection.execute("INSERT INTO leases VALUES ('lease-secret', 1)")
        connection.execute("INSERT INTO instances VALUES ('codespace', 'g1', 'i1', 'active', ?)",
                           (json.dumps({'process': {'pid': 7}}),))
        connection.commit()
        connection.close()
        return database

    def test_the_journal_is_read_without_permits_or_leases(self):
        database = self.journal()
        found = host.journal_attempts(None, database)
        self.assertEqual(sorted(found), ['cs-attempt-1', 'cs-attempt-2', 'cs-attempt-3', 'cs-attempt-4'])
        self.assertNotIn('secret', json.dumps(found))
        self.assertNotIn('f' * 64, json.dumps(found))
        self.assertTrue(found['cs-attempt-1']['launched'])
        self.assertFalse(found['cs-attempt-3']['launched'])
        self.assertFalse(found['cs-attempt-4']['launched'])
        self.assertEqual(found['cs-attempt-1']['scope_root_pid'], 99)
        self.assertEqual(sorted(host.journal_attempts(['cs-attempt-2', 'cs-attempt-5'], database)), ['cs-attempt-2'])
        self.assertEqual(host.journal_charged(database), {'attempts': {'codespace': 1, 'dev-cli': 1}, 'leases': 1})
        self.assertEqual(host.journal_instances(database=database),
                         [{'generation': 'g1', 'instance': 'i1', 'state': 'active', 'pid': 7}])
        found, charged = host.settle_attempts(['cs-attempt-1', 'cs-attempt-2'], database, limit=0)
        self.assertEqual(charged, ['cs-attempt-2'])


FAKE_SERVER = textwrap.dedent("""
    import json, sys, time
    def send(message):
        sys.stdout.write(json.dumps(message) + '\\n')
        sys.stdout.flush()
    def text(value, error=False):
        return {'content': [{'type': 'text', 'text': json.dumps(value)}], 'isError': error}
    for line in sys.stdin:
        message = json.loads(line)
        if 'method' not in message:
            continue
        name = message.get('params', {}).get('name')
        if message['method'] == 'initialize':
            send({'jsonrpc': '2.0', 'id': message['id'], 'result': {'protocolVersion': '2025-11-25'}})
        elif name == 'echo':
            send({'jsonrpc': '2.0', 'id': message['id'],
                  'result': dict(text({}), structuredContent=message['params']['arguments'])})
        elif name == 'exec_command' and message['params']['arguments']['command'] == ['busy']:
            send({'jsonrpc': '2.0', 'id': message['id'], 'result': text(
                {'code': 'WORKSPACE_BUSY', 'message': 'attempt `cs-attempt-0a` for process_id `proc-2`: busy',
                 'approval_id': 'appr-1'}, error=True)})
        elif name == 'ping-me':
            send({'jsonrpc': '2.0', 'id': 'server-1', 'method': 'ping'})
            reply = json.loads(sys.stdin.readline())
            send({'jsonrpc': '2.0', 'id': message['id'], 'result': text({'pinged': reply.get('result') == {}})})
        elif name == 'silent':
            pass
        elif name == 'exit':
            sys.exit(0)
""")


class McpClientTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        server = Path(self.scratch.name) / 'server.py'
        server.write_text(FAKE_SERVER)
        self.rows = []
        self.gateway = host.Gateway(sys.executable, [str(server)], dict(os.environ), Path(self.scratch.name) / 'gw.log',
                                    record=self.rows.append)
        self.gateway.initialize(timeout=30)

    def tearDown(self):
        self.gateway.close(limit=10)
        self.scratch.cleanup()

    def test_answers_errors_and_server_requests(self):
        payload, latency = self.gateway.call('echo', {'workspace_id': 'off'})
        self.assertEqual(payload, {'workspace_id': 'off'})
        self.assertGreaterEqual(latency, 0)
        refused = self.gateway.exec('off', ['busy'])
        self.assertEqual((refused['outcome'], refused['code'], refused['attempt_id'], refused['process_id'],
                          refused['approval_id']), ('refused', 'WORKSPACE_BUSY', 'cs-attempt-0a', 'proc-2', 'appr-1'))
        self.assertEqual(self.gateway.call('ping-me', {})[0], {'pinged': True})
        self.assertTrue(any(row['dir'] == 'in' for row in self.rows))

    def test_no_answer_and_a_lost_connection(self):
        with self.assertRaises(host.ToolError) as raised:
            self.gateway.call('silent', {}, timeout=0.3)
        self.assertEqual(raised.exception.code, host.NO_ANSWER)
        result = self.gateway.exec('off', ['true'], timeout=0.3)
        self.assertEqual((result['outcome'], result['code']), ('error', host.NO_ANSWER))
        with self.assertRaises(host.ToolError) as raised:
            self.gateway.call('exit', {}, timeout=10)
        self.assertEqual(raised.exception.code, analysis.TRANSPORT)
        with self.assertRaises(host.ToolError) as raised:
            self.gateway.call('echo', {}, timeout=10)
        self.assertTrue(raised.exception.transport)


FAKE_FIXTURE = textwrap.dedent("""
    import json, sys
    print(json.dumps({'base': sys.argv[1]}), flush=True)
    for line in sys.stdin:
        print(json.dumps({'command': line.strip()}), flush=True)
""")


def open_descriptors(expected=None, limit=5.0):
    """The test process's open descriptors; with `expected`, once they are that many or `limit`
    seconds have passed, so that another test's thread that is still ending cannot make a leak
    seem to come or go."""
    deadline = time.monotonic() + limit
    while True:
        count = len(os.listdir('/dev/fd'))
        if expected is None or count == expected or time.monotonic() >= deadline:
            return count
        time.sleep(0.05)


class DescriptorTests(unittest.TestCase):
    """Gateways and fixture authorities that ended leave none of their pipes open in the harness,
    which starts hundreds of them in one run under a terminal's limit of 256 open files and keeps
    each one referenced until the run settles."""

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.dir = Path(self.scratch.name)
        self.server = self.dir / 'server.py'
        self.server.write_text(FAKE_SERVER)

    def tearDown(self):
        self.scratch.cleanup()

    def gateway(self, name):
        gateway = host.Gateway(sys.executable, [str(self.server)], dict(os.environ), self.dir / f'{name}.log')
        gateway.initialize(timeout=30)
        return gateway

    def test_ended_gateways_close_their_pipes(self):
        started = [self.gateway('first')]
        started[0].close(limit=10)
        before = open_descriptors()
        for _ in range(3):
            for how in host.GATEWAY_ENDS:
                started.append(self.gateway(how))
                self.assertIsNone(host.end_gateway(started[-1], how, limit=10)['stopped'])
            started.append(self.gateway('killed'))
            started[-1].process.kill()
            started[-1].process.wait()
            started[-1].join_readers()
            started.append(self.gateway('closed'))
            started[-1].close(limit=10)
        self.assertEqual(open_descriptors(before), before)

    def test_stopped_fixture_authorities_close_their_pipes(self):
        binary = self.dir / 'fixture'
        binary.write_text(f'#!{sys.executable}\n' + FAKE_FIXTURE)
        binary.chmod(0o755)

        def start(name):
            return host.FixtureAuthority(binary, self.dir / name, dict(os.environ), self.dir / f'{name}.log')
        started = [start('first')]
        started[0].stop()
        before = open_descriptors()
        for number in range(5):
            started.append(start(f'fixture-{number}'))
            self.assertEqual(started[-1].settings, {'base': str(self.dir / f'fixture-{number}')})
            self.assertEqual(started[-1].charged(), {'command': 'charged'})
            if number % 2:
                started[-1].kill()
            else:
                self.assertEqual(started[-1].stop(), 0)
        self.assertEqual(open_descriptors(before), before)


class HarnessTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.dir = Path(self.scratch.name)

    def tearDown(self):
        for path in self.dir.rglob('*'):
            os.chmod(path, 0o700 if path.is_dir() else 0o600)
        self.scratch.cleanup()

    def test_consumer_settings_never_return_a_digest(self):
        nested = textwrap.dedent("""
            schema = 1
            [consumers.dev-cli]
            role = "workload"
            credential_sha256 = "{0}"
            [consumers.codespace]
            generation = "codespace-g1"
            role = "control_service"
            credential_sha256 = "{0}"
            max_instances = 1
            [consumers.codespace.control_reservation]
            cpu_milli = 150
            memory_bytes = 536870912
            tasks = 40
            [projects]
        """).format('a' * 64)
        inline = textwrap.dedent("""
            [consumers.codespace]
            generation = "codespace-g1"  # comment
            role = "control_service"
            credential_sha256 = "{0}"
            max_instances = 1
            control_reservation = {{ cpu_milli = 150, memory_bytes = 536870912, tasks = 40 }}
        """).format('a' * 64)
        expected = {'generation': 'codespace-g1', 'role': 'control_service', 'max_instances': 1,
                    'control_reservation': RESERVATION}
        for text in (nested, inline):
            settings = qualify.consumer_settings(text)
            self.assertEqual(settings, expected)
            self.assertNotIn('a' * 64, json.dumps(settings))
        self.assertEqual(qualify.consumer_settings('[consumers.dev-cli]\nrole = "workload"\n'), {})

    def test_the_harness_records_its_open_file_limit(self):
        self.assertEqual(qualify.harness()['open_files_limit'], list(resource.getrlimit(resource.RLIMIT_NOFILE)))

    def test_documentation_keeps_the_candidate(self):
        repo = self.dir / 'repo'
        (repo / 'docs').mkdir(parents=True)
        (repo / 'docs/guide.md').write_text('one\n')
        (repo / 'main.rs').write_text('fn main() {}\n')

        def commit():
            subprocess.run(['git', 'add', '-A'], cwd=repo, env=GIT_ENV, check=True)
            subprocess.run(['git', 'commit', '-q', '-m', 'c'], cwd=repo, env=GIT_ENV, check=True)
            return qualify.code_tree(repo)
        subprocess.run(['git', 'init', '-q'], cwd=repo, env=GIT_ENV, check=True)
        first = commit()
        (repo / 'docs/guide.md').write_text('two\n')
        (repo / 'NOTES.md').write_text('notes\n')
        self.assertEqual(commit(), first)
        (repo / 'main.rs').write_text('fn main() { }\n')
        self.assertNotEqual(commit(), first)

    def test_the_devguard_pin_from_the_lockfile(self):
        (self.dir / 'Cargo.lock').write_text(textwrap.dedent("""
            [[package]]
            name = "devguard-client"
            version = "0.1.0"
            source = "git+https://github.com/novelKR/DevGuard?rev=6e7e065#6e7e065"

            [[package]]
            name = "serde"
            version = "1.0.0"
        """))
        self.assertEqual(qualify.devguard_lock(self.dir), {'devguard-client': {
            'version': '0.1.0', 'source': 'git+https://github.com/novelKR/DevGuard?rev=6e7e065#6e7e065'}})

    def test_candidate_problems(self):
        record = {'codespace': {'dirty': False, 'codex_patch': {'returncode': 0}},
                  'release': {'state': {'runs_release': True}, 'artifacts': {'devguardd': {'matches': True}}},
                  'policy': {'doctor': {'satisfied': True}, 'consumer': {'role': 'control_service'}},
                  'toolchain': {'rustc': f'rustc {qualify.PINNED_RUST} (abc 2026-01-01)'}}
        self.assertEqual(qualify.candidate_problems(record), [])
        record['policy']['consumer'] = None
        record['release']['artifacts']['devguardd']['matches'] = False
        self.assertEqual(qualify.candidate_problems(record), [
            'an installed binary differs from the release manifest',
            'host.toml has no codespace control-service consumer'])

    def test_the_patched_codex_files_do_not_make_the_checkout_dirty(self):
        def run(root, *args):
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=qualify', '-c', 'user.email=qualify@example.com',
                            '-c', 'commit.gpgsign=false', '-c', 'protocol.file.allow=always', *args],
                           check=True, capture_output=True)

        codex, checkout = self.dir / 'codex', self.dir / 'checkout'
        for root in (codex, checkout):
            root.mkdir()
            run(root, 'init', '-q')
        (codex / 'lib.rs').write_text('fn pinned() {}\n')
        run(codex, 'add', 'lib.rs')
        run(codex, 'commit', '-qm', 'pinned')
        (checkout / 'Cargo.toml').write_text('[workspace]\n')
        run(checkout, 'submodule', 'add', '-q', str(codex), 'third_party/codex')
        run(checkout, 'add', 'Cargo.toml')
        run(checkout, 'commit', '-qm', 'gitlink')
        self.assertFalse(qualify.dirty(checkout))
        # The recorded patch changes the submodule's files; the Codex check judges them.
        (checkout / 'third_party/codex/lib.rs').write_text('fn pinned() { observed() }\n')
        self.assertFalse(qualify.dirty(checkout))
        (checkout / 'Cargo.toml').write_text('[workspace]\nmembers = []\n')
        self.assertTrue(qualify.dirty(checkout))
        run(checkout, 'checkout', '-q', 'Cargo.toml')
        run(checkout / 'third_party/codex', 'commit', '-qam', 'another commit')
        self.assertTrue(qualify.dirty(checkout))

    def test_a_late_action_misses_slots_and_never_runs_late(self):
        writer = host.Writer()

        def action(slot):
            if slot == 1:
                time.sleep(0.45)
            return {'outcome': 'answered', 'latency_ms': 1.0, 'state': 'running'}
        schedule = qualify.Schedule(writer, self.dir / 's.jsonl', action, time.monotonic() + 0.05, period=0.1)
        schedule.start()
        time.sleep(0.9)
        rows = schedule.stop()
        writer.close()
        missed = {row['slot'] for row in rows if row['outcome'] == 'missed_slot'}
        self.assertTrue({2, 3, 4} <= missed, rows)
        self.assertEqual([row['slot'] for row in rows], list(range(len(rows))))
        self.assertEqual(len(qualify.read_rows(self.dir, 's')), len(rows))

    def test_slots_that_pass_while_the_last_action_runs_are_missed(self):
        writer = host.Writer()

        def action(slot):
            if slot == 2:
                time.sleep(0.5)
            return {'outcome': 'answered', 'latency_ms': 1.0, 'state': 'running'}
        began = time.monotonic()
        schedule = qualify.Schedule(writer, self.dir / 's.jsonl', action, began, period=0.1)
        schedule.start()
        time.sleep(0.36)
        rows = schedule.stop()
        writer.close()
        self.assertEqual([row['outcome'] for row in rows[:3]], ['answered'] * 3)
        self.assertEqual([row['outcome'] for row in rows[3:]], ['missed_slot'])

    def test_offsets_end_by_themselves(self):
        writer = host.Writer()
        schedule = qualify.Schedule(writer, self.dir / 'o.jsonl', lambda slot: {'outcome': 'refused'},
                                    time.monotonic(), offsets=(0.0, 0.05))
        schedule.start()
        schedule.join(timeout=5)
        self.assertFalse(schedule.is_alive())
        self.assertEqual(len(schedule.stop()), 2)
        writer.close()

    def test_phase_expectations(self):
        self.assertTrue(qualify.phase_checks('admitted', admitted(4))[0]['passed'])
        self.assertFalse(qualify.phase_checks('admitted', admitted(3) + refused(1))[0]['passed'])
        self.assertTrue(qualify.phase_checks('refused', refused(4, 'ADMISSION_UNKNOWN'))[0]['passed'])
        self.assertFalse(qualify.phase_checks('refused', refused(4))[0]['passed'])
        self.assertFalse(qualify.phase_checks('refused', [{'slot': 0, 'outcome': 'missed_slot'}])[0]['passed'])

    def test_targets_wait_until_devguard_admits_them(self):
        class Gateway:
            def __init__(self, refusals, code='RESOURCE_UNAVAILABLE'):
                self.refusals, self.code, self.calls = refusals, code, []

            def exec(self, workspace, command, tty=False):
                self.calls.append(workspace)
                if self.refusals:
                    self.refusals -= 1
                    return {'outcome': 'refused', 'code': self.code}
                return {'outcome': 'started', 'process_id': f'proc-{len(self.calls)}'}

        gateway = Gateway(2)
        pool, refusals, _ = qualify.start_targets(gateway, 3, period=0)
        self.assertEqual([(entry['outcome'], entry['tty']) for entry in pool],
                         [('started', False), ('started', True), ('started', False)])
        self.assertEqual((refusals, gateway.calls), (2, ['term-0'] * 3 + ['term-1', 'term-2']))
        # Only a refusal for capacity or pressure is awaited, and only up to the limit.
        pool, refusals, _ = qualify.start_targets(Gateway(1, 'RESOURCE_AUTHORITY_UNAVAILABLE'), 1, period=0)
        self.assertEqual((pool[0]['outcome'], refusals), ('refused', 0))
        pool, refusals, _ = qualify.start_targets(Gateway(10 ** 6), 1, limit=0.05, period=0.01)
        self.assertEqual(pool[0]['outcome'], 'refused')
        self.assertGreater(refusals, 0)

    def test_settled_awaits_charges_and_instances(self):
        bench = SimpleNamespace(database=None, authority={'consumer': 'codespace'})
        charged = iter([{'attempts': {'codespace': 1}, 'leases': 0}] + [{'attempts': {}, 'leases': 0}] * 3)
        instances = iter([[{'state': 'active'}]] * 2 + [[{'state': 'retired'}]] * 2)
        with mock.patch.object(host, 'journal_charged', lambda database: next(charged)), \
                mock.patch.object(host, 'journal_instances', lambda consumer, database: next(instances)):
            checks = qualify.settled_checks(bench, 'uds', period=0)
        self.assertEqual([item['passed'] for item in checks], [True, True], checks)
        # An instance still active once the limit has passed fails, though nothing is charged.
        with mock.patch.object(host, 'journal_charged', lambda database: {'attempts': {}, 'leases': 0}), \
                mock.patch.object(host, 'journal_instances', lambda consumer, database: [{'state': 'active'}]):
            checks = qualify.settled_checks(bench, 'uds', limit=0.05, period=0.01)
        self.assertEqual([item['passed'] for item in checks], [True, False], checks)

    def test_repetition_outcomes(self):
        report = {'service_before': {'runs_release': True, 'service_pid': 7},
                  'service_after': {'runs_release': True, 'service_pid': 7}, 'rehearsal': False}
        good = {'verdict': 'pass', 'reasons': []}
        self.assertIsNone(qualify.early_outcome(report))
        self.assertEqual(qualify.repetition_outcome(report, good, good), ('pass', []))
        restarted = dict(report, service_after={'runs_release': True, 'service_pid': 8})
        self.assertEqual(qualify.repetition_outcome(restarted, good, good)[0], 'inconclusive')
        self.assertEqual(qualify.repetition_outcome(dict(report, rehearsal=True), good, good)[0], 'inconclusive')
        cold = dict(report, prewarm=[{'result': 'completed', 'exit_code': 1}])
        self.assertEqual(qualify.early_outcome(cold)[1], ['the warm target directory could not be built'])

    def test_modes(self):
        self.assertEqual(qualify.modes('uds'), ['uds'])
        self.assertEqual(qualify.modes('in-process, uds'), ['in-process', 'uds'])
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            qualify.arguments(['matrix', '--out', 'x', '--mcp-bin', 'm', '--runtime-bin', 'r', '--release', 'v',
                               '--modes', 'pipe'])

    # ------------------------------------------------------------ verify

    def write_rows(self, directory, rows):
        directory.mkdir(parents=True, exist_ok=True)
        for name, values in rows.items():
            (directory / f'{name}.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in values))

    def scenarios_run(self, name='matrix', falsify=False):
        evidence = host.Evidence(self.dir / name)
        check = analysis.check
        entries = [{'scenario': 'exits', 'mode': 'uds', 'io': 'pipe', 'checks': [check('a', True)]},
                   {'scenario': 'timeout', 'mode': 'uds', 'io': 'pty', 'checks': [check('a', True), check('b', False)]}]
        for entry in entries:
            entry['verdict'] = analysis.scenario_verdict(entry['checks'])
            evidence.append('scenarios.jsonl', entry)
        if falsify:
            entries[1]['verdict'] = 'pass'
        verdict = analysis.suite_verdict([entry['verdict'] for entry in entries])
        evidence.write_json('report.json', {'suite': 'matrix', 'verdict': verdict, 'result': {'verdict': verdict},
                                            'harness': qualify.harness()})
        if falsify:
            os.chmod(evidence.path / 'scenarios.jsonl', 0o600)
            (evidence.path / 'scenarios.jsonl').write_text(''.join(json.dumps(entry) + '\n' for entry in entries))
        evidence.seal()
        return evidence.path

    def test_verify_scenarios(self):
        result = qualify.verify(self.scenarios_run())
        self.assertEqual((result['verdict'], result['recomputed_verdict']), ('verified', 'failed'), result)
        self.assertTrue(result['same_harness'])
        falsified = qualify.verify(self.scenarios_run('falsified', falsify=True))
        self.assertEqual(falsified['verdict'], 'failed')
        self.assertTrue(any(problem.startswith('uds pty timeout verdict') for problem in falsified['problems']),
                        falsified['problems'])

    def authority_run(self, name='authority', lie=False):
        evidence = host.Evidence(self.dir / name)
        records = []
        for phase, fault, expectation, release_expected in qualify.AUTHORITY_PHASES:
            raw = evidence.file(f'uds/authority/{phase}/x').parent
            terminations, releases = terminated(3)
            if not release_expected:
                releases = []
            rows = {'status_pipe': answered(15), 'status_pty': answered(15), 'terminations': terminations,
                    'releases': releases, 'io': echoed(5),
                    'refusals': admitted(4) if expectation == 'admitted' else refused(4, 'ADMISSION_UNKNOWN')}
            self.write_rows(raw, rows)
            record = {'mode': 'uds', 'phase': phase, 'fault': fault, 'seconds': 15.01,
                      'release_expected': release_expected, 'expectation': expectation, 'invalid': [],
                      'checks': qualify.phase_checks(expectation, rows['refusals']), 'raw': qualify.raw_hashes(raw)}
            record['verdict'], record['reasons'], record['metrics'] = qualify.phase_verdict(record, rows)
            if lie and phase == 'stopped':
                record['metrics']['status']['p99_ms'] = 1.0
            evidence.write_json(f'uds/authority/{phase}/phase.json', record)
            records.append(record)
        verdict = analysis.suite_verdict([record['verdict'] for record in records])
        evidence.write_json('report.json', {'suite': 'authority', 'verdict': verdict, 'result': {'verdict': verdict},
                                            'harness': qualify.harness()})
        evidence.seal()
        return evidence.path

    def test_verify_authority(self):
        path = self.authority_run()
        result = qualify.verify(path)
        self.assertEqual((result['verdict'], result['recomputed_verdict']), ('verified', 'passed'), result)
        self.assertEqual(qualify.verify(self.authority_run('lie', lie=True))['verdict'], 'failed')
        rows = path / 'uds/authority/paused/status_pty.jsonl'
        os.chmod(rows, 0o600)
        rows.write_text(rows.read_text().replace('"latency_ms": 5.0', '"latency_ms": 900.0', 1))
        result = qualify.verify(path)
        self.assertEqual(result['verdict'], 'failed')
        self.assertTrue(any(problem.startswith('changed uds/authority/paused/status_pty.jsonl')
                            for problem in result['problems']), result['problems'])

    def control_run(self, name='control', interval_seconds=60.2):
        evidence = host.Evidence(self.dir / name)
        plan = dict(qualify.CONTROL_REHEARSAL, combinations=['in-process'], load='cold', rehearsal=True)
        evidence.write_json('run.json', {'plan': plan, 'protocol': qualify.CONTROL_PROTOCOL,
                                         'targets': {'status_p99_ms': analysis.STATUS_P99_MS,
                                                     'termination_ack_p99_ms': analysis.TERMINATION_P99_MS},
                                         'release': {'id': 'r1'}, 'reservation': RESERVATION})
        key = 'in-process-1'
        records = {}
        for kind in ('idle', 'load'):
            raw = evidence.file(f'{key}/{kind}/x').parent
            terminations, releases = terminated(3)
            rows = {'status_pipe': answered(60), 'status_pty': answered(60), 'terminations': terminations,
                    'releases': releases, 'io': echoed(12), 'refusals': refused(2)}
            sampled = {'host': [{'pressure_level': '1', 'swap_used_mib': 10.0, 'load_average': [2.0, 1.0, 1.0]}],
                       'service': [{'runs_release': True, 'current': 'r1', 'service_pid': 7}],
                       'footprint': [{'owner': {'rss_bytes': 1 << 20, 'cpu_percent': 1.0, 'threads': 9},
                                      'gateway': None}]}
            self.write_rows(raw, dict(rows, **sampled))
            runs = []
            record = {'kind': kind, 'combination': 'in-process', 'invalid': [], 'seconds': interval_seconds,
                      'started_unix': 1000.0, 'ended_unix': 1000.0 + interval_seconds,
                      'status_targets': [{'effective': True}, {'effective': True}],
                      'journal': {'still_charged': [], 'duplicate_launches': 0, 'readmitted_uncertain': 0}}
            if kind == 'load':
                runs = [{'consumer': 'cargo', 'result': 'completed', 'exit_code': 0, 'running_from': 1000.0,
                         'ended': 1000.0 + interval_seconds}]
                (raw / 'runs.json').write_text(json.dumps(runs) + '\n')
                record.update(consumers=['cargo'], load_issues=[], load_failures=[])
            record['raw'] = qualify.raw_hashes(raw)
            metrics = qualify.interval_metrics(record, rows, sampled, runs, 'r1', 7, RESERVATION)
            record['verdict'], record['reasons'] = analysis.interval_verdict(kind, record['invalid'], metrics)
            record['metrics'] = metrics
            evidence.write_json(f'{key}/{kind}/interval.json', record)
            records[kind] = record
        report = {'combination': 'in-process', 'repetition': 1, 'rehearsal': True,
                  'service_before': {'runs_release': True, 'service_pid': 7},
                  'service_after': {'runs_release': True, 'service_pid': 7},
                  'intervals': {kind: {'verdict': record['verdict'], 'reasons': record['reasons']}
                                for kind, record in records.items()}}
        report['verdict'], report['reasons'] = qualify.repetition_outcome(report, records['idle'], records['load'])
        evidence.write_json(f'{key}/report.json', report)
        combination = analysis.combination_verdict([report['verdict']], 1)
        overall = analysis.overall_verdict({'in-process': {'verdict': combination}}, plan, qualify.CONTROL_PROTOCOL,
                                           True)
        evidence.write_json('report.json', {
            'suite': 'control', 'verdict': overall, 'harness': qualify.harness(),
            'result': {'verdict': overall, 'combinations': {'in-process': {
                'verdict': combination, 'repetitions': [{
                    'repetition': 1, 'verdict': report['verdict'], 'reasons': report['reasons'],
                    'report_sha256': host.sha256(evidence.path / key / 'report.json')}]}}}})
        evidence.seal()
        return evidence.path, records

    def test_verify_control(self):
        path, records = self.control_run()
        self.assertEqual([records[kind]['verdict'] for kind in ('idle', 'load')], ['pass', 'pass'])
        result = qualify.verify(path)
        self.assertEqual((result['verdict'], result['recomputed_verdict']), ('verified', 'inconclusive'), result)
        with contextlib.redirect_stdout(io.StringIO()) as printed:
            self.assertEqual(qualify.main(['verify', str(path)]), 0)
        self.assertEqual(json.loads(printed.getvalue())['verdict'], 'verified')
        interval = path / 'in-process-1/load/interval.json'
        os.chmod(interval, 0o600)
        record = json.loads(interval.read_text())
        record['metrics']['status']['p99_ms'] = 1.0
        interval.write_text(json.dumps(record))
        os.chmod(path / 'MANIFEST.json', 0o600)
        manifest = json.loads((path / 'MANIFEST.json').read_text())
        manifest['files']['in-process-1/load/interval.json'] = host.sha256(interval)
        (path / 'MANIFEST.json').write_text(json.dumps(manifest))
        result = qualify.verify(path)
        self.assertEqual(result['verdict'], 'failed')
        self.assertTrue(any(problem.startswith('in-process-1 load metrics') for problem in result['problems']),
                        result['problems'])
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(qualify.main(['verify', str(path)]), 1)

    def test_verify_refuses_an_unsealed_directory(self):
        (self.dir / 'open').mkdir()
        self.assertEqual(qualify.verify(self.dir / 'open')['verdict'], 'failed')


class ScriptedGateway:
    """A gateway whose exec_command answers in turn from `answers`, the last one again and again;
    the processes it starts end at once when terminated."""

    def __init__(self, answers, pid=4141):
        self.answers, self.pid, self.sent = list(answers), pid, []

    def exec(self, workspace, command, tty=False):
        self.sent.append({'workspace': workspace, 'tag': command[3], 'tty': tty})
        return dict(self.answers.pop(0) if len(self.answers) > 1 else self.answers[0])

    def terminate(self, process_id, timeout=None):
        return 12.5

    def wait_exited(self, process_id, limit=30, period=0.02):
        return {'state': 'exited', 'termination': 'terminated'}


class RecoveryTests(unittest.TestCase):
    """P1R-C06: the recovery suites' analysis, the Runner's log and wire protocol, the serving gate,
    the command line and `verify` of a recovery run."""

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.dir = Path(self.scratch.name)

    def tearDown(self):
        for path in self.dir.rglob('*'):
            os.chmod(path, 0o700 if path.is_dir() else 0o600)
        self.scratch.cleanup()

    def test_claims_from_the_runner_log(self):
        log = '\n'.join((
            'codespace-codex-runtime: independent Runner 7 listening on /x/runner.sock',
            'codespace-codex-runtime: process 11 claimed the Runner under epoch 1',
            'codespace-codex-runtime: refused the Claim of process 12 (controller_active): another connection '
            'controls this Runner and is still open',
            'codespace-codex-runtime: the controlling gateway detached; its processes go on',
            'codespace-codex-runtime: process 13 claimed the Runner under epoch 2; an earlier controller started '
            'work, so it starts nothing where that controller did until it reconciled it'))
        self.assertEqual(host.claim_lines(log), [
            {'granted': True, 'pid': 11, 'epoch': 1, 'previous_controller': False},
            {'granted': False, 'pid': 12, 'refusal': 'controller_active'},
            {'granted': True, 'pid': 13, 'epoch': 2, 'previous_controller': True}])

    def test_the_wire_client_frames_requests_and_passes_events_over(self):
        path = self.dir / 'r.sock'
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(str(path))
        server.listen(1)
        seen = []

        def exactly(connection, count):
            data = b''
            while len(data) < count:
                chunk = connection.recv(count - len(data))
                if not chunk:
                    raise ConnectionError('closed')
                data += chunk
            return data

        def send(connection, message):
            body = json.dumps(message).encode()
            connection.sendall(struct.pack('>I', len(body)) + body)

        def serve():
            connection, _ = server.accept()
            with connection:
                for answer in ({'ok': True, 'result': {'type': 'claimed', 'data': {'epoch': 4}}},
                               {'ok': False, 'error': {'code': 'STALE_CONTROLLER', 'message': 'not the controller'}}):
                    (length,) = struct.unpack('>I', exactly(connection, 4))
                    request = json.loads(exactly(connection, length))
                    seen.append(request)
                    # An exit event, which the Runner sends its controller, comes before the answer.
                    send(connection, {'protocol': host.WIRE_PROTOCOL, 'kind': 'event', 'event': {'type': 'exited'}})
                    send(connection, dict(answer, protocol=host.WIRE_PROTOCOL, kind='response',
                                          request_id=request['request_id']))
        thread = threading.Thread(target=serve, daemon=True)
        thread.start()
        client = host.WireClient(path, timeout=10)
        try:
            claim = client.claim('store-1')
            stale = client.request('terminate', {'process_id': 'proc-1'}, epoch=3)
        finally:
            client.close()
            thread.join(timeout=10)
            server.close()
        self.assertEqual([request['op'] for request in seen], [{'op': 'claim', 'data': {'store_id': 'store-1'}},
                                                              {'op': 'terminate', 'data': {'process_id': 'proc-1'}}])
        self.assertEqual([request.get('epoch') for request in seen], [None, 3])
        self.assertEqual({request['protocol'] for request in seen}, {host.WIRE_PROTOCOL})
        self.assertEqual((claim['ok'], claim['type'], claim['data']), (True, 'claimed', {'epoch': 4}))
        self.assertEqual((stale['ok'], stale['code']), (False, 'STALE_CONTROLLER'))

    def test_output_continuity(self):
        whole = stream('beat', 1, 10)
        # A line split between two reads is still one line.
        split = ([dict(whole[0], chunk=whole[0]['chunk'][:-5]),
                  dict(whole[1], chunk=whole[0]['chunk'][-5:] + whole[1]['chunk'])] + whole[2:])
        for rows in (whole, split):
            metrics = analysis.continuity_metrics(rows, 'beat')
            self.assertEqual((metrics['lines'], metrics['first'], metrics['last'], metrics['passed']),
                             (10, 1, 10, True))
        # A gap the Runner declared: the numbers only move on after it.
        burst = stream('burst', 1, 6)
        burst += [lost(row, row['from'] + 4096) for row in stream('burst', 40, 42, cursor=burst[-1]['to'])]
        metrics = analysis.continuity_metrics(burst, 'burst')
        self.assertEqual((metrics['declared_gaps'], metrics['declared_lost_lines'], metrics['gap_reasons'],
                          metrics['undeclared_lost_lines'], metrics['passed']), (1, 33, ['ring_overflow'], 0, True))
        # Lines skipped or seen again where no gap was declared fail.
        skipped = analysis.continuity_metrics(stream('beat', 1, 6) + stream('beat', 9, 12, cursor=1000), 'beat')
        self.assertEqual((skipped['undeclared_lost_lines'], skipped['passed']), (2, False))
        again = analysis.continuity_metrics(stream('beat', 1, 6) + stream('beat', 4, 9, cursor=1000), 'beat')
        self.assertEqual((again['repeated_lines'], again['passed']), (3, False))
        empty = [{'slot': 0, 'outcome': 'read', 'from': 0, 'to': 0, 'chunk': '', 'output_lost': False,
                  'retained_from': 0}]
        self.assertIsNone(analysis.continuity_metrics(empty, 'burst')['passed'])

    def test_timeouts(self):
        metrics = analysis.timeout_metrics([timed_out(1000, 1000.05, in_gap=True), timed_out(2000, 2000.4)])
        self.assertEqual((metrics['targets'], metrics['in_gap'], metrics['passed']), (2, 1, True))
        for row, field in ((timed_out(1000, 1003.0), 'extended'),
                           (timed_out(1000, 1000.05, deadline_after_ms=1_005_000), 'extended'),
                           (timed_out(1000, 999.5), 'early'),
                           (timed_out(1000, 1000.05, termination='exited'), 'misreported')):
            metrics = analysis.timeout_metrics([row])
            self.assertEqual((metrics[field], metrics['passed']), (1, False), field)
        unobserved = analysis.timeout_metrics([timed_out(1000, None)])
        self.assertEqual((unobserved['missing'], unobserved['passed']), (1, True))
        self.assertIsNone(analysis.timeout_metrics([])['passed'])

    def test_restarts(self):
        rows = cycles(4)[:-1]
        metrics = analysis.restart_metrics(rows)
        self.assertEqual((metrics['cycles'], metrics['restarts'], metrics['fence_requests'], metrics['passed']),
                         (4, 3, 36, True))
        self.assertEqual(metrics['ends'], {'sigterm': 1, 'sigusr1': 1, 'sigkill': 1})
        cases = {
            'stale_mutations': lambda rows: rows[1]['fence']['requests'][0].update(ok=True, code=None),
            'unfenced_refusals': lambda rows: rows[1]['fence']['requests'][4].update(code='NOT_FOUND'),
            'fence_claims_not_refused': lambda rows: rows[2]['fence'].update(claim='claimed'),
            'epoch_mismatches': lambda rows: rows[2]['claim'].update(epoch=2),
            'targets_not_running': lambda rows: rows[3]['inherited'].update({'beat-pty': 'exited'}),
            'unexpected_exits': lambda rows: rows[0]['ended'].update(returncode=1),
            'rival_claims': lambda rows: rows[0]['rival'].update(claims=[{'granted': False, 'pid': 201}]),
            'rivals_served': lambda rows: rows[0]['rival'].update(returncode=0, another_writer=True),
            'rivals_not_refused_for_the_lock': lambda rows: rows[0]['rival'].update(another_writer=False)}
        for field, change in cases.items():
            changed = json.loads(json.dumps(rows))
            change(changed)
            metrics = analysis.restart_metrics(changed)
            self.assertEqual((metrics[field], metrics['passed']), (1, False), field)
        # A rival still waiting for the writer when its cycle ended was killed; that fails nothing.
        waiting = json.loads(json.dumps(rows))
        waiting[0]['rival'].update(killed=True, returncode=-9, another_writer=False)
        self.assertEqual(analysis.restart_metrics(waiting)['rivals_killed_waiting'], 1)
        self.assertTrue(analysis.restart_metrics(waiting)['passed'])
        self.assertIsNone(analysis.restart_metrics([])['passed'])

    def test_approvals_and_markers(self):
        markers = {'confirm': ['approval-001', 'approval-002'], 'term': ['term-0']}
        rows = [resumed('approval-001', 'proc-1'), resumed('approval-002', 'proc-2', killed=True)]
        metrics = analysis.approval_metrics(rows, markers)
        self.assertEqual((metrics['resumed'], metrics['killed_during_resume'], metrics['passed']), (2, 1, True))
        twice = analysis.approval_metrics(rows, dict(markers, again=['approval-002']))
        self.assertEqual((twice['duplicates'], twice['passed']), (1, False))
        differs = analysis.approval_metrics([resumed('approval-001', 'proc-1', again={'process_id': 'proc-9'})],
                                            markers)
        self.assertEqual((differs['second_resume_differs'], differs['passed']), (1, False))
        never = analysis.approval_metrics([resumed('approval-003', 'proc-3')], markers)
        self.assertEqual((never['resumed_but_never_ran'], never['passed']), (1, False))
        unresumed = analysis.approval_metrics([{'tag': 'approval-004', 'resume': {'outcome': 'error'}}], markers)
        self.assertEqual((unresumed['unresumed'], unresumed['passed']), (1, False))
        capacity = analysis.approval_metrics([{'tag': 'approval-004', 'not_admitted': True}], markers)
        self.assertEqual((capacity['not_admitted'], capacity['unresumed'], capacity['passed']), (1, 0, True))
        execs = [{'tag': 'term-0', 'outcome': 'started'}, {'tag': 'hog-1', 'outcome': 'refused'},
                 {'tag': 'approval-001', 'outcome': 'refused', 'approval': True}]
        self.assertTrue(analysis.marker_metrics(markers, execs)['passed'])
        ran = analysis.marker_metrics(dict(markers, hog=['hog-1']), execs)
        self.assertEqual((ran['refused_but_ran'], ran['passed']), (['hog-1'], False))
        self.assertEqual(analysis.marker_metrics(dict(markers, again=['term-0']), execs)['ran_twice'], ['term-0'])

    def test_drain_failures(self):
        drain = {'governed': True, 'expected_epoch': 5, 'claim_epoch': 5, 'terminated': 0, 'runner_exit': 0,
                 'socket_left': False, 'retired': True, 'charged': 0,
                 'gateway': {'how': 'eof', 'returncode': 0, 'stopped': None}}
        self.assertEqual(analysis.drain_failures(drain), [])
        self.assertEqual(analysis.drain_failures(None), ['the Runner was not drained'])
        for change, failure in (
                ({'claim_epoch': 4}, 'the draining gateway claimed the Runner under epoch 4, not 5'),
                ({'gateway': {'how': 'eof', 'returncode': -9, 'stopped': 'SIGKILL'}},
                 'the draining gateway exited -9 when its input closed'),
                ({'terminated': None}, 'the Runner did not log its Stop'),
                ({'terminated': 2}, 'the Stop terminated 2 processes the harness had already ended'),
                ({'runner_exit': None}, 'the Runner exited None after Stop'),
                ({'socket_left': True}, "the Runner's socket was left after Stop"),
                ({'retired': False}, "the Runner's DevGuard instance was not retired"),
                ({'charged': 1}, 'work of the consumer stayed charged after the drain'),
                ({'error': 'OSError: boom'}, 'the drain failed: OSError: boom')):
            self.assertEqual(analysis.drain_failures(dict(drain, **change)), [failure])
        self.assertEqual(analysis.drain_failures(dict(drain, governed=False, retired=None, charged=None)), [])

    def test_the_serving_gate(self):
        serving = qualify.Serving()
        sample = qualify.gated(serving, lambda gateway, slot: {'outcome': 'answered', 'slot': slot})
        self.assertEqual(sample(0), {'outcome': 'restarting'})
        serving.serve(SimpleNamespace(pid=41))
        self.assertEqual(sample(1), {'outcome': 'answered', 'slot': 1, 'gateway': 41})
        entered, release = threading.Event(), threading.Event()

        def slow(gateway, slot):
            entered.set()
            release.wait(10)
            return {'outcome': 'answered'}
        thread = threading.Thread(target=qualify.gated(serving, slow), args=(2,))
        thread.start()
        self.assertTrue(entered.wait(10))
        # A restart closes the gate at once, and waits for what is in flight up to its limit.
        self.assertEqual(serving.close(limit=0.05), 1)
        self.assertEqual(sample(3), {'outcome': 'restarting'})
        release.set()
        thread.join(timeout=10)
        served = serving.served_s
        self.assertGreater(served, 0)
        time.sleep(0.02)
        self.assertEqual(serving.close(), 0)
        self.assertEqual(serving.served_s, served)

    def test_new_work_is_sent_again_until_its_workspace_is_free(self):
        def answers(*codes):
            results = iter([{'outcome': 'refused', 'code': code} for code in codes]
                           + [{'outcome': 'started', 'process_id': 'proc-1'}])
            return lambda: next(results)

        def busy():
            return {'outcome': 'refused', 'code': 'WORKSPACE_BUSY'}
        # once_reconciled waits for the reconciliation only: a workspace still held is its answer.
        result, pending, _ = qualify.once_reconciled(answers('RECOVERY_PENDING', 'WORKSPACE_BUSY'), period=0)
        self.assertEqual((result['code'], pending), ('WORKSPACE_BUSY', 1))
        # once_free also waits while the gateway still holds the workspace for a process it
        # reported exited, until the Runner's exit event frees it.
        result, pending, _ = qualify.once_free(answers('RECOVERY_PENDING', 'WORKSPACE_BUSY', 'WORKSPACE_BUSY'))
        self.assertEqual((result['outcome'], pending), ('started', 3))
        # Any other refusal is the answer at once, and one that lasts is the answer at the limit.
        result, pending, _ = qualify.once_free(answers('RESOURCE_UNAVAILABLE'))
        self.assertEqual((result['code'], pending), ('RESOURCE_UNAVAILABLE', 0))
        result, pending, waited = qualify.once_free(busy, limit=0.12)
        self.assertEqual(result['code'], 'WORKSPACE_BUSY')
        self.assertGreaterEqual(waited, 120)
        self.assertGreater(pending, 0)

    def test_samples_wait_for_the_reconciliation_of_their_workspace(self):
        rows = []
        bench = SimpleNamespace(roots={'term': self.dir, 'hog': self.dir}, governed=False)
        execs = qualify.Execs(bench, SimpleNamespace(write=lambda path, row: rows.append(row)),
                              self.dir / 'execs.jsonl')
        terminations = qualify.GatedTerminations(bench, execs, None)
        pending = {'outcome': 'refused', 'code': 'RECOVERY_PENDING'}
        with mock.patch.object(qualify, 'tag_pid', return_value=4242), \
                mock.patch.object(qualify, 'gone', return_value=True):
            # A target the serving gateway refuses while it reconciles `term` is sent again, under
            # a new tag, and timed once it started.
            gateway = ScriptedGateway([pending, {'outcome': 'started', 'process_id': 'proc-1'}])
            sample = terminations(gateway, 1)
            self.assertEqual((sample['outcome'], sample['termination'], sample['ack_ms'], sample['pending_refusals']),
                             ('acknowledged', 'terminated', 12.5, 1))
            self.assertEqual([(row['tag'], row['code']) for row in rows],
                             [('term-1', 'RECOVERY_PENDING'), ('term-1-1', None)])
            self.assertTrue(all(sent['tty'] for sent in gateway.sent))
            # A target refused for capacity is not a sample, and is not sent again.
            gateway = ScriptedGateway([{'outcome': 'refused', 'code': 'RESOURCE_UNAVAILABLE'}, pending])
            sample = terminations(gateway, 2)
            self.assertEqual((sample['outcome'], sample['pending_refusals'], len(gateway.sent)), ('not_started', 0, 1))
            # One still refused when the wait ends is an error, a missing sample.
            gateway = ScriptedGateway([pending])
            with mock.patch.object(qualify, 'RECONCILE_WAIT_S', 0.3):
                sample = terminations(gateway, 3)
            self.assertEqual((sample['outcome'], sample['code']), ('error', 'RECOVERY_PENDING'))
            self.assertEqual(sample['pending_refusals'], len(gateway.sent) - 1)
            self.assertGreater(sample['pending_refusals'], 0)
        # The refusal timed is the authority's, not the reconciliation's.
        gateway = ScriptedGateway([pending, {'outcome': 'refused', 'code': 'RESOURCE_UNAVAILABLE', 'latency_ms': 41.0}])
        row = qualify.gated_admission(execs)(gateway, 4)
        self.assertEqual((row['outcome'], row['code'], row['latency_ms'], row['pending_refusals']),
                         ('refused', 'RESOURCE_UNAVAILABLE', 41.0, 1))
        self.assertEqual([sent['tag'] for sent in gateway.sent], ['hog-4', 'hog-4-1'])

    def test_recovery_arguments(self):
        common = ['--out', 'x', '--mcp-bin', 'm', '--runtime-bin', 'r']
        self.assertEqual(qualify.arguments(['recovery-fence', *common, '--participation', 'off']).participation,
                         ['off'])
        self.assertEqual(qualify.arguments(['recovery-store', *common]).participation, list(qualify.PARTICIPATIONS))
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            qualify.arguments(['recovery-fence', *common, '--participation', 'sometimes'])
        with mock.patch.object(qualify.platform, 'system', return_value='Darwin'):
            with contextlib.redirect_stderr(io.StringIO()) as printed, self.assertRaises(SystemExit):
                qualify.arguments(['recovery-fence', *common])
            self.assertIn('needs --fixture-bin, --fixture-helper', printed.getvalue())
            fixture = ['--fixture-bin', 'f', '--fixture-helper', 'h']
            self.assertEqual(qualify.arguments(['recovery-fence', *common, *fixture]).fixture_bin, Path('f'))
            self.assertEqual(qualify.arguments(['recovery', *common, '--participation', 'off']).release, None)
            with contextlib.redirect_stderr(io.StringIO()) as printed, self.assertRaises(SystemExit):
                qualify.arguments(['recovery', *common, *fixture])
            self.assertIn('needs --release, --work, --qualify-bin, --devguard-checkout', printed.getvalue())

    # ------------------------------------------------------------ verify

    def write_rows(self, directory, rows):
        directory.mkdir(parents=True, exist_ok=True)
        for name, values in rows.items():
            (directory / f'{name}.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in values))

    def recovery_interval(self, evidence, key, kind, served_s=60.0):
        """An interval of four gateway cycles that saw nothing wrong, as a self-test records it."""
        raw = evidence.file(f'{key}/{kind}/x').parent
        terminations, _ = terminated(3)
        burst = stream('burst', 1, 6)
        burst += [lost(row, row['from'] + 4096) for row in stream('burst', 900, 902, cursor=burst[-1]['to'])]
        rows = {'status_pipe': answered(60) + [{'slot': 60, 'outcome': 'restarting'}], 'status_pty': answered(60),
                'terminations': terminations, 'releases': [], 'io': echoed(12), 'refusals': [],
                'read_beat': stream('beat', 1, 30), 'read_burst': burst}
        records = {'restarts': cycles(4),
                   'timeouts': [timed_out(1000, 1000.04, in_gap=True), timed_out(1030, 1030.02)],
                   'approvals': [resumed('approval-001', 'proc-a1'), resumed('approval-002', 'proc-a2', killed=True)],
                   'execs': [{'tag': 'status-pipe', 'outcome': 'started'},
                             {'tag': 'approval-001', 'outcome': 'refused', 'code': 'APPROVAL_REQUIRED',
                              'approval': True}]}
        found = {'status-pipe': ['status-pipe'], 'confirm': ['approval-001', 'approval-002']}
        sampled = {'host': [{'pressure_level': '1', 'swap_used_mib': 10.0, 'load_average': [2.0, 1.0, 1.0]}],
                   'footprint': [{'owner': {'rss_bytes': 1 << 20, 'cpu_percent': 1.0, 'threads': 9},
                                  'gateway': {'rss_bytes': 1 << 21, 'cpu_percent': 2.0, 'threads': 8}}]}
        self.write_rows(raw, dict(rows, **records, **sampled))
        (raw / 'markers.json').write_text(json.dumps(found))
        record = {'kind': kind, 'combination': 'independent-off', 'governed': False, 'invalid': [],
                  'planned_restarts': 3, 'served_s': served_s, 'started_unix': 990.0, 'ended_unix': 1060.0,
                  'seconds': 70.0, 'targets': [{'effective': True}, {'effective': True}],
                  'drain': {'governed': False, 'expected_epoch': 5, 'claim_epoch': 5, 'runner_exit': 0,
                            'socket_left': False, 'terminated': 0,
                            'gateway': {'how': 'eof', 'returncode': 0, 'stopped': None}}}
        record['raw'] = qualify.raw_hashes(raw)
        metrics = qualify.recovery_interval_metrics(record, rows, sampled, records, found, [], None, None, None)
        record['verdict'], record['reasons'] = analysis.recovery_interval_verdict(kind, [], metrics)
        record['metrics'] = metrics
        evidence.write_json(f'{key}/{kind}/interval.json', record)
        return record

    def recovery_run(self, name='recovery'):
        evidence = host.Evidence(self.dir / name)
        check = analysis.check
        entries = [{'suite': 'recovery-fence', 'scenario': 'stale-epoch', 'mode': 'independent',
                    'participation': 'off', 'io': None, 'checks': [check('a stale epoch is refused', True)]},
                   {'suite': 'recovery-fence', 'scenario': 'stale-epoch', 'mode': 'independent',
                    'participation': 'required', 'io': None, 'checks': [], 'not_run': qualify.NOT_NATIVE}]
        for entry in entries:
            entry['verdict'] = analysis.scenario_verdict(entry['checks'], entry.get('not_run'))
            evidence.append('scenarios.jsonl', entry)
        functional = analysis.suite_verdict([entry['verdict'] for entry in entries])
        plan = dict(qualify.RECOVERY_REHEARSAL, combinations=['independent-off'], load=None, rehearsal=True)
        evidence.write_json('run.json', {'plan': plan, 'protocol': qualify.RECOVERY_PROTOCOL, 'governed': False})
        key = 'independent-off-1'
        records = {kind: self.recovery_interval(evidence, key, kind) for kind in ('idle', 'load')}
        report = {'combination': 'independent-off', 'repetition': 1, 'rehearsal': True, 'governed': False,
                  'intervals': {kind: {'verdict': record['verdict'], 'reasons': record['reasons']}
                                for kind, record in records.items()}}
        report['verdict'], report['reasons'] = qualify.recovery_repetition_outcome(report, records['idle'],
                                                                                   records['load'])
        evidence.write_json(f'{key}/report.json', report)
        combination = analysis.combination_verdict([report['verdict']], 1)
        measurement = analysis.overall_verdict({'independent-off': {'verdict': combination}}, plan,
                                               qualify.RECOVERY_PROTOCOL, True)
        verdict = analysis.recovery_verdict(functional, measurement)
        evidence.write_json('report.json', {
            'suite': 'recovery', 'verdict': verdict, 'harness': qualify.harness(),
            'result': {'verdict': verdict, 'functional': {'verdict': functional}, 'measurement': {
                'verdict': measurement, 'combinations': {'independent-off': {'verdict': combination, 'repetitions': [{
                    'repetition': 1, 'verdict': report['verdict'], 'reasons': report['reasons'],
                    'report_sha256': host.sha256(evidence.path / key / 'report.json')}]}}}}})
        evidence.seal()
        return evidence.path, records

    def test_recovery_interval_verdicts(self):
        _, records = self.recovery_run()
        self.assertEqual((records['idle']['verdict'], records['idle']['reasons']), ('pass', []))
        self.assertEqual(records['load']['reasons'], ['no load ran: there is no DevGuard service to run it'])
        metrics = records['idle']['metrics']

        def judged(change):
            changed = json.loads(json.dumps(metrics))
            change(changed['recovery'])
            return analysis.recovery_interval_verdict('idle', [], changed)
        for change, expected in (
                (lambda found: found['timeouts'].update(in_gap=0),
                 ('inconclusive', ['timeouts: no deadline fell while no gateway ran'])),
                (lambda found: found['approvals'].update(killed_during_resume=0),
                 ('inconclusive', ['approvals: no gateway was killed with a resume in flight'])),
                (lambda found: found['restarts'].update(restarts=2), ('inconclusive', ['restarts 2 of 3'])),
                (lambda found: found['continuity']['burst_pipe'].update(passed=None),
                 ('inconclusive', ['output continuity of burst_pipe: nothing was read'])),
                (lambda found: found['restarts'].update(passed=False), ('fail', ['recovery restarts'])),
                (lambda found: found['continuity']['beat_pty'].update(passed=False),
                 ('fail', ['output continuity of beat_pty'])),
                (lambda found: found['last_gateway'].update(returncode=-9),
                 ('fail', ['the last gateway exited -9 when its input closed'])),
                (lambda found: found['drain'].update(socket_left=True),
                 ('fail', ["the Runner's socket was left after Stop"]))):
            self.assertEqual(judged(change), expected)
        self.assertEqual(analysis.recovery_interval_verdict('idle', ['the Runner did not register'], metrics),
                         ('inconclusive', ['the Runner did not register']))
        self.assertEqual(analysis.recovery_verdict('passed', 'qualified'), 'qualified')
        self.assertEqual(analysis.recovery_verdict('passed', 'inconclusive'), 'inconclusive')
        self.assertEqual(analysis.recovery_verdict('incomplete', 'qualified'), 'inconclusive')
        self.assertEqual(analysis.recovery_verdict('passed', 'failed'), 'failed')

    def test_an_unreadable_journal_at_the_drain_is_recorded(self):
        interval = qualify.RecoveryInterval.__new__(qualify.RecoveryInterval)
        interval.governed, interval.number, interval.invalid = True, 6, []
        interval.args = SimpleNamespace(journal=self.dir / 'missing/authority.sqlite')

        def no_gateway(*args, **kwargs):
            raise OSError(24, 'Too many open files')
        interval.bench = SimpleNamespace(gateway=no_gateway, socket=self.dir / 'runner.sock',
                                         authority={'consumer': host.CONSUMER},
                                         database=self.dir / 'missing/authority.sqlite')
        interval.runner = SimpleNamespace(pid=4242, wait=lambda limit: 0,
                                          log=lambda: 'stopped; 0 processes were terminated\n')
        drain = interval.drain()
        self.assertEqual(drain['error'], 'OSError: [Errno 24] Too many open files')
        self.assertTrue(drain['journal_error'].startswith('OperationalError: '), drain)
        self.assertEqual(interval.invalid, [f"the journal could not be read: {drain['journal_error']}"])
        self.assertEqual((drain['runner_exit'], drain['socket_left'], drain['terminated']), (0, False, 0))

    def test_verify_recovery(self):
        path, _ = self.recovery_run()
        result = qualify.verify(path)
        self.assertEqual((result['verdict'], result['recomputed_verdict']), ('verified', 'inconclusive'), result)
        self.assertEqual(qualify.scenario_label(qualify.read_rows(path, 'scenarios')[1]),
                         'recovery-fence independent required - stale-epoch')
        # A stale mutation the record leaves out is found again from the raw restarts.
        restarts = path / 'independent-off-1/idle/restarts.jsonl'
        rows = qualify.read_rows(restarts.parent, 'restarts')
        rows[1]['fence']['requests'][0].update(ok=True, code=None)
        os.chmod(restarts, 0o600)
        restarts.write_text(''.join(json.dumps(row) + '\n' for row in rows))
        result = qualify.verify(path)
        self.assertEqual(result['verdict'], 'failed')
        for prefix in ('changed independent-off-1/idle/restarts.jsonl', 'independent-off-1 idle metrics',
                       'independent-off-1 idle verdict'):
            self.assertTrue(any(problem.startswith(prefix) for problem in result['problems']),
                            (prefix, result['problems']))


if __name__ == '__main__':
    unittest.main()

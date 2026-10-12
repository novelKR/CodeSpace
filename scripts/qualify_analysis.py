"""CSRG-U6 qualification analysis: pure functions over recorded samples, so that every verdict
can be recomputed from the raw evidence alone. Targets and rules follow DevGuard's SLO protocol
(DevGuard docs/planning/verification.md and scripts/measure.py) wherever they apply; nothing here
redefines them."""
import math

# The established targets (DevGuard docs/planning/verification.md, "SLO protocol").
STATUS_P99_MS = 500
TERMINATION_P99_MS = 1000
# Fewer samples than this share of an interval's schedule is not a complete observation.
MIN_SAMPLES = 0.9
# Terminations whose targets were refused admission for capacity or pressure may lower this share.
MIN_TERMINATIONS = 0.8
# Admitted load must run for at least this share of the load interval, and each consumer for at
# least its own share, with at least one successful run.
MIN_LOAD = 0.5
MIN_CONSUMER = 0.25
# The outcome code of a sample whose MCP connection was lost.
TRANSPORT = 'connection_lost'


def nearest_rank(values, fraction, missing=0):
    """Nearest-rank percentile of raw values. A missing sample ranks above every value."""
    count = len(values) + missing
    if count == 0:
        return None
    ordered = sorted(values) + [math.inf] * missing
    return ordered[max(1, math.ceil(fraction * count)) - 1]


def number(value):
    """A JSON-safe metric: a missing sample (infinite) is reported as the string "missing"."""
    if value is None:
        return None
    return 'missing' if math.isinf(value) else round(value, 3)


def worst(values, missing):
    return number(math.inf if missing else max(values, default=None))


def status_metrics(samples, expected):
    """`process_status` of a running governed target. An answer about a target that no longer
    runs, an error and a missed slot are missing samples; a lost connection fails."""
    answered = [s['latency_ms'] for s in samples if s['outcome'] == 'answered' and s.get('state') == 'running']
    not_running = sum(1 for s in samples if s['outcome'] == 'answered' and s.get('state') != 'running')
    errors = [s for s in samples if s['outcome'] == 'error']
    missed = sum(1 for s in samples if s['outcome'] == 'missed_slot')
    losses = sum(1 for s in errors if s.get('code') == TRANSPORT)
    missing = not_running + len(errors) + missed
    p99 = nearest_rank(answered, 0.99, missing)
    return {'samples': len(answered) + missing, 'expected': expected, 'answered': len(answered),
            'not_running': not_running, 'errors': len(errors), 'missed_slots': missed,
            'connection_losses': losses, 'p99_ms': number(p99), 'max_ms': worst(answered, missing),
            'passed': None if p99 is None else (p99 <= STATUS_P99_MS and losses == 0)}


def effective(sample):
    """An acknowledged termination after which CodeSpace reported the target terminated and its
    process group was observed gone."""
    return (sample['outcome'] == 'acknowledged' and sample.get('termination') == 'terminated'
            and sample.get('scope_exit_ms') is not None)


def termination_metrics(samples, expected, release_expected=True):
    """`terminate_process` of fresh governed targets. The acknowledgement is timed to the tool's
    answer; the scope's actual end (its process group gone) and DevGuard's release are reported
    separately and never combined with it. An acknowledgement that did not end its target fails
    outright; an error or a missed slot is a missing sample, a lost connection fails. A target
    refused admission for capacity or pressure is not a sample. Where DevGuard cannot release
    (its authority restarted or stopped), `release_expected` is false and an unreleased target
    is reported, not failed."""
    good = [s for s in samples if effective(s)]
    ineffective = sum(1 for s in samples if s['outcome'] == 'acknowledged' and not effective(s))
    errors = [s for s in samples if s['outcome'] == 'error']
    missed = sum(1 for s in samples if s['outcome'] == 'missed_slot')
    unstarted = sum(1 for s in samples if s['outcome'] == 'not_started')
    losses = sum(1 for s in errors if s.get('code') == TRANSPORT)
    missing = ineffective + len(errors) + missed
    acks = [s['ack_ms'] for s in good]
    exits = [s['scope_exit_ms'] for s in good]
    released = [s['released_ms'] for s in good if s.get('released_ms') is not None]
    unreleased = len(good) - len(released)
    p99 = nearest_rank(acks, 0.99, missing)
    return {'samples': len(good) + missing, 'expected': expected, 'effective': len(good),
            'ineffective': ineffective, 'errors': len(errors), 'missed_slots': missed, 'not_started': unstarted,
            'connection_losses': losses, 'ack_p99_ms': number(p99), 'ack_max_ms': worst(acks, missing),
            'scope_exit_p99_ms': number(nearest_rank(exits, 0.99)), 'scope_exit_max_ms': number(max(exits, default=None)),
            'release_p99_ms': number(nearest_rank(released, 0.99, unreleased)),
            'release_max_ms': worst(released, unreleased), 'unreleased': unreleased,
            'passed': (False if losses or ineffective or (unreleased and release_expected)
                       else None if p99 is None else p99 <= TERMINATION_P99_MS)}


def io_metrics(samples, expected):
    """Output and control responsiveness: a line written with `write_stdin` to a running governed
    `cat` until `read_process` returns it. There is no established numeric target, so the values
    are reported; a lost connection or a line that never came back fails."""
    echoed = [s['latency_ms'] for s in samples if s['outcome'] == 'echoed']
    lost_lines = sum(1 for s in samples if s['outcome'] == 'not_echoed')
    errors = [s for s in samples if s['outcome'] == 'error']
    missed = sum(1 for s in samples if s['outcome'] == 'missed_slot')
    losses = sum(1 for s in errors if s.get('code') == TRANSPORT)
    missing = lost_lines + len(errors) + missed
    return {'samples': len(echoed) + missing, 'expected': expected, 'echoed': len(echoed),
            'not_echoed': lost_lines, 'errors': len(errors), 'missed_slots': missed, 'connection_losses': losses,
            'p50_ms': number(nearest_rank(echoed, 0.5, missing)), 'p99_ms': number(nearest_rank(echoed, 0.99, missing)),
            'max_ms': worst(echoed, missing),
            'passed': False if losses or lost_lines else (None if not samples else True)}


def refusal_metrics(samples):
    """New-admission rejection latency: governed `exec_command` calls the authority must refuse
    (a shortage, Critical pressure or an unavailable authority). Reported against the design's
    250 ms preparation budget, which is a reference and not an established target. A call that
    ran instead of being refused is counted, and a lost connection fails."""
    refused = [s['latency_ms'] for s in samples if s['outcome'] == 'refused']
    codes = {}
    for sample in samples:
        if sample['outcome'] == 'refused':
            codes[sample.get('code')] = codes.get(sample.get('code'), 0) + 1
    errors = [s for s in samples if s['outcome'] == 'error']
    losses = sum(1 for s in errors if s.get('code') == TRANSPORT)
    admitted = sum(1 for s in samples if s['outcome'] == 'admitted')
    return {'samples': len(samples), 'refused': len(refused), 'codes': codes, 'admitted': admitted,
            'errors': len(errors), 'connection_losses': losses,
            'p50_ms': number(nearest_rank(refused, 0.5)), 'p99_ms': number(nearest_rank(refused, 0.99)),
            'max_ms': number(max(refused, default=None)),
            'passed': False if losses else (None if not refused else True)}


def with_releases(terminations, releases):
    """Each termination sample with the release time the journal watcher recorded for its slot."""
    found = {row.get('slot'): row.get('released_ms') for row in releases}
    return [dict(row, released_ms=found.get(row.get('slot'))) for row in terminations]


def connection_lost(*row_lists):
    """Whether any sample lost the MCP connection."""
    return any(row.get('code') == TRANSPORT for rows in row_lists for row in rows)


# The raw sample files of a control interval, one JSON line per sample.
SAMPLE_FILES = ('status_pipe', 'status_pty', 'terminations', 'releases', 'io', 'refusals')


def control_metrics(rows, elapsed, periods, release_expected=True, terminations_expected=None):
    """The control path's metrics from an interval's raw samples: `process_status` of a running
    governed pipe target and PTY target, terminations with their releases, I/O through the pipe
    target and governed requests the authority must refuse. `periods` gives each schedule's
    period in seconds; slots that never ran count against it."""
    status_expected = int(elapsed / periods['status'])
    if terminations_expected is None:
        terminations_expected = max(1, int(elapsed / periods['termination']))
    terminations = with_releases(rows['terminations'], rows['releases'])
    return {
        'status': status_metrics(rows['status_pipe'] + rows['status_pty'], 2 * status_expected),
        'status_pipe': status_metrics(rows['status_pipe'], status_expected),
        'status_pty': status_metrics(rows['status_pty'], status_expected),
        'termination': termination_metrics(terminations, terminations_expected, release_expected),
        'io': io_metrics(rows['io'], int(elapsed / periods['io'])),
        'refusal': refusal_metrics(rows['refusals']),
        'connection_lost': connection_lost(*(rows[name] for name in SAMPLE_FILES)),
    }


def covered(spans, started, ended):
    """The share of [started, ended] covered by the union of spans."""
    total, cursor = 0.0, started
    for begin, end in sorted((max(b, started), min(e, ended)) for b, e in spans):
        begin = max(begin, cursor)
        if end > begin:
            total += end - begin
            cursor = end
    return round(total / max(ended - started, 1e-9), 4)


def load_metrics(runs, started, ended, consumers=()):
    """What each load consumer did, the share of the interval in which its admitted work ran, and
    what makes the load not the declared one (issues) or wrong (failures), as DevGuard's protocol
    judges its own load."""
    entries = {name: [] for name in consumers}
    for run in runs:
        entries.setdefault(run['consumer'], []).append(run)
    report, issues = {}, []
    for name, own in sorted(entries.items()):
        ok = [r for r in own if r.get('result') == 'completed' and r.get('exit_code') == 0]
        spans = [(r['running_from'], r['ended']) for r in own if r.get('running_from') is not None]
        entry = {'runs': len(own), 'succeeded': len(ok),
                 'nonzero_exit': sum(1 for r in own if r.get('result') == 'completed' and r.get('exit_code') != 0),
                 'exec_failed': sum(1 for r in own if r.get('result') == 'exec_failed'),
                 'not_started': sum(1 for r in own if r.get('result') == 'not_started'),
                 'uncertain': sum(1 for r in own if r.get('result') == 'uncertain'),
                 'unknown': sum(1 for r in own if r.get('result') not in
                                ('completed', 'exec_failed', 'not_started', 'uncertain')),
                 'share': covered(spans, started, ended)}
        report[name] = entry
        if not ok:
            issues.append(f'{name}: no successful run')
        if entry['share'] < MIN_CONSUMER:
            issues.append(f"{name}: ran for {entry['share']:.0%} of the interval")
        for field in ('nonzero_exit', 'exec_failed', 'unknown'):
            if entry[field]:
                issues.append(f"{name}: {entry[field]} {field.replace('_', ' ')}")
    applied = covered([(r['running_from'], r['ended']) for r in runs if r.get('running_from') is not None],
                      started, ended)
    if applied < MIN_LOAD:
        issues.append(f'admitted load ran for {applied:.0%} of the interval')
    uncertain = sum(entry['uncertain'] for entry in report.values())
    failures = [f'{uncertain} load runs are uncertain'] if uncertain else []
    return {'consumers': report, 'applied_fraction': applied, 'uncertain': uncertain,
            'issues': issues, 'failures': failures}


def footprint_metrics(rows, reservation):
    """The execution owner's measured footprint (the gateway, and in `uds` mode its worker) against
    the static control reservation DevGuard holds for it. Reported, never a verdict: the
    reservation is an accounting estimate."""
    def peak(field):
        values = [row[field] for row in rows if row.get(field) is not None]
        return max(values, default=None)
    cpu = [row['cpu_percent'] for row in rows if row.get('cpu_percent') is not None]
    result = {'samples': len(rows), 'rss_bytes_max': peak('rss_bytes'), 'threads_max': peak('threads'),
              'processes_max': peak('processes'),
              'cpu_milli_mean': round(10 * sum(cpu) / len(cpu), 1) if cpu else None,
              'cpu_milli_max': round(10 * max(cpu), 1) if cpu else None, 'reservation': reservation}
    if reservation:
        result['within_reservation'] = {
            'memory': None if result['rss_bytes_max'] is None else result['rss_bytes_max'] <= reservation['memory_bytes'],
            'tasks': None if result['threads_max'] is None else result['threads_max'] <= reservation['tasks'],
            'cpu_mean': None if result['cpu_milli_mean'] is None else result['cpu_milli_mean'] <= reservation['cpu_milli']}
    return result


def host_peaks(rows):
    """Memory pressure levels, swap and load over an interval's host samples."""
    levels = {}
    for row in rows:
        levels[str(row.get('pressure_level'))] = levels.get(str(row.get('pressure_level')), 0) + 1
    swap = [row['swap_used_mib'] for row in rows if row.get('swap_used_mib') is not None]
    load = [row['load_average'][0] for row in rows if row.get('load_average')]
    return {'samples': len(rows), 'pressure_levels': levels, 'swap_used_mib_max': max(swap, default=None),
            'load_average_max': max(load, default=None)}


def service_findings(rows, release_id, service_pid):
    """(invalid, failure) from the service checks, as DevGuard's protocol judges them: another
    release selected or a check that could not be made is invalid observation; the measured
    release restarting or unhealthy is a failure."""
    invalid = []
    if not rows:
        invalid.append('the service was not observed')
    if any(row.get('error') for row in rows):
        invalid.append('a service check could not be made')
    if any(row.get('current') not in (None, release_id) for row in rows):
        invalid.append('another release was selected during the interval')
    ours = [row for row in rows if not row.get('error') and row.get('current') in (None, release_id)]
    failure = None
    if any(not row.get('runs_release') or row.get('service_pid') != service_pid for row in ours):
        failure = 'the release restarted or was unhealthy during the interval'
    return invalid, failure


def interval_verdict(kind, invalid, metrics):
    """inconclusive when the observation is invalid; otherwise fail when a target was missed or
    the accounting is wrong, and inconclusive when too little was observed or the load was not
    the declared one."""
    if invalid:
        return 'inconclusive', list(invalid)
    names = ('status', 'termination', 'io', 'refusal')
    failures = [name for name in names if metrics[name]['passed'] is False]
    if metrics.get('connection_lost'):
        failures.append('the MCP connection was lost')
    if (metrics.get('service') or {}).get('failure'):
        failures.append(metrics['service']['failure'])
    if metrics['termination'].get('status_target_effective') is False:
        failures.append("a status target's termination was not effective")
    if kind == 'load':
        failures += metrics['load']['failures']
    journal = metrics.get('journal') or {}
    if journal.get('still_charged'):
        failures.append('harness attempts still charged after the interval')
    if journal.get('duplicate_launches'):
        failures.append('an execution was launched more than once')
    if journal.get('readmitted_uncertain'):
        failures.append('an uncertain execution was admitted again')
    if failures:
        return 'fail', failures
    shortfalls = [f'{name}: nothing was measured' for name in ('status', 'termination') if metrics[name]['passed'] is None]
    status, termination = metrics['status'], metrics['termination']
    if status['samples'] < MIN_SAMPLES * status['expected']:
        shortfalls.append(f"status samples {status['samples']} of {status['expected']}")
    if termination['samples'] < MIN_TERMINATIONS * termination['expected']:
        shortfalls.append(f"termination samples {termination['samples']} of {termination['expected']}")
    if kind == 'load':
        shortfalls += metrics['load']['issues']
    return ('inconclusive', shortfalls) if shortfalls else ('pass', [])


def repetition_verdict(idle, load):
    """A failing idle baseline is inconclusive, never a failure of the load."""
    if idle['verdict'] != 'pass':
        return 'inconclusive', [f"idle baseline {idle['verdict']}"] + idle['reasons']
    if load['verdict'] != 'pass':
        return load['verdict'], load['reasons']
    return 'pass', []


def combination_verdict(verdicts, required):
    """Qualified only when every required repetition passed; values are never pooled."""
    if required < 1 or not verdicts:
        return 'inconclusive'
    if any(verdict == 'fail' for verdict in verdicts):
        return 'failed'
    if len(verdicts) >= required and all(verdict == 'pass' for verdict in verdicts):
        return 'qualified'
    return 'inconclusive'


def overall_verdict(combinations, plan, protocol, rehearsal):
    """The run's verdict: qualified only for the full protocol, with every combination qualified."""
    verdicts = [entry['verdict'] for entry in combinations.values()]
    full = (not rehearsal and set(plan['combinations']) >= set(protocol['combinations'])
            and plan['repetitions'] >= protocol['repetitions']
            and plan['idle_s'] >= protocol['idle_s'] and plan['load_s'] >= protocol['load_s'])
    if 'failed' in verdicts:
        return 'failed'
    if full and verdicts and all(verdict == 'qualified' for verdict in verdicts):
        return 'qualified'
    return 'inconclusive'


def check(name, passed, **detail):
    """One assertion of a functional scenario. `passed` None means it could not be judged."""
    return {'check': name, 'passed': None if passed is None else bool(passed), **detail}


def scenario_verdict(checks, not_run=None):
    """A scenario passes only when every check passed; a failed check fails it, and a check that
    could not be judged leaves it inconclusive."""
    if not_run:
        return 'not_run'
    if any(c['passed'] is False for c in checks):
        return 'fail'
    if not checks or any(c['passed'] is None for c in checks):
        return 'inconclusive'
    return 'pass'


def suite_verdict(verdicts):
    """A suite of scenarios: failed if any failed, passed only if every one that ran passed and
    at least one ran; scenarios that cannot run on this platform are not counted."""
    ran = [v for v in verdicts if v != 'not_run']
    if 'fail' in ran:
        return 'failed'
    if ran and all(v == 'pass' for v in ran):
        return 'passed'
    return 'incomplete'

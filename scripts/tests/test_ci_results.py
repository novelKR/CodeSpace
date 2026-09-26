import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_ci_results as check
import ci_plan

POLICY, DIGEST = ci_plan.read_policy()
SOURCE = 'a' * 40
ENV = {'GITHUB_SHA': SOURCE, 'GITHUB_EVENT_NAME': 'pull_request', 'GITHUB_RUN_ATTEMPT': '1'}


def affected(*paths):
    return ci_plan.build_plan(POLICY, DIGEST, 'pull_request', SOURCE, 'b' * 40, list(paths), set())


def results(plan):
    always = {leg.split('/')[0] for leg in POLICY['always']}
    return {job: {'result': 'success' if job == 'plan' or job in always or job in plan['jobs'] else 'skipped'}
            for job in check.REQUIRED_JOBS}


def upload(reports, leg, stages, attempt=1, status='passed', head=SOURCE):
    job, name = leg.split('/')
    path = reports / f'upstream-{job}-{name}-{attempt}' / 'stage' / 'report.json'
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps({'status': status, 'scope': stages, 'inputs': {'head': head},
                                'stages': [{'name': stage, 'status': 'passed'} for stage in stages]}))


class GateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.reports = Path(self.tmp.name)
        self.plan = affected('crates/pty/src/lib.rs')
        planned = dict(POLICY['always'], **{leg: POLICY['legs'][leg]['stages'] for leg in self.plan['legs']})
        for leg, stages in planned.items():
            upload(self.reports, leg, stages)

    def tearDown(self):
        self.tmp.cleanup()

    def problems(self, plan=None, jobs=None, env=None, digest=DIGEST):
        plan = plan or self.plan
        return check.problems(jobs or results(plan), plan, POLICY, digest, env or ENV, self.reports)

    def test_matching_run_passes(self):
        self.assertEqual(self.problems(), [])

    def test_full_run_passes(self):
        full = ci_plan.build_plan(POLICY, DIGEST, 'schedule', SOURCE, None, [], {'event:schedule'})
        for leg in full['legs']:
            if leg not in self.plan['legs']:
                upload(self.reports, leg, POLICY['legs'][leg]['stages'])
        self.assertEqual(self.problems(full, env=dict(ENV, GITHUB_EVENT_NAME='schedule')), [])

    def test_job_results_must_follow_the_plan(self):
        for job, result in (('rust-unit', 'failure'), ('policy-scan', 'skipped'), ('plan', 'failure'),
                            ('rust-linux-isolation', 'success'), ('rust-macos', 'cancelled')):
            with self.subTest(job=job, result=result):
                jobs = results(self.plan)
                jobs[job] = {'result': result}
                self.assertTrue(self.problems(jobs=jobs))

    def test_gate_must_see_exactly_the_required_jobs(self):
        jobs = results(self.plan)
        del jobs['rust-macos']
        self.assertTrue(self.problems(jobs=jobs))
        self.assertTrue(self.problems(jobs=dict(results(self.plan), extra={'result': 'success'})))

    def test_each_planned_leg_needs_its_passing_report(self):
        for leg, change in (('rust-unit/pty', 'missing'), ('rust-format/single', 'missing'),
                            ('rust-unit/pty', 'incomplete'), ('rust-macos/single', 'scope'),
                            ('rust-integration/single', 'head')):
            with self.subTest(leg=leg, change=change), tempfile.TemporaryDirectory() as tmp:
                reports = Path(tmp)
                planned = dict(POLICY['always'], **{name: POLICY['legs'][name]['stages'] for name in self.plan['legs']})
                for name, stages in planned.items():
                    if name != leg:
                        upload(reports, name, stages)
                    elif change == 'incomplete':
                        upload(reports, name, stages, status='incomplete')
                    elif change == 'scope':
                        upload(reports, name, stages[:1])
                    elif change == 'head':
                        upload(reports, name, stages, head='c' * 40)
                found = check.problems(results(self.plan), self.plan, POLICY, DIGEST, ENV, reports)
                self.assertTrue(any(line.startswith(leg) for line in found), found)

    def test_unplanned_report_fails(self):
        upload(self.reports, 'rust-linux-isolation/single', ['linux-isolation'])
        self.assertEqual(self.problems(), ['rust-linux-isolation/single: uploaded a report although it was not planned'])

    def test_latest_attempt_up_to_the_current_one_counts(self):
        upload(self.reports, 'rust-unit/pty', ['unit-pty'], attempt=2, status='failed')
        self.assertTrue(self.problems(env=dict(ENV, GITHUB_RUN_ATTEMPT='2')))
        self.assertEqual(self.problems(), [])
        upload(self.reports, 'rust-unit/pty', ['unit-pty'], attempt=3)
        self.assertEqual(self.problems(env=dict(ENV, GITHUB_RUN_ATTEMPT='3')), [])

    def test_plan_must_be_bound_to_the_run(self):
        schedule = ci_plan.build_plan(POLICY, DIGEST, 'schedule', SOURCE, None, [], set())
        cases = {
            'source': (self.plan, dict(ENV, GITHUB_SHA='d' * 40), DIGEST),
            'event': (self.plan, dict(ENV, GITHUB_EVENT_NAME='push'), DIGEST),
            'policy': (self.plan, ENV, '0' * 64),
            'schedule narrowed': (dict(schedule, legs=self.plan['legs'], jobs=self.plan['jobs']),
                                  dict(ENV, GITHUB_EVENT_NAME='schedule'), DIGEST),
            'unknown leg': (dict(self.plan, legs=self.plan['legs'] + ['rust-unit/other']), ENV, DIGEST),
            'jobs': (dict(self.plan, jobs=['rust-unit']), ENV, DIGEST),
        }
        for label, (plan, env, digest) in cases.items():
            with self.subTest(case=label):
                self.assertEqual(self.problems(plan, env=env, digest=digest),
                                 ['the plan is not bound to this source, event and policy'])


if __name__ == '__main__':
    unittest.main()

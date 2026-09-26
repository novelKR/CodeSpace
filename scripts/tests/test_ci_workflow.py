from pathlib import Path
import re
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_ci_results
import ci_plan
from upstream_dependencies import ROOT

WORKFLOW = (ROOT / '.github' / 'workflows' / 'ci.yml').read_text()
POLICY, _ = ci_plan.read_policy()
RUST_CACHE = 'uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2'
COMPILE_JOBS = {leg.split('/')[0] for leg in POLICY['legs']}
MATRIX_JOBS = {leg.split('/')[0] for leg in POLICY['legs'] if not leg.endswith('/single')}


def jobs(text):
    body = text.split('\njobs:\n', 1)[1]
    parts = re.split(r'^  ([a-z0-9-]+):\n', body, flags=re.M)
    return dict(zip(parts[1::2], parts[2::2]))


def run_line(stages):
    return 'python3 scripts/validate-upstream.py ' + ' '.join(stages) + ' --output'


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.jobs = jobs(WORKFLOW)

    def test_jobs_are_the_gate_requirements(self):
        self.assertEqual(set(self.jobs), check_ci_results.REQUIRED_JOBS | {'rust'})
        always = {leg.split('/')[0] for leg in POLICY['always']}
        self.assertEqual(check_ci_results.REQUIRED_JOBS, {'plan'} | always | COMPILE_JOBS)

    def test_gate_is_unskippable_and_checks_every_job(self):
        gate = self.jobs['rust']
        self.assertIn('    if: ${{ always() }}\n', gate)
        needs = re.search(r'    needs:\n((?:      - [a-z-]+\n)+)', gate).group(1)
        self.assertEqual(set(re.findall(r'- ([a-z-]+)', needs)), check_ci_results.REQUIRED_JOBS)
        self.assertIn('CI_RESULTS: ${{ toJSON(needs) }}', gate)
        self.assertIn('CI_PLAN: ${{ needs.plan.outputs.plan }}', gate)
        self.assertIn('scripts/check_ci_results.py --reports target/ci-reports', gate)
        self.assertIn('pattern: upstream-*', gate)

    def test_plan_exports_every_flag_and_matrix(self):
        plan = self.jobs['plan']
        self.assertIn('fetch-depth: 0', plan)
        self.assertIn('run: python3 -B scripts/ci_plan.py', plan)
        outputs = ['plan', *sorted(COMPILE_JOBS), *(job + '-matrix' for job in sorted(MATRIX_JOBS))]
        for output in outputs:
            self.assertIn(f'      {output}: ${{{{ steps.plan.outputs.{output} }}}}\n', plan)

    def test_compile_jobs_follow_the_plan(self):
        for job in COMPILE_JOBS:
            text = self.jobs[job]
            with self.subTest(job=job):
                self.assertIn('    needs: plan\n', text)
                self.assertIn(f"    if: ${{{{ needs.plan.outputs.{job} == 'true' }}}}\n", text)
                if job in MATRIX_JOBS:
                    self.assertIn(f'matrix: ${{{{ fromJSON(needs.plan.outputs.{job}-matrix) }}}}', text)
                    self.assertIn('validate-upstream.py ${{ matrix.stage }} --output', text)
                    self.assertIn('key: ${{ matrix.name }}', text)
                    self.assertIn('workspaces: ${{ matrix.cache-workspace }}', text)
                else:
                    spec = POLICY['legs'][job + '/single']
                    self.assertIn(run_line(spec['stages']), text)
                    self.assertIn(f'workspaces: "{ci_plan.anchor(spec["cache"])}"', text)

    def test_always_on_jobs_run_their_stages(self):
        for leg, stages in POLICY['always'].items():
            text = self.jobs[leg.split('/')[0]]
            with self.subTest(leg=leg):
                self.assertNotIn('needs:', text)
                self.assertIn(run_line(stages), text)

    def test_one_pinned_main_only_cache_per_compile_job(self):
        for name, job in self.jobs.items():
            uses = [line.strip() for line in job.splitlines() if 'rust-cache@' in line]
            with self.subTest(job=name):
                if name not in COMPILE_JOBS:
                    self.assertEqual(uses, [])
                    continue
                self.assertEqual(uses, [RUST_CACHE])
                self.assertIn("save-if: ${{ github.ref == 'refs/heads/main' }}", job)
                self.assertIn('cache-bin: "false"', job)

    def test_no_shared_cargo_cache_or_checkout_walk(self):
        self.assertNotIn('actions/cache@', WORKFLOW)
        self.assertIsNone(re.search(r"hashFiles\([^)]*\*\*", WORKFLOW))

    def test_triggers_permissions_and_concurrency(self):
        header = WORKFLOW.split('\njobs:\n', 1)[0]
        self.assertIn("  schedule:\n    - cron: '43 19 * * *'\n  workflow_dispatch:\n", header)
        self.assertIn('permissions:\n  contents: read\n', header)
        self.assertIn("format('pr-{0}', github.event.pull_request.number) || format('run-{0}', github.run_id)", header)
        self.assertIn("cancel-in-progress: ${{ github.event_name == 'pull_request' }}", header)
        self.assertIn('\n  CARGO_INCREMENTAL: "0"\n', header)
        self.assertIn('\n  CARGO_PROFILE_DEV_DEBUG: "0"\n', header)


if __name__ == '__main__':
    unittest.main()

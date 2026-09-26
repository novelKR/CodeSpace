import importlib.util
import os
from pathlib import Path
import re
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from upstream_dependencies import ADAPTERS, ROOT
spec = importlib.util.spec_from_file_location('validation', ROOT / 'scripts' / 'validate-upstream.py')
validation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validation)

WORKFLOW = (ROOT / '.github' / 'workflows' / 'ci.yml').read_text()
RUST_CACHE = 'uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2'
COMPILE_JOBS = {'rust-clippy', 'rust-unit', 'rust-linux-isolation', 'rust-integration', 'rust-macos'}
SHARED_TARGET = 'target/upstream-validation'


def jobs(text):
    body = text.split('\njobs:\n', 1)[1]
    parts = re.split(r'^  ([a-z0-9-]+):\n', body, flags=re.M)
    return dict(zip(parts[1::2], parts[2::2]))


def legs(job):
    return re.findall(r'- name: (\S+)\n +stage: (.+)\n +cache-workspace: "([^"]+)"', job)


class RustCacheTests(unittest.TestCase):
    def setUp(self):
        self.jobs = jobs(WORKFLOW)

    def test_job_split(self):
        self.assertEqual(set(self.jobs), COMPILE_JOBS | {'policy-scan', 'rust-format', 'rust'})

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

    def test_build_settings(self):
        header = WORKFLOW.split('\njobs:\n', 1)[0]
        self.assertIn('\n  CARGO_INCREMENTAL: "0"\n', header)
        self.assertIn('\n  CARGO_PROFILE_DEV_DEBUG: "0"\n', header)

    def test_matrix_legs_are_keyed_and_anchored(self):
        for name in ('rust-clippy', 'rust-unit'):
            job = self.jobs[name]
            with self.subTest(job=name):
                self.assertIn('key: ${{ matrix.name }}', job)
                self.assertIn('workspaces: ${{ matrix.cache-workspace }}', job)
                matrix = job.split('\n    steps:\n', 1)[0]
                self.assertEqual(len(legs(job)), matrix.count('- name: '))

    def test_anchor_is_one_workspace_on_the_shared_target(self):
        anchors = re.findall(r'(?:workspaces|cache-workspace): "([^"]+)"', WORKFLOW)
        self.assertEqual(len(anchors), 3 + 6 + 3)
        self.assertIn("ROOT / 'target' / 'upstream-validation'", (ROOT / 'scripts' / 'validate-upstream.py').read_text())
        roots = {'.'} | {f'crates/{area}' for area in ADAPTERS}
        for anchor in anchors:
            with self.subTest(anchor=anchor):
                workspace, target = (part.strip() for part in anchor.split('->'))
                self.assertIn(workspace, roots)
                self.assertTrue((ROOT / workspace / 'Cargo.lock').is_file())
                self.assertEqual(os.path.normpath(os.path.join(workspace, target)), SHARED_TARGET)

    def test_legs_run_known_stages(self):
        known = set(validation.stages())
        units = legs(self.jobs['rust-unit'])
        self.assertEqual({stage for _, stage, _ in units},
                         {f'unit-{area}' for area in ADAPTERS} | {'unit-linux-sandbox-protocol'})
        for name, stage, _ in units:
            self.assertEqual(stage, f'unit-{name}')
        for _, stage, _ in legs(self.jobs['rust-clippy']):
            self.assertLessEqual(set(stage.split()), known)


if __name__ == '__main__':
    unittest.main()

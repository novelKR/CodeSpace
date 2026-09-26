import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import ci_plan
from upstream_dependencies import ADAPTERS, ROOT
spec = importlib.util.spec_from_file_location('validation', ROOT / 'scripts' / 'validate-upstream.py')
validation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validation)

POLICY, DIGEST = ci_plan.read_policy()
ALL = list(POLICY['legs'])
PTY = ['rust-clippy/root', 'rust-clippy/adapters', 'rust-clippy/codex-adapters', 'rust-unit/codex-runtime',
       'rust-unit/pty', 'rust-integration/single', 'rust-macos/single']


def plan(*paths):
    return ci_plan.build_plan(POLICY, DIGEST, 'local', 'f' * 40, 'e' * 40, list(paths), set())


def ordered(*legs):
    return [leg for leg in ALL if leg in legs]


class SelectionTests(unittest.TestCase):
    def test_documentation_selects_no_leg(self):
        docs = plan('docs/guide.md', 'docs/ko/guide.md', 'docs-site/src/index.md', 'README.md', 'README.ko.md',
                    'LICENSE', 'NOTICE', '.env.example', '.codex/config.toml')
        self.assertEqual((docs['profile'], docs['legs'], docs['jobs']), ('affected', [], []))

    def test_pty_selects_every_leg_that_compiles_it(self):
        self.assertEqual(plan('crates/pty/src/lib.rs')['legs'], PTY)

    def test_components_follow_the_crate_graph(self):
        rows = {
            'crates/domain/src/lib.rs': ordered('rust-clippy/root', 'rust-clippy/adapters', 'rust-clippy/codex-adapters',
                                                'rust-unit/patch', 'rust-unit/codex-runtime', 'rust-integration/single'),
            'crates/runner/src/lib.rs': ordered('rust-clippy/root', 'rust-clippy/codex-adapters',
                                                'rust-unit/codex-runtime', 'rust-integration/single'),
            'crates/server/src/main.rs': ['rust-clippy/root', 'rust-integration/single'],
            'crates/store/src/lib.rs': ['rust-clippy/root', 'rust-integration/single'],
            'crates/linux-sandbox-protocol/src/lib.rs': ordered(
                'rust-clippy/root', 'rust-clippy/codex-adapters', 'rust-unit/codex-runtime', 'rust-unit/linux-sandbox-protocol',
                'rust-unit/linux-sandbox', 'rust-linux-isolation/single', 'rust-integration/single'),
            'crates/patch/src/lib.rs': ['rust-clippy/adapters', 'rust-unit/patch', 'rust-integration/single'],
            'crates/codex-runtime/src/lib.rs': ['rust-clippy/codex-adapters', 'rust-unit/codex-runtime',
                                                'rust-integration/single'],
            'crates/file-system/src/lib.rs': [leg.replace('unit/pty', 'unit/file-system') for leg in PTY],
            'crates/linux-sandbox/src/main.rs': ['rust-clippy/codex-adapters', 'rust-unit/linux-sandbox',
                                                 'rust-linux-isolation/single', 'rust-integration/single'],
        }
        rows['deploy/Dockerfile'] = rows['crates/runner/src/lib.rs']
        rows['tests/e2e/flow.rs'] = rows['crates/server/src/main.rs']
        for path, legs in rows.items():
            with self.subTest(path=path):
                self.assertEqual(plan(path)['legs'], legs)

    def test_build_inputs_ci_files_and_unknown_paths_run_everything(self):
        for path in ('Cargo.toml', 'Cargo.lock', 'crates/patch/Cargo.toml', 'crates/pty/Cargo.lock',
                     'rust-toolchain.toml', 'crates/pty/.cargo/config.toml', 'third_party/codex', '.gitmodules',
                     '.gitignore', '.github/workflows/docs.yml', 'scripts/check_docs.py', 'docs/upstream-lock.md',
                     'Makefile'):
            with self.subTest(path=path):
                full = plan('docs/guide.md', path)
                self.assertEqual((full['profile'], full['legs']), ('full', ALL))
        self.assertIn('unclassified:Makefile', plan('Makefile')['reasons'])

    def test_empty_diff_runs_everything(self):
        self.assertEqual(plan()['reasons'], ['empty-diff'])

    def test_globs_respect_segments(self):
        self.assertTrue(ci_plan.matches(['**/Cargo.lock'], 'Cargo.lock'))
        self.assertTrue(ci_plan.matches(['docs/**'], 'docs/ko/a.md'))
        self.assertFalse(ci_plan.matches(['README.md'], 'crates/pty/README.md'))
        self.assertFalse(ci_plan.matches(['crates/*/src'], 'crates/a/b/src'))


class Repo:
    """A throwaway repository holding the real planning inputs."""

    def __init__(self, root):
        self.root = Path(root)
        self.git('init', '-q', '-b', 'main')
        for path in ci_plan.PLANNING:
            self.write(path, (ROOT / path).read_text())
        self.write('docs/guide.md', 'guide\n')
        self.base = self.commit('base')

    def git(self, *args):
        env = dict(os.environ, GIT_AUTHOR_NAME='CI', GIT_AUTHOR_EMAIL='ci@example.invalid',
                   GIT_COMMITTER_NAME='CI', GIT_COMMITTER_EMAIL='ci@example.invalid')
        command = ['git', '-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=/dev/null', *args]
        return subprocess.run(command, cwd=self.root, env=env, check=True, capture_output=True).stdout.decode().strip()

    def write(self, path, text):
        (self.root / path).parent.mkdir(parents=True, exist_ok=True)
        (self.root / path).write_text(text)

    def commit(self, message):
        self.git('add', '-A')
        self.git('commit', '-q', '--allow-empty', '-m', message)
        return self.git('rev-parse', 'HEAD')

    def pull_request(self, path, text='changed\n'):
        self.git('checkout', '-q', '-b', 'topic', self.base)
        self.write(path, text)
        head = self.commit('head')
        self.git('checkout', '-q', 'main')
        self.git('merge', '-q', '--no-ff', '-m', 'merge', 'topic')
        return head, self.git('rev-parse', 'HEAD')

    def prepare(self, event, name, sha=None):
        env = {'GITHUB_EVENT_NAME': name, 'GITHUB_SHA': sha or self.git('rev-parse', 'HEAD')}
        return ci_plan.prepare(self.root, env, event)


class EventTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Repo(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_pull_request_plans_its_merge_against_the_base(self):
        head, merge = self.repo.pull_request('crates/pty/src/lib.rs')
        result = self.repo.prepare({'pull_request': {'head': {'sha': head}}}, 'pull_request')
        self.assertEqual((result['source_sha'], result['base_sha']), (merge, self.repo.base))
        self.assertEqual((result['profile'], result['legs']), ('affected', PTY))
        self.assertEqual(result['policy_sha256'], DIGEST)

    def test_changed_planning_input_runs_everything(self):
        head, _ = self.repo.pull_request(ci_plan.POLICY, (ROOT / ci_plan.POLICY).read_text() + '\n')
        result = self.repo.prepare({'pull_request': {'head': {'sha': head}}}, 'pull_request')
        self.assertIn('planning-changed', result['reasons'])
        self.assertEqual(result['legs'], ALL)

    def test_pull_request_checkout_must_match_the_event(self):
        head, merge = self.repo.pull_request('docs/guide.md')
        with self.assertRaises(ci_plan.PlanError):
            self.repo.prepare({'pull_request': {'head': {'sha': self.repo.base}}}, 'pull_request')
        with self.assertRaises(ci_plan.PlanError):
            self.repo.prepare({'pull_request': {'head': {'sha': head}}}, 'pull_request', sha=head)

    def test_push_plans_before_to_after(self):
        self.repo.write('docs/guide.md', 'more\n')
        after = self.repo.commit('docs')
        result = self.repo.prepare({'before': self.repo.base}, 'push')
        self.assertEqual((result['source_sha'], result['profile'], result['legs']), (after, 'affected', []))

    def test_push_without_a_trusted_base_runs_everything(self):
        self.repo.write('docs/guide.md', 'more\n')
        self.repo.commit('docs')
        self.repo.git('checkout', '-q', '--orphan', 'other')
        unrelated = self.repo.commit('unrelated')
        self.repo.git('checkout', '-q', 'main')
        for event in ({'before': '0' * 40}, {'before': '1' * 40}, {'before': unrelated}, {},
                      {'before': self.repo.base, 'forced': True}):
            with self.subTest(event=event):
                result = self.repo.prepare(event, 'push')
                self.assertEqual((result['reasons'], result['legs']), (['push-base-untrusted'], ALL))

    def test_scheduled_and_manual_runs_are_full(self):
        for name in ('schedule', 'workflow_dispatch'):
            with self.subTest(event=name):
                result = self.repo.prepare({}, name)
                self.assertEqual((result['profile'], result['base_sha'], result['legs']), ('full', None, ALL))

    def test_other_events_are_refused(self):
        with self.assertRaises(ci_plan.PlanError):
            self.repo.prepare({}, 'issue_comment')


class OutputTests(unittest.TestCase):
    def outputs(self, *paths):
        lines = ci_plan.outputs(POLICY, plan(*paths))
        return {key: value for key, value in (line.split('=', 1) for line in lines)}

    def test_flags_and_matrices_carry_only_selected_legs(self):
        out = self.outputs('crates/pty/src/lib.rs')
        self.assertEqual(json.loads(out['plan']), plan('crates/pty/src/lib.rs'))
        self.assertEqual({job: out[job] for job in ('rust-clippy', 'rust-unit', 'rust-linux-isolation',
                                                    'rust-integration', 'rust-macos')},
                         {'rust-clippy': 'true', 'rust-unit': 'true', 'rust-linux-isolation': 'false',
                          'rust-integration': 'true', 'rust-macos': 'true'})
        unit = json.loads(out['rust-unit-matrix'])['include']
        self.assertEqual(unit, [
            {'name': 'codex-runtime', 'stage': 'unit-codex-runtime',
             'cache-workspace': 'crates/codex-runtime -> ../../target/upstream-validation'},
            {'name': 'pty', 'stage': 'unit-pty', 'cache-workspace': 'crates/pty -> ../../target/upstream-validation'}])
        clippy = json.loads(out['rust-clippy-matrix'])['include']
        self.assertEqual(clippy[0], {'name': 'root', 'stage': 'clippy-root dependencies',
                                     'cache-workspace': '. -> target/upstream-validation'})

    def test_documentation_selects_no_job(self):
        out = self.outputs('docs/guide.md')
        self.assertEqual({out[job] for job in ('rust-clippy', 'rust-unit', 'rust-linux-isolation', 'rust-integration',
                                                'rust-macos')}, {'false'})
        self.assertEqual(json.loads(out['rust-unit-matrix']), {'include': []})


def path_deps(crate):
    return set(re.findall(r'path = "\.\./([a-z-]+)"', (ROOT / 'crates' / crate / 'Cargo.toml').read_text()))


def closure(crates):
    seen, todo = set(), list(crates)
    while todo:
        crate = todo.pop()
        if crate not in seen:
            seen.add(crate)
            todo += path_deps(crate)
    return seen


def compiled(stage):
    """Crates a validate-upstream stage compiles, read from its cargo commands."""
    if stage == 'dependencies':
        return set()
    members = re.search(r'members = \[([^\]]*)\]', (ROOT / 'Cargo.toml').read_text()).group(1)
    root = {name.split('/')[-1] for name in re.findall(r'"([^"]+)"', members)}
    commands = validation.stages()[stage]
    if stage == 'integration':
        commands = [validation.cargo('build', area) for area in validation.HELPERS] + [validation.cargo('test')]
    crates = set()
    for command in commands:
        manifest = [arg for arg in command if arg.startswith('crates/') and arg.endswith('/Cargo.toml')]
        package = command[command.index('-p') + 1] if '-p' in command else None
        if manifest:
            crates.add(manifest[0].split('/')[1])
        elif package:
            crates.add(package.removeprefix('codespace-'))
        else:
            crates |= root
    return closure(crates)


class PolicyTests(unittest.TestCase):
    def test_stages_cover_the_validation_entry_point(self):
        stages = [stage for spec in POLICY['legs'].values() for stage in spec['stages']]
        stages += [stage for always in POLICY['always'].values() for stage in always]
        self.assertLessEqual(set(stages), set(validation.stages()))
        self.assertEqual(set(stages), set(validation.all_stages()) | {'macos-core'})

    def test_compiles_matches_the_manifest_graph(self):
        for leg, spec in POLICY['legs'].items():
            with self.subTest(leg=leg):
                expected = set().union(*(compiled(stage) for stage in spec['stages']))
                self.assertEqual(set(spec['compiles']), expected)
                self.assertEqual(spec['compiles'], sorted(spec['compiles']))

    def test_cache_anchor_is_a_workspace_root(self):
        roots = {'.'} | {f'crates/{area}' for area in ADAPTERS}
        for leg, spec in POLICY['legs'].items():
            with self.subTest(leg=leg):
                self.assertIn(spec['cache'], roots)
                self.assertTrue((ROOT / spec['cache'] / 'Cargo.lock').is_file())

    def test_every_tracked_path_is_classified(self):
        paths = subprocess.run(['git', 'ls-files', '-z'], cwd=ROOT, check=True, capture_output=True).stdout
        _, reasons = ci_plan.classify(POLICY, [path for path in paths.decode().split('\0') if path])
        self.assertEqual([reason for reason in reasons if reason.startswith('unclassified:')], [])

    def test_components_are_the_crates(self):
        crates = sorted(path.name for path in (ROOT / 'crates').iterdir() if (path / 'Cargo.toml').is_file())
        self.assertEqual(sorted(POLICY['components']), crates)


if __name__ == '__main__':
    unittest.main()

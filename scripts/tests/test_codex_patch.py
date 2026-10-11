import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import codex_patch
from upstream_dependencies import ROOT

GIT_ENV = dict(os.environ, GIT_AUTHOR_NAME='t', GIT_AUTHOR_EMAIL='t@example.invalid',
               GIT_COMMITTER_NAME='t', GIT_COMMITTER_EMAIL='t@example.invalid',
               GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1')


def git(cwd, *args):
    return subprocess.run(['git', *args], cwd=cwd, env=GIT_ENV, check=True,
                          capture_output=True, text=True).stdout.strip()


def lock(commit, patches=(), tree=None):
    text = '# Pin\n\n## Deployment pin\n\n| Field | Value |\n| --- | --- |\n'
    text += f'| Commit | `{commit}` |\n| Patch options | none |\n\n'
    if patches:
        text += '## Downstream patch\n\n| Field | Value |\n| --- | --- |\n'
        for path, digest in patches:
            text += f'| Patch | `{path}`, SHA-256 `{digest}` |\n'
        text += f'| Patched tree | `{tree}` |\n'
    # A later table with the same field names is not the pin.
    return text + '\n## Other pin\n\n| Field | Value |\n| --- | --- |\n| Commit | `' + 'f' * 40 + '` |\n'


class CodexPatchTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.root = Path(self.scratch.name)
        self.checkout = self.root / 'third_party' / 'codex'
        self.checkout.mkdir(parents=True)
        git(self.checkout, 'init', '-q')
        (self.checkout / 'pty.rs').write_text('fn spawn() {}\n')
        (self.checkout / '.gitignore').write_text('target/\n')
        git(self.checkout, 'add', '-A')
        git(self.checkout, 'commit', '-q', '-m', 'release')
        self.commit = git(self.checkout, 'rev-parse', 'HEAD')
        self.pinned = git(self.checkout, 'rev-parse', 'HEAD^{tree}')
        # The patch, made the way the record's patch was: one commit, exported, then undone.
        (self.checkout / 'pty.rs').write_text('fn spawn() {}\nfn spawn_observed() {}\n')
        (self.checkout / 'observe.rs').write_text('pub struct ExitObserver;\n')
        git(self.checkout, 'add', '-A')
        git(self.checkout, 'commit', '-q', '-m', 'observe')
        self.patched = git(self.checkout, 'rev-parse', 'HEAD^{tree}')
        patches = self.root / 'third_party' / 'codex-patches'
        patches.mkdir()
        self.patch = patches / '0001-observe.patch'
        self.patch.write_text(git(self.checkout, 'format-patch', '-1', '--stdout') + '\n')
        git(self.checkout, 'reset', '-q', '--hard', self.commit)
        self.digest = hashlib.sha256(self.patch.read_bytes()).hexdigest()
        self.lock = self.root / 'upstream-lock.md'
        self.lock.write_text(lock(self.commit, [('third_party/codex-patches/0001-observe.patch', self.digest)],
                                  self.patched))

    def tearDown(self):
        self.scratch.cleanup()

    def record(self):
        return codex_patch.read_record(self.lock, self.root)

    def test_record_reads_the_pin_tables_only(self):
        record = self.record()
        self.assertEqual(record.commit, self.commit)
        self.assertEqual(record.patches, ((self.patch, self.digest),))
        self.assertEqual(record.tree, self.patched)

    def test_apply_reaches_the_recorded_tree_once(self):
        with self.assertRaises(codex_patch.PatchStateError):
            codex_patch.check(self.record(), self.checkout)
        self.assertIn('Applied', codex_patch.apply(self.record(), self.checkout))
        self.assertEqual(codex_patch.working_tree(self.checkout), self.patched)
        self.assertIn('already', codex_patch.apply(self.record(), self.checkout))
        codex_patch.check(self.record(), self.checkout)
        # The checkout's own index and commit stay those of the pinned release.
        self.assertEqual(git(self.checkout, 'rev-parse', 'HEAD'), self.commit)
        self.assertEqual(git(self.checkout, 'diff', '--cached', '--name-only'), '')

    def test_ignored_files_do_not_count(self):
        codex_patch.apply(self.record(), self.checkout)
        (self.checkout / 'target').mkdir()
        (self.checkout / 'target' / 'build.o').write_text('object')
        codex_patch.check(self.record(), self.checkout)

    def test_revert_restores_the_pinned_files(self):
        codex_patch.apply(self.record(), self.checkout)
        codex_patch.revert(self.record(), self.checkout)
        self.assertEqual(codex_patch.working_tree(self.checkout), self.pinned)
        self.assertFalse((self.checkout / 'observe.rs').exists())
        self.assertIn('unpatched', codex_patch.revert(self.record(), self.checkout))

    def test_other_changes_are_refused(self):
        (self.checkout / 'stray.rs').write_text('fn stray() {}\n')
        with self.assertRaisesRegex(codex_patch.PatchStateError, 'changes the record does not hold'):
            codex_patch.apply(self.record(), self.checkout)
        (self.checkout / 'stray.rs').unlink()
        codex_patch.apply(self.record(), self.checkout)
        (self.checkout / 'observe.rs').write_text('pub struct Other;\n')
        with self.assertRaises(codex_patch.PatchStateError):
            codex_patch.check(self.record(), self.checkout)

    def test_a_changed_patch_file_is_refused(self):
        self.patch.write_text(self.patch.read_text() + '\n')
        with self.assertRaisesRegex(codex_patch.PatchStateError, 'SHA-256'):
            codex_patch.apply(self.record(), self.checkout)
        self.assertEqual(codex_patch.working_tree(self.checkout), self.pinned)

    def test_another_commit_is_refused(self):
        (self.checkout / 'pty.rs').write_text('fn spawn() { /* newer */ }\n')
        git(self.checkout, 'commit', '-q', '-am', 'newer release')
        with self.assertRaisesRegex(codex_patch.PatchStateError, 'not the pinned'):
            codex_patch.apply(self.record(), self.checkout)

    def test_without_a_recorded_patch_the_checkout_must_be_clean(self):
        self.lock.write_text(lock(self.commit))
        record = self.record()
        self.assertEqual((record.patches, record.tree), ((), None))
        codex_patch.apply(record, self.checkout)
        codex_patch.check(record, self.checkout)
        (self.checkout / 'pty.rs').write_text('fn changed() {}\n')
        with self.assertRaises(codex_patch.PatchStateError):
            codex_patch.check(record, self.checkout)

    def test_a_patch_without_a_tree_is_refused(self):
        self.lock.write_text(lock(self.commit, [('third_party/codex-patches/0001-observe.patch', self.digest)])
                             .replace('| Patched tree | `None` |\n', ''))
        with self.assertRaisesRegex(codex_patch.PatchStateError, 'patched tree'):
            self.record()


class RepositoryRecordTests(unittest.TestCase):
    def test_the_record_names_the_gitlink_and_existing_patches(self):
        record = codex_patch.read_record()
        gitlink = subprocess.run(['git', 'ls-tree', 'HEAD', 'third_party/codex'], cwd=ROOT, check=True,
                                 capture_output=True, text=True).stdout.split()
        self.assertEqual(gitlink[:3], ['160000', 'commit', record.commit])
        for path, digest in record.patches:
            self.assertTrue(path.is_relative_to(ROOT / 'third_party' / 'codex-patches'))
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), digest)
        self.assertEqual(bool(record.patches), record.tree is not None)


if __name__ == '__main__':
    unittest.main()

import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import upstream_dependencies as deps
spec = importlib.util.spec_from_file_location('validation', Path(__file__).resolve().parents[1] / 'validate-upstream.py')
validation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validation)


def fixture(names, edges, sources=None):
    """Codex packages sit in the gitlink, others in `crates/`, unless `sources` names another."""
    def manifest(n):
        parent = deps.ROOT / 'third_party/codex/codex-rs' if n.startswith('codex-') else deps.ROOT / 'crates'
        return str(parent / n / 'Cargo.toml')
    return {'packages': [{'id': n, 'name': n, 'version': '1', 'source': (sources or {}).get(n),
                           'manifest_path': manifest(n)} for n in names],
            'resolve': {'nodes': [{'id': n, 'deps': [
                {'name': 'renamed_alias', 'pkg': child, 'dep_kinds': [{'kind': kind, 'target': None}]}
                for parent, child, kind in edges if parent == n]} for n in names]}}


class GraphTests(unittest.TestCase):
    def test_transitive_and_renamed_build_edge(self):
        data = fixture(['app', 'adapter', 'codex-core'], [('app', 'adapter', None), ('adapter', 'codex-core', 'build')])
        self.assertEqual(deps.graph(data, ['app'])['violations'], ['app -> adapter -> codex-core'])

    def test_development_not_product(self):
        data = fixture(['app', 'codex-core'], [('app', 'codex-core', 'dev')])
        self.assertFalse(deps.graph(data, ['app'])['violations'])
        self.assertEqual(len(deps.graph(data, ['app'])['packages']), 1)

    def test_runner_boundary(self):
        data = fixture(['codespace-runner', 'codex-linux-sandbox'], [('codespace-runner', 'codex-linux-sandbox', None)])
        self.assertTrue(deps.graph(data, ['codespace-runner'])['violations'])

    def test_direct_and_missing_roots(self):
        data = fixture(['app', 'codex-login'], [('app', 'codex-login', None)])
        self.assertTrue(deps.graph(data, ['app'])['violations'])
        with self.assertRaises(ValueError):
            deps.graph(data, ['absent'])
        with self.assertRaises(ValueError):
            deps.graph(data, [])

    def test_devguard_and_codex_each_have_one_source(self):
        pinned = {'devguard-client': deps.DEVGUARD_SOURCE}
        moved = {'devguard-client': deps.DEVGUARD_SOURCE.replace('6e7e065', 'e0e0e0e')}
        edges = [('codespace-server', 'devguard-client', None)]
        self.assertFalse(deps.graph(fixture(['codespace-server', 'devguard-client'], edges, pinned),
                                    ['codespace-server'], devguard=True)['violations'])
        self.assertEqual(deps.graph(fixture(['codespace-server', 'devguard-client'], edges, moved),
                                    ['codespace-server'], devguard=True)['violations'],
                         ['codespace-server -> devguard-client: not the reviewed DevGuard pin'])
        registry = {'codex-utils-pty': 'registry+https://github.com/rust-lang/crates.io-index'}
        data = fixture(['codespace-pty', 'codex-utils-pty'], [('codespace-pty', 'codex-utils-pty', None)], registry)
        self.assertEqual(deps.graph(data, ['codespace-pty'])['violations'],
                         ['codespace-pty -> codex-utils-pty: not the Codex gitlink'])

    def test_devguard_only_with_the_feature(self):
        data = fixture(['codespace-server', 'codespace-devguard', 'devguard-client'],
                       [('codespace-server', 'codespace-devguard', None), ('codespace-devguard', 'devguard-client', None)],
                       {'devguard-client': deps.DEVGUARD_SOURCE})
        self.assertEqual(deps.graph(data, ['codespace-server'])['violations'], [
            'codespace-server -> codespace-devguard -> devguard-client: DevGuard outside the devguard feature',
            'codespace-server -> codespace-devguard: DevGuard outside the devguard feature'])
        self.assertFalse(deps.graph(data, ['codespace-server'], devguard=True)['violations'])

    def test_devguard_brings_no_codespace_or_codex(self):
        names = ['codespace-devguard', 'devguard-client', 'serde', 'codex-protocol', 'codespace-domain']
        edges = [('codespace-devguard', 'devguard-client', None), ('devguard-client', 'serde', None),
                 ('serde', 'codex-protocol', None), ('codespace-devguard', 'codespace-domain', None)]
        data = fixture(names, edges, {'devguard-client': deps.DEVGUARD_SOURCE, 'serde': 'registry'})
        # The adapter may use CodeSpace's own crates; DevGuard's crates may not reach Codex or CodeSpace.
        self.assertEqual(deps.graph(data, ['codespace-devguard'], devguard=True)['violations'],
                         ['devguard-client -> serde -> codex-protocol: brought in by DevGuard'])

    def test_the_worker_links_devguard_only_with_its_feature(self):
        names = ['codespace-codex-runtime', 'codespace-runner', 'codespace-devguard', 'devguard-client',
                 'codex-utils-pty']
        edges = [('codespace-codex-runtime', 'codespace-runner', None), ('codespace-runner', 'codespace-devguard', None),
                 ('codespace-devguard', 'devguard-client', None), ('codespace-runner', 'codex-utils-pty', None)]
        data = fixture(names, edges, {'devguard-client': deps.DEVGUARD_SOURCE})
        self.assertEqual(deps.graph(data, ['codespace-codex-runtime'])['violations'], [
            'codespace-codex-runtime -> codespace-runner -> codespace-devguard -> devguard-client: '
            'DevGuard outside the devguard feature',
            'codespace-codex-runtime -> codespace-runner -> codespace-devguard: DevGuard outside the devguard feature'])
        self.assertFalse(deps.graph(data, ['codespace-codex-runtime'], devguard=True)['violations'])

    def test_devguard_test_fixtures_stay_out_of_products(self):
        names = ['codespace-devguard', 'devguard-client', 'devguard-daemon']
        pinned = {'devguard-client': deps.DEVGUARD_SOURCE, 'devguard-daemon': deps.DEVGUARD_SOURCE}
        development = fixture(names, [('codespace-devguard', 'devguard-client', None),
                                      ('codespace-devguard', 'devguard-daemon', 'dev')], pinned)
        self.assertFalse(deps.graph(development, ['codespace-devguard'], devguard=True)['violations'])
        product = fixture(names, [('codespace-devguard', 'devguard-client', None),
                                  ('codespace-devguard', 'devguard-daemon', None)], pinned)
        self.assertEqual(deps.graph(product, ['codespace-devguard'], devguard=True)['violations'],
                         ['codespace-devguard -> devguard-daemon: a DevGuard crate outside its product set'])

    def test_features_forwarded(self):
        with patch.object(deps.subprocess, 'check_output', return_value=b'{}') as call:
            deps.metadata(Path('Cargo.toml'), 'target', deps.DEVGUARD_FEATURE)
            self.assertEqual(call.call_args.args[0][-2:], ['--features', 'codespace-server/devguard'])
            deps.metadata(Path('crates/codex-runtime/Cargo.toml'), 'target', deps.RUNTIME_DEVGUARD_FEATURE)
            self.assertEqual(call.call_args.args[0][-2:], ['--features', 'codespace-codex-runtime/devguard'])
            deps.metadata(Path('Cargo.toml'), 'target')
            self.assertNotIn('--features', call.call_args.args[0])

    def test_target_forwarded_and_failure(self):
        for target in ('x86_64-unknown-linux-gnu', 'aarch64-apple-darwin'):
            with patch.object(deps.subprocess, 'check_output', return_value=b'{}') as call:
                deps.metadata(Path('Cargo.toml'), target)
                self.assertIn(target, call.call_args.args[0])
                self.assertIn('--locked', call.call_args.args[0])
        with patch.object(deps.subprocess, 'check_output', side_effect=subprocess.CalledProcessError(1, 'cargo')):
            with self.assertRaises(subprocess.CalledProcessError):
                deps.metadata(Path('Cargo.toml'), 'target')
        with patch.object(deps.subprocess, 'check_output', return_value=b'bad json'):
            with self.assertRaises(ValueError):
                deps.metadata(Path('Cargo.toml'), 'target')

    def test_comparison_deterministic(self):
        data = fixture(['app', 'dep'], [('app', 'dep', None)])
        first = deps.graph(data, ['app'])
        data['packages'].reverse()
        self.assertEqual(first, deps.graph(data, ['app']))
        old = {'schema': 1, 'target': 'linux', 'graphs': {'root': first}}
        new = json.loads(json.dumps(old))
        new['graphs']['root']['packages'].append('["new","2","registry"]')
        diff = deps.difference(old, new)
        self.assertEqual(len(diff['root']['packages']['added']), 1)
        new['target'] = 'macos'
        with self.assertRaises(ValueError):
            deps.difference(old, new)


class RunnerTests(unittest.TestCase):
    def test_command_failure(self):
        with patch.object(validation.subprocess, 'run') as call:
            call.return_value.returncode = 7
            with self.assertRaises(RuntimeError):
                validation.execute([['false']], {}, io.StringIO())

    def test_helper_paths_absolute(self):
        env = validation.helper_env(Path('/tmp/build'))
        self.assertEqual(env['CODESPACE_PATCH_BIN'], '/tmp/build/debug/codespace-patch')
        env = validation.devguard_env(Path('/tmp/build'))
        self.assertEqual(env['CODESPACE_DEVGUARD_RUNTIME_BIN'], '/tmp/build/debug/codespace-codex-runtime-devguard')
        self.assertEqual(env['CODESPACE_DEVGUARD_FIXTURE_BIN'], '/tmp/build/debug/examples/fixture_authority')
        self.assertEqual(env['CODESPACE_REQUIRE_DEVGUARD_BINS'], '1')

    def test_devguard_worker_is_built_before_and_kept_beside_the_default_one(self):
        build, copy, fixture = validation.devguard_binaries()
        self.assertIn('--features', build)
        self.assertEqual(build[build.index('--features') + 1], 'devguard')
        self.assertEqual(copy[0], 'cp')
        self.assertEqual(Path(copy[1]).name, 'codespace-codex-runtime')
        self.assertEqual(Path(copy[2]).name, validation.DEVGUARD_RUNTIME)
        self.assertEqual(fixture[-2:], ['--example', 'fixture_authority'])
        # The default worker is built after the copy, so CODESPACE_RUNTIME_BIN stays the default.
        for stage in validation.DEVGUARD_STAGES:
            self.assertIn(stage, validation.stages())

    def test_report_and_platform_skip(self):
        with tempfile.TemporaryDirectory() as tmp, \
             patch.object(validation, 'fingerprint', return_value={'head': 'a', 'codex_sha': 'pin'}), \
             patch.object(validation, 'capture', return_value='host: aarch64-apple-darwin'), \
             patch.object(validation.platform, 'system', return_value='Darwin'):
            self.assertEqual(validation.run(['linux-isolation'], Path(tmp)), 0)
            report = json.loads((Path(tmp) / 'report.json').read_text())
            self.assertEqual(report['status'], 'incomplete')
            self.assertFalse(report['full_linux_qualification'])

    def test_changed_inputs_fail(self):
        with tempfile.TemporaryDirectory() as tmp, \
             patch.object(validation, 'fingerprint', side_effect=[{'head': 'a', 'codex_sha': 'pin'}, {'head': 'b', 'codex_sha': 'pin'}]), \
             patch.object(validation, 'capture', return_value='host: x86_64-unknown-linux-gnu'):
            self.assertEqual(validation.run([], Path(tmp)), 1)
            report = json.loads((Path(tmp) / 'report.json').read_text())
            self.assertIn('changed', report['error'])

    def test_failure_report(self):
        with tempfile.TemporaryDirectory() as tmp, \
             patch.object(validation, 'fingerprint', return_value={'head': 'a', 'codex_sha': 'pin'}), \
             patch.object(validation, 'capture', return_value='host: x86_64-unknown-linux-gnu'), \
             patch.object(validation, 'execute', side_effect=RuntimeError('failure')):
            self.assertEqual(validation.run(['pin'], Path(tmp)), 1)
            report = json.loads((Path(tmp) / 'report.json').read_text())
            self.assertEqual(report['stages'][0]['status'], 'failed')


if __name__ == '__main__':
    unittest.main()

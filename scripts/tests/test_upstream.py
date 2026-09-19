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


def fixture(names, edges):
    return {'packages': [{'id': n, 'name': n, 'version': '1', 'source': None,
                           'manifest_path': str(deps.ROOT / 'crates' / n / 'Cargo.toml')} for n in names],
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

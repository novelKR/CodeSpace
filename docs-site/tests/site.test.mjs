// SPDX-License-Identifier: MIT
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
const root = fileURLToPath(new URL('../../', import.meta.url));
function python(script) {
  return execFileSync(process.env.DOCS_PYTHON || 'python3', ['-B', '-c', script], { cwd: root, encoding: 'utf8' });
}
test('compatibility anchors count as targets, but fenced examples do not', () => {
  assert.equal(python(`
import importlib.util
s=importlib.util.spec_from_file_location('docs', 'scripts/check_docs.py')
m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
text='# New heading\\n<a id="old-heading"></a>\\n<a id="이전"></a>\\n~~~html\\n<a id="example"></a>\\n# Example\\n~~~\\n'
assert m.headings(text)==['new-heading','old-heading','이전']
print('ok')`).trim(), 'ok');
});
test('home cards have distinct purposes and locale-specific guide links', () => {
  assert.equal(python(`
import importlib.util
s=importlib.util.spec_from_file_location('site', 'docs-site/scripts/site.py')
m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
_, pages=m.pages(m.ROOT)
for locale in ['en','ko']:
 cards=m.home_features(pages,locale)
 prefix='/ko' if locale=='ko' else ''
 assert [c['link'] for c in cards]==[prefix+'/guide/'+p for p in ['getting-started','agent-integration','operations']]
 assert len({c['details'] for c in cards})==3
print('ok')`).trim(), 'ok');
});
test('Korean body links stay Korean and explicit anchors survive generation', () => {
  assert.equal(python(`
import importlib.util
s=importlib.util.spec_from_file_location('site', 'docs-site/scripts/site.py')
m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
_, pages=m.pages(m.ROOT)
routes={p['source']:p['route'] for p in pages}
text='<a id="이전-제목"></a>\\n[설치](operations.md)\\n'
actual=m.remap(text,'docs/ko/agent-integration.md',routes,m.ROOT,'a'*40)
assert actual=='<a id="이전-제목"></a>\\n[설치](/ko/guide/operations)\\n'
print('ok')`).trim(), 'ok');
});

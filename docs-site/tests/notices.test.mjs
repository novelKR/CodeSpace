// SPDX-License-Identifier: MIT
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { checkedNotice, packageFor, sha256 } from '../scripts/notices.mjs';

test('notices preserve exact bytes and reject changed license evidence', () => {
  const dir = mkdtempSync(join(tmpdir(), 'docs-notices-'));
  try {
    const path = join(dir, 'LICENSE');
    const bytes = Buffer.from('Synthetic notice\n');
    writeFileSync(path, bytes);
    assert.deepEqual(checkedNotice(path, sha256(bytes)), bytes);
    assert.throws(() => checkedNotice(path, '0'.repeat(64)), /Unreviewed/);
  } finally { rmSync(dir, { recursive: true }); }
});
test('client package identity is limited to the installation directory', () => {
  const dir = mkdtempSync(join(tmpdir(), 'docs-modules-'));
  const modules = join(dir, 'node_modules');
  try {
    mkdirSync(join(modules, 'example', 'dist'), { recursive: true });
    writeFileSync(join(modules, 'example', 'package.json'), JSON.stringify({ name: 'example', version: '1.0.0' }));
    assert.equal(packageFor(join(modules, 'example', 'dist', 'index.js') + '?vue', modules).info.name, 'example');
    assert.equal(packageFor(join(dir, 'source.ts'), modules), null);
    assert.equal(packageFor('\0virtual-module', modules), null);
  } finally { rmSync(dir, { recursive: true }); }
});

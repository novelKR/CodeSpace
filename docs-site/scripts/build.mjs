// SPDX-License-Identifier: MIT
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { build } from 'vitepress';

const site = fileURLToPath(new URL('../', import.meta.url));
if (process.versions.node !== '24.21.0') throw new Error('Use the pinned Node 24.21.0 toolchain');
const python = process.env.DOCS_PYTHON || 'python3';
function check(command) {
  const result = spawnSync(python, ['-B', site + 'scripts/site.py', command], { stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error('Docs ' + command + ' failed');
}
check('prepare');
await build(site);
check('record');
check('check');

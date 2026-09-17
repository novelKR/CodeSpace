// SPDX-License-Identifier: MIT
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';

export const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
export function checkedNotice(path, expected) {
  const bytes = readFileSync(path);
  if (sha256(bytes) !== expected) throw new Error('Unreviewed web notice: ' + path.split(sep).pop());
  return bytes;
}
export function packageFor(id, modules) {
  if (id.startsWith('\0')) return null;
  const clean = id.split('?')[0];
  if (!clean.startsWith(modules + sep)) return null;
  let directory = dirname(clean);
  while (directory.startsWith(modules + sep)) {
    const path = join(directory, 'package.json');
    if (existsSync(path)) {
      const info = JSON.parse(readFileSync(path, 'utf8'));
      if (info.name && info.version) return { directory, info };
    }
    directory = dirname(directory);
  }
  throw new Error('Cannot identify client dependency');
}
export function notices(root) {
  const site = join(root, 'docs-site');
  const modules = resolve(site, 'node_modules');
  const policy = JSON.parse(readFileSync(join(site, 'licensing/policy.json'), 'utf8'));
  const lock = JSON.parse(readFileSync(join(site, 'package-lock.json'), 'utf8'));
  let server = false;
  return {
    name: 'docs-actions-web-notices',
    apply: 'build',
    configResolved(config) { server = Boolean(config.build.ssr); },
    generateBundle(_options, bundle) {
      if (server) return;
      const found = new Map();
      for (const item of Object.values(bundle)) {
        if (item.type !== 'chunk') continue;
        for (const id of Object.keys(item.modules)) {
          const pkg = packageFor(id, modules);
          if (pkg) found.set(pkg.directory, pkg);
        }
      }
      // CSS-only assets may not appear in JS chunk.modules. Keep their notices too.
      for (const name of ['vitepress', '@docsearch/css']) {
        const directory = join(modules, name);
        found.set(directory, { directory, info: JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8')) });
      }
      const records = [], texts = [];
      for (const { directory, info } of [...found.values()].sort((a, b) => a.info.name.localeCompare(b.info.name, 'en'))) {
        const rule = policy.packages[info.name];
        if (!rule || info.license !== rule.license) throw new Error('Unreviewed client package: ' + info.name + '@' + info.version);
        const key = relative(site, directory).split(sep).join('/');
        const pinned = lock.packages[key];
        if (!pinned || pinned.version !== info.version || !pinned.integrity || !pinned.resolved?.startsWith('https://registry.npmjs.org/'))
          throw new Error('Client package differs from npm lock: ' + info.name);
        const noticePath = rule.supplement ? join(site, 'licensing/originals', rule.file) : join(directory, rule.file);
        const bytes = checkedNotice(noticePath, rule.sha256);
        texts.push(Buffer.from('\n--- ' + info.name + '@' + info.version + ' ---\n'), bytes);
        records.push({ name: info.name, version: info.version, license: info.license,
          resolved: pinned.resolved, integrity: pinned.integrity,
          notice_sha256: rule.sha256, notice_source: rule.source || pinned.resolved });
      }
      const icon = policy.icons;
      checkedNotice(join(modules, icon.asset), icon.sha256);
      for (const item of icon.notices) {
        texts.push(Buffer.from('\n--- ' + item.name + ' ---\n'), checkedNotice(join(site, 'licensing/originals', item.file), item.sha256));
      }
      records.push({ name: 'VitePress embedded icons', license: 'ISC AND MIT', asset_sha256: icon.sha256,
        source: icon.source, notices: icon.notices });
      this.emitFile({ type: 'asset', fileName: 'web-notices.txt', source: Buffer.concat(texts) });
      this.emitFile({ type: 'asset', fileName: 'web-dependencies.json', source: JSON.stringify(records, null, 2) + '\n' });
    },
  };
}

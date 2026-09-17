#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Allowlisted Markdown -> site inputs; exact output verification and static preview."""
from __future__ import annotations

import argparse
import hashlib
from html.parser import HTMLParser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
from urllib.parse import quote, unquote, urlsplit

ROOT = Path(__file__).resolve().parents[2]
BASE = '/CodeSpace/'
REPOSITORY = 'https://github.com/novelKR/CodeSpace'
SCHEMA = 'codespace-docs-site/v1'
REGISTRY = 'docs/translations.json'
REGISTRY_SCHEMA = 'codespace-documentation/v1'
BLOCKED = {'.git', '.private', '.local', '.env', 'node_modules', '__pycache__'}
DOCUMENT_KEYS = {
    'id', 'section', 'order', 'route', 'source', 'translation',
    'anchors', 'source_sha256', 'translation_sha256',
}
INLINE_CODE = re.compile(r'`+[^`\n]+`+')
MARKDOWN_LINK = re.compile(r'\[([^]\n]+)\]\(([^)\s]+)\)')


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def read_json(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, 'Duplicate JSON key')
            result[key] = value
        return result
    return json.loads(path.read_text(encoding='utf-8'), object_pairs_hook=unique)


def safe_file(root, relative):
    require(isinstance(relative, str) and bool(relative), 'Missing source path')
    parts = PurePosixPath(relative).parts
    require(not relative.startswith('/') and '\\' not in relative and '..' not in parts,
            'Unsafe source path')
    require(not set(parts) & BLOCKED, 'Reserved source path')
    file = root.joinpath(*parts)
    for parent in (file, *file.parents):
        if parent == root:
            break
        require(not parent.is_symlink(), 'Source links are forbidden')
    require(file.resolve().is_relative_to(root.resolve()) and file.is_file(),
            'Missing regular source file: ' + relative)
    require(file.stat().st_size <= 1024 * 1024, 'Source is too large')
    return file


def site_route(prefix, route):
    require(isinstance(route, str) and route.startswith('/') and '//' not in route, 'Invalid document route')
    if route == '/':
        return prefix + '/' if prefix else '/'
    require(route.startswith('/guide/'), 'Guide routes must start with /guide/')
    return prefix + route


def pages(root):
    registry = read_json(safe_file(root, REGISTRY))
    require(set(registry) == {'schema', 'review_method', 'groups', 'documents'}, 'Invalid document registry')
    require(registry['schema'] == REGISTRY_SCHEMA, 'Unknown documentation schema')
    require(isinstance(registry['review_method'], str) and registry['review_method'], 'Missing review method')
    require(isinstance(registry['groups'], list) and 0 < len(registry['groups']) <= 20, 'Invalid group count')
    groups, group_ids = [], set()
    for group in registry['groups']:
        require(set(group) == {'id', 'en', 'ko'}, 'Invalid group entry')
        ident = group['id']
        require(isinstance(ident, str) and re.fullmatch(r'[a-z][a-z0-9-]*', ident) and ident not in group_ids,
                'Duplicate or invalid group ID')
        require(isinstance(group['en'], str) and group['en'] and isinstance(group['ko'], str) and group['ko'],
                'Missing group labels')
        group_ids.add(ident)
        groups.append(group)
    require(isinstance(registry['documents'], list) and 0 < len(registry['documents']) <= 100, 'Invalid page count')
    result, ids, paths, routes = [], set(), set(), set()
    for entry in registry['documents']:
        require(set(entry) == DOCUMENT_KEYS, 'Invalid registry entry')
        ident = entry['id']
        require(isinstance(ident, str) and re.fullmatch(r'[a-z][a-z0-9-]*', ident) and ident not in ids,
                'Duplicate or invalid page ID')
        ids.add(ident)
        require(entry['section'] in group_ids, 'Unknown document section')
        require(isinstance(entry['order'], int) and entry['order'] >= 0, 'Invalid document order')
        require(isinstance(entry['anchors'], list) and all(isinstance(item, str) and item for item in entry['anchors']),
                'Invalid document anchors')
        route = entry['route']
        require(route not in routes, 'Duplicate document route')
        routes.add(route)
        for locale, field, hash_field in (
            ('en', 'source', 'source_sha256'),
            ('ko', 'translation', 'translation_sha256'),
        ):
            source = entry[field]
            require(isinstance(source, str) and source.endswith('.md') and source not in paths,
                    'Invalid or duplicate Markdown source')
            require(source in {'README.md', 'README.ko.md'} or source.startswith('docs/'),
                    'Not a maintained document')
            paths.add(source)
            raw = safe_file(root, source).read_bytes()
            require(re.fullmatch(r'[0-9a-f]{64}', entry[hash_field] or '') and digest(raw) == entry[hash_field],
                    'Stale or missing hash for ' + source)
            text = raw.decode('utf-8')
            heading = re.search(r'^# (.+)$', text, re.M)
            require(heading is not None, 'Missing document title')
            prefix = '/ko' if locale == 'ko' else ''
            result.append({
                'id': ident, 'locale': locale, 'source': source, 'section': entry['section'],
                'order': entry['order'], 'title': heading[1], 'route': site_route(prefix, route),
                'copy': 'sources/' + locale + '/' + ident + '.md', 'sha256': digest(raw),
            })
    return groups, result


def remap(text, source, routes, root, commit):
    """Rewrite prose links, not code examples. Keep the original file for copying."""
    def link(match):
        label, target = match.groups()
        parsed = urlsplit(target)
        if parsed.scheme or parsed.netloc:
            require(parsed.scheme in {'https', 'http', 'mailto'} and not target.startswith('//'),
                    'Unsupported link scheme')
            return match[0]
        if not parsed.path:
            return match[0]
        path = (root / source).parent / unquote(parsed.path)
        normalized = Path(__import__('os').path.normpath(path))
        require(normalized.is_relative_to(root) and not set(normalized.relative_to(root).parts) & BLOCKED,
                'Link escapes public sources')
        relative = normalized.relative_to(root).as_posix()
        if relative in routes or root.joinpath(*PurePosixPath(relative).parts).is_file():
            safe_file(root, relative)
        suffix = ('?' + parsed.query if parsed.query else '') + ('#' + parsed.fragment if parsed.fragment else '')
        url = routes[relative] if relative in routes else REPOSITORY + '/blob/' + commit + '/' + quote(relative, safe='/')
        return '[' + label + '](' + url + suffix + ')'

    def validate(value):
        require('<<<' not in value and '{{' not in value, 'Includes and Vue expressions are not allowed in documents')
        sanitized = re.sub(r'<a id="[\w-]+"></a>', '', value)
        require(not re.search(r'<\s*/?\s*[A-Za-z!]', sanitized), 'Raw HTML or components are not allowed')
        require('![' not in value, 'Unreviewed page resources are not allowed')
        require(not re.search(r'^\s*\[[^]]+\]:', value), 'Use inline Markdown links, not reference definitions')
        return value

    def prose(value):
        pieces = re.split(r'(`+[^`\n]+`+)', value)
        checked = ''.join(part if part.startswith('`') else validate(part) for part in pieces)
        spans = [(match.start(), match.end()) for match in INLINE_CODE.finditer(checked)]

        def replace(match):
            if any(start <= match.start() and match.end() <= end for start, end in spans):
                return match[0]
            return link(match)

        return MARKDOWN_LINK.sub(replace, checked)

    lines, fence = [], None
    for line in text.splitlines(keepends=True):
        marker = re.match(r'^\s*(`{3,}|~{3,})(.*)$', line)
        if marker and fence is None:
            fence = marker[1]
            lines.append(line)
            continue
        if fence:
            lines.append(line)
            if marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence) and not marker[2].strip():
                fence = None
            continue
        if re.fullmatch(r'\[English\]\([^\n]+\) \| \[한국어\]\([^\n]+\)\s*', line):
            continue
        lines.append(prose(line))
    require(fence is None, 'Unclosed fenced code block')
    return ''.join(lines)


def git(root, *args):
    return subprocess.check_output(['git', *args], cwd=root, text=True).strip()


def output_markdown(route):
    if route == '/':
        return 'index.md'
    if route == '/ko/':
        return 'ko/index.md'
    return route.lstrip('/') + '.md'


def prepare(root=ROOT):
    root = root.resolve()
    commit = git(root, 'rev-parse', 'HEAD')
    require(re.fullmatch('[0-9a-f]{40}', commit), 'Expected committed source')
    dirty = bool(git(root, 'status', '--porcelain', '--untracked-files=normal'))
    groups, inventory = pages(root)
    work = root / '.local/docs-site'
    require(not (root / '.local').is_symlink() and not work.is_symlink(), 'Build directories cannot be symlinks')
    source = work / 'source'
    require(not source.is_symlink(), 'Build source cannot be a symlink')
    if source.exists():
        shutil.rmtree(source)
    source.mkdir(parents=True)
    routes = {page['source']: page['route'] for page in inventory}
    for page in inventory:
        raw = safe_file(root, page['source']).read_bytes()
        body = remap(raw.decode('utf-8'), page['source'], routes, root, commit)
        dest = source / output_markdown(page['route'])
        dest.parent.mkdir(parents=True, exist_ok=True)
        front = {'title': page['title'], 'docLocale': page['locale'],
                 'copyPath': '/' + page['copy'], 'sourceCommit': commit}
        dest.write_text('---\n' + '\n'.join(k + ': ' + json.dumps(v, ensure_ascii=False)
                                            for k, v in front.items()) + '\n---\n\n' + body, encoding='utf-8')
        copy = source / 'public' / page['copy']
        copy.parent.mkdir(parents=True, exist_ok=True)
        copy.write_bytes(raw)
    group_rank = {group['id']: index for index, group in enumerate(groups)}
    en_guides = [page for page in inventory if page['locale'] == 'en']
    en_guides.sort(key=lambda page: (group_rank[page['section']], page['order']))
    feature_sources = en_guides[:3]
    for locale, tagline, start in (
        ('en', 'The client decides. This process reads, patches, and runs.', 'Start here'),
        ('ko', '클라이언트가 판단합니다. 이 프로세스는 읽고, 패치하고, 실행합니다.', '시작하기'),
    ):
        prefix = '/ko' if locale == 'ko' else ''
        features = []
        for page in feature_sources:
            mate = next(item for item in inventory if item['id'] == page['id'] and item['locale'] == locale)
            features.append({'title': mate['title'], 'details': tagline, 'link': mate['route']})
        data = {
            'layout': 'home', 'docLocale': locale,
            'hero': {
                'name': 'CodeSpace',
                'text': 'Execution-tools MCP' if locale == 'en' else '실행 도구 MCP',
                'tagline': tagline,
                'actions': [
                    {'theme': 'brand', 'text': start, 'link': prefix + '/guide/getting-started'},
                    {'theme': 'alt', 'text': 'GitHub', 'link': REPOSITORY},
                ],
            },
            'features': features,
        }
        home = source / output_markdown(prefix + '/' if prefix else '/')
        home.parent.mkdir(parents=True, exist_ok=True)
        home.write_text('---\n' + json.dumps(data, ensure_ascii=False, indent=2) + '\n---\n', encoding='utf-8')
    (source / 'public').mkdir(exist_ok=True)
    shutil.copyfile(root / 'LICENSE', source / 'public/LICENSE.txt')
    record = {'schema': SCHEMA, 'source_commit': commit, 'working_tree': dirty,
              'groups': groups, 'pages': inventory}
    (work / 'catalogue.json').write_text(json.dumps(record, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return record


class HTML(HTMLParser):
    def __init__(self, raw):
        super().__init__(convert_charrefs=True)
        self.ids, self.links, self.resources = set(), [], []
        self.feed(raw.decode('utf-8'))

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if 'id' in attrs:
            self.ids.add(attrs['id'])
        if tag == 'a' and 'href' in attrs:
            self.links.append(attrs['href'])
        if tag in {'script', 'img', 'source', 'iframe', 'video', 'audio'} and 'src' in attrs:
            self.resources.append(attrs['src'])
        if tag == 'link' and set(attrs.get('rel', '').split()) & {'stylesheet', 'modulepreload', 'preload', 'icon'}:
            self.resources.append(attrs.get('href', ''))


def html_name(route):
    if route in {'/', '/ko/'}:
        return 'index.html' if route == '/' else 'ko/index.html'
    if route.endswith('/'):
        return route.lstrip('/') + 'index.html'
    return route.lstrip('/') + '.html'


def inventory(directory, catalog, root=ROOT):
    require(directory.is_dir() and not directory.is_symlink(), 'Missing regular site directory')
    expected = {'index.html', 'ko/index.html', '404.html'} | {html_name(page['route']) for page in catalog['pages']}
    required = expected | {page['copy'] for page in catalog['pages']} | {
        'LICENSE.txt', 'web-notices.txt', 'web-dependencies.json',
    }
    allowed = required | {'hashmap.json', 'vp-icons.css', 'build-manifest.json'}
    files, html = {}, {}
    for path in sorted(directory.rglob('*')):
        require(not path.is_symlink(), 'Symlink in published output')
        if path.is_dir():
            continue
        require(path.is_file() and path.stat().st_nlink == 1, 'Only regular non-linked files may be published')
        name = path.relative_to(directory).as_posix()
        require(name in allowed or re.fullmatch(r'assets/(?:chunks/)?[@A-Za-z0-9_.-]+\.(?:js|css)', name),
                'Unexpected publication file: ' + name)
        raw = path.read_bytes()
        require(len(raw) <= 16 * 1024 * 1024, 'Published file too large')
        require(str(root.resolve()).encode() not in raw, 'Local build path leaked')
        if name != 'build-manifest.json':
            files[name] = digest(raw)
        if name.endswith('.html'):
            html[name] = HTML(raw)
    require(set(html) == expected and required <= set(files), 'Missing or unexpected pages/notices')
    for page in catalog['pages']:
        require(files[page['copy']] == page['sha256'], 'Copied Markdown differs from selected source')
    for name, page in html.items():
        for resource in page.resources:
            require(resource.startswith(BASE) and not urlsplit(resource).netloc, 'Nonlocal page resource')
            require(unquote(urlsplit(resource).path[len(BASE):]) in files, 'Missing local resource')
        for link in page.links:
            url = urlsplit(link)
            if url.scheme or url.netloc:
                require(url.scheme in {'https', 'http', 'mailto'}, 'Invalid link scheme')
                continue
            target = name
            if url.path:
                require(url.path.startswith(BASE), 'Link escapes the site base')
                target = unquote(url.path[len(BASE):])
                if not target or target.endswith('/'):
                    target += 'index.html'
                elif target not in files and not Path(target).suffix:
                    target += '.html'
            require(target in files, 'Broken local link: ' + target)
            if url.fragment and target in html:
                require(unquote(url.fragment) in html[target].ids, 'Broken local anchor: ' + target)
    return dict(sorted(files.items()))


def verify(root=ROOT, record=False, commit=None):
    catalog = read_json(root / '.local/docs-site/catalogue.json')
    directory = root / '.local/docs-site/dist'
    require(catalog['schema'] == SCHEMA, 'Unknown catalogue schema')
    files = inventory(directory, catalog, root)
    manifest = {key: catalog[key] for key in ('schema', 'source_commit', 'working_tree')}
    manifest['sources'] = {page['source']: page['sha256'] for page in catalog['pages']}
    manifest['files'] = files
    for source, sha in manifest['sources'].items():
        require(digest(safe_file(root, source).read_bytes()) == sha, 'Sources changed after preparation')
    if commit is not None:
        require(not record, 'Publication verification cannot rewrite its evidence')
        require(re.fullmatch(r'[0-9a-f]{40}', commit) and manifest['source_commit'] == commit,
                'Publication commit mismatch')
        require(git(root, 'rev-parse', 'HEAD') == commit, 'Checkout does not match publishing commit')
        require(manifest['working_tree'] is False and not git(root, 'status', '--porcelain', '--untracked-files=normal'),
                'Publication source is dirty')
    path = directory / 'build-manifest.json'
    if record:
        path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    else:
        require(read_json(path) == manifest, 'Build manifest differs from verified output')
    print(f'Site verified: {len(files)} hashed files, {sum(n.endswith(".html") for n in files)} HTML pages')
    return manifest


def handler(directory, manifest):
    class Static(BaseHTTPRequestHandler):
        def do_GET(self):
            url = urlsplit(self.path)
            path = unquote(url.path)
            if not path.startswith(BASE) or '\\' in path or '..' in PurePosixPath(path).parts:
                self.send_error(404)
                return
            relative = path[len(BASE):]
            relative = relative + 'index.html' if not relative or relative.endswith('/') else relative
            if relative not in manifest['files'] and not Path(relative).suffix:
                relative += '.html'
            if relative not in manifest['files']:
                self.send_error(404)
                return
            file = directory / relative
            try:
                require(not any(p.is_symlink() for p in (file, *file.parents)), 'Linked preview file')
                raw = file.read_bytes()
                require(digest(raw) == manifest['files'][relative], 'Modified preview file')
            except (OSError, ValueError):
                self.send_error(409)
                return
            mime = {'.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css',
                    '.json': 'application/json'}.get(file.suffix, 'text/plain')
            self.send_response(200)
            self.send_header('Content-Type', mime + '; charset=utf-8')
            self.send_header('Content-Length', str(len(raw)))
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            self.wfile.write(raw)

        def log_message(self, *_args):
            pass
    return Static


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['prepare', 'check', 'record', 'preview'])
    parser.add_argument('--commit')
    args = parser.parse_args()
    try:
        if args.command == 'prepare':
            prepare()
        elif args.command == 'preview':
            manifest = verify()
            print('Verified preview: http://127.0.0.1:43141' + BASE, flush=True)
            ThreadingHTTPServer(('127.0.0.1', 43141),
                                handler(ROOT / '.local/docs-site/dist', manifest)).serve_forever()
        else:
            verify(record=args.command == 'record', commit=args.commit)
    except (ValueError, OSError, KeyError, TypeError, subprocess.CalledProcessError, StopIteration) as error:
        print('Site check failed: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

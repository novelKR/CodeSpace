#!/usr/bin/env python3
"""Validate and record bilingual documentation registry hashes."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = ROOT / 'docs' / 'translations.json'
SCHEMA = 'codespace-documentation/v1'
KEYS = {
    'id', 'section', 'order', 'route', 'source', 'translation',
    'anchors', 'source_sha256', 'translation_sha256',
}


def fail(message):
    print('Documentation check failed: ' + message, file=sys.stderr)
    return 1


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def slug(heading):
    text = re.sub(r'`+', '', heading).strip().lower()
    text = re.sub(r'[^\w\s가-힣-]', '', text, flags=re.UNICODE)
    return re.sub(r'\s+', '-', text).strip('-')


def headings(text):
    found, fence = [], None
    for line in text.splitlines():
        marker = re.match(r'^\s*(`{3,}|~{3,})(.*)$', line)
        if marker and fence is None:
            fence = marker[1]
            continue
        if fence:
            if marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence) and not marker[2].strip():
                fence = None
            continue
        match = re.match(r'^#{1,6} (.+)$', line)
        if match:
            found.append(slug(match[1]))
    return found


def load():
    registry = json.loads(REGISTRY.read_text(encoding='utf-8'))
    if registry.get('schema') != SCHEMA:
        raise ValueError('Unknown documentation schema')
    return registry


def check():
    registry = load()
    ids = set()
    for document in registry['documents']:
        missing = KEYS - set(document)
        if missing:
            return fail('Missing fields on ' + document.get('id', '?') + ': ' + ', '.join(sorted(missing)))
        ident = document['id']
        if ident in ids:
            return fail('Duplicate document id ' + ident)
        ids.add(ident)
        source = ROOT / document['source']
        translation = ROOT / document['translation']
        if not source.is_file() or not translation.is_file():
            return fail('Missing files for ' + ident)
        if digest(source) != document['source_sha256']:
            return fail('Stale source hash for ' + ident + ' (' + document['source'] + ')')
        if digest(translation) != document['translation_sha256']:
            return fail('Stale translation hash for ' + ident + ' (' + document['translation'] + ')')
        source_text = source.read_text(encoding='utf-8')
        translation_text = translation.read_text(encoding='utf-8')
        if not re.search(r'^\[English\]\([^)]+\) \| \[한국어\]\([^)]+\)\s*$', source_text, re.M):
            return fail('Missing language links in ' + document['source'])
        if not re.search(r'^\[English\]\([^)]+\) \| \[한국어\]\([^)]+\)\s*$', translation_text, re.M):
            return fail('Missing language links in ' + document['translation'])
        available = set(headings(source_text)) | set(headings(translation_text))
        missing_anchors = [item for item in document['anchors'] if item not in available]
        if missing_anchors:
            return fail('Missing anchors for ' + ident + ': ' + ', '.join(missing_anchors))
    print('Documentation registry verified: ' + str(len(registry['documents'])) + ' paired documents')
    return 0


def record(ident=None):
    registry = load()
    selected = [document for document in registry['documents'] if ident is None or document['id'] == ident]
    if ident and not selected:
        return fail('Unknown document id ' + ident)
    for document in selected:
        source = ROOT / document['source']
        translation = ROOT / document['translation']
        document['source_sha256'] = digest(source)
        document['translation_sha256'] = digest(translation)
        document['anchors'] = sorted(set(headings(source.read_text(encoding='utf-8')))
                                     | set(headings(translation.read_text(encoding='utf-8'))))
    REGISTRY.write_text(json.dumps(registry, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print('Recorded ' + ', '.join(document['id'] for document in selected))
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', nargs='?', default='check', choices=['check', 'record'])
    parser.add_argument('--id')
    args = parser.parse_args()
    try:
        if args.command == 'record':
            return record(args.id)
        if args.id:
            return fail('check does not take --id')
        return check()
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        return fail(str(error))


if __name__ == '__main__':
    raise SystemExit(main())

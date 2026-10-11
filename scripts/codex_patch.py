#!/usr/bin/env python3
"""Apply, check or revert the recorded Codex patches on the pinned checkout.

The submodule stays at the official commit that docs/upstream-lock.md pins. The patches that
the record lists are applied to its files in order, and the result must be the patched tree the
record names. `apply` brings a clean checkout of the pinned commit to that tree, `check` only
verifies it and `revert` returns the files to the pinned commit before a pin change. Any other
state of the checkout is an error, so neither a missing patch nor a stray change passes as the
recorded source.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
LOCK = ROOT / 'docs' / 'upstream-lock.md'
CHECKOUT = ROOT / 'third_party' / 'codex'
PIN_SECTION = 'Deployment pin'
PATCH_SECTION = 'Downstream patch'


class PatchStateError(RuntimeError):
    """The record, a patch file or the checkout is not in a state this script accepts."""


@dataclass(frozen=True)
class Record:
    commit: str
    # Each patch file and its SHA-256, in the order they apply.
    patches: tuple[tuple[Path, str], ...]
    # The tree the patches produce from the pinned commit; None when no patch is recorded.
    tree: str | None


def read_record(lock: Path = LOCK, root: Path = ROOT) -> Record:
    """The pinned commit, the patches and the patched tree, as the pin record states them."""
    section, commit, tree, patches = None, None, None, []
    for line in lock.read_text(encoding='utf-8').splitlines():
        if line.startswith('## '):
            section = line[3:].strip()
            continue
        cells = [cell.strip() for cell in line.split('|')]
        if len(cells) < 4 or cells[0]:
            continue
        field, value = cells[1], cells[2]
        if section == PIN_SECTION and field == 'Commit' and commit is None:
            commit = _one(r'`([0-9a-f]{40})`', value, 'pinned commit')
        elif section == PATCH_SECTION and field == 'Patch':
            path = _one(r'`(third_party/codex-patches/[^`]+\.patch)`', value, 'patch file')
            patches.append((root / path, _one(r'SHA-256 `([0-9a-f]{64})`', value, 'patch SHA-256')))
        elif section == PATCH_SECTION and field == 'Patched tree':
            tree = _one(r'`([0-9a-f]{40})`', value, 'patched tree')
    if commit is None:
        raise PatchStateError(f'no pinned commit in the {PIN_SECTION} table of {lock}')
    if bool(patches) != (tree is not None):
        raise PatchStateError(f'{lock} must record a patched tree exactly when it records a patch')
    return Record(commit, tuple(patches), tree)


def _one(pattern, value, what):
    match = re.search(pattern, value)
    if not match:
        raise PatchStateError(f'cannot read the {what} from: {value}')
    return match[1]


def git(checkout: Path, *args: str, env=None) -> str:
    result = subprocess.run(['git', '-C', str(checkout), *args], env=env, capture_output=True, text=True)
    if result.returncode:
        raise PatchStateError(f'git {" ".join(args)} failed in {checkout}: {result.stderr.strip()}')
    return result.stdout.strip()


def working_tree(checkout: Path = CHECKOUT) -> str:
    """The tree of the checkout's files as they are, untracked files included and ignored ones
    left out. It is built in a scratch index, so the checkout's own index is not touched."""
    with tempfile.TemporaryDirectory() as scratch:
        env = dict(os.environ, GIT_INDEX_FILE=str(Path(scratch) / 'index'))
        git(checkout, 'read-tree', 'HEAD', env=env)
        git(checkout, 'add', '--all', env=env)
        return git(checkout, 'write-tree', env=env)


def verify_patches(record: Record):
    for path, expected in record.patches:
        try:
            actual = hashlib.sha256(path.read_bytes()).hexdigest()
        except OSError as error:
            raise PatchStateError(f'cannot read the recorded patch {path}: {error}') from error
        if actual != expected:
            raise PatchStateError(f'{path} has SHA-256 {actual}, not the recorded {expected}')


def state(record: Record, checkout: Path) -> str:
    """'patched' when the checkout holds exactly the recorded source, 'pinned' when it holds the
    pinned commit without the patches. Raises for any other state."""
    head = git(checkout, 'rev-parse', 'HEAD')
    if head != record.commit:
        raise PatchStateError(f'{checkout} is at {head}, not the pinned {record.commit}')
    current = working_tree(checkout)
    pinned = git(checkout, 'rev-parse', 'HEAD^{tree}')
    if current == (record.tree or pinned):
        return 'patched'
    if current == pinned:
        return 'pinned'
    raise PatchStateError(
        f'{checkout} has tree {current}, which is neither the pinned commit ({pinned}) nor the '
        f'recorded patched tree ({record.tree}); it has changes the record does not hold')


def apply(record: Record, checkout: Path = CHECKOUT) -> str:
    verify_patches(record)
    if state(record, checkout) == 'patched':
        return f'Codex checkout already holds the recorded source: tree {record.tree or "of the pinned commit"}'
    for path, _ in record.patches:
        git(checkout, 'apply', '--whitespace=nowarn', str(path.resolve()))
    if state(record, checkout) != 'patched':
        raise PatchStateError(f'the patches did not produce the recorded tree {record.tree}')
    return f'Applied {len(record.patches)} recorded patch(es) to {record.commit}: tree {record.tree}'


def check(record: Record, checkout: Path = CHECKOUT) -> str:
    verify_patches(record)
    if state(record, checkout) != 'patched':
        raise PatchStateError(
            f'{checkout} lacks the recorded patches; run python3 scripts/codex_patch.py apply')
    return f'Codex checkout ok: {record.commit} with {len(record.patches)} recorded patch(es)'


def revert(record: Record, checkout: Path = CHECKOUT) -> str:
    verify_patches(record)
    if state(record, checkout) == 'pinned' or not record.patches:
        return f'Codex checkout holds the pinned commit {record.commit} unpatched'
    for path, _ in reversed(record.patches):
        git(checkout, 'apply', '--reverse', '--whitespace=nowarn', str(path.resolve()))
    if state(record, checkout) != 'pinned':
        raise PatchStateError(f'reverting the patches did not restore the pinned commit in {checkout}')
    return f'Reverted the recorded patches: {checkout} holds {record.commit}'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['apply', 'check', 'revert'])
    args = parser.parse_args()
    try:
        print({'apply': apply, 'check': check, 'revert': revert}[args.command](read_record()))
    except (OSError, PatchStateError) as error:
        print(f'codex_patch {args.command} failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())

# Public documentation and the documentation site

[English](documentation.md) | [한국어](ko/documentation.md)

English files are the editorial source. Root guides use matching
`.ko.md` files. Guides under `docs/` use `docs/ko/`. Keep reciprocal
language links. Commands, code blocks, identifiers, and support
conditions must stay the same across each pair.

The [document registry](translations.json) records stable IDs,
navigation groups, preserved anchors, source/translation paths, and
both reviewed file hashes. The hashes detect drift. They do not prove
semantic equivalence or human approval. Review both complete documents
before recording the pair.

```sh
python3 -B scripts/check_docs.py
python3 -B scripts/check_docs.py record --id documentation
```

`record` is for an editor after reviewing that document pair. Select
each reviewed ID explicitly. A new maintained Markdown document needs
its Korean edition and a registry entry. CI never updates review
records automatically.

## Documentation site

The site uses VitePress 1.6.4, Node 24.21.0, and npm 11.19.0 with the
committed [npm lock](../docs-site/package-lock.json). The site
implementation is MIT, copied from docs-actions and adapted here. See
[site provenance](../docs-site/PROVENANCE.json). Project documentation
stays Apache-2.0.

Do not start the Vite development server. From the repository root:

```sh
npm ci --prefix docs-site --ignore-scripts
npm test --prefix docs-site
npm run build --prefix docs-site
python3 -B docs-site/scripts/site.py check
python3 -B docs-site/scripts/site.py preview
```

The preview listens at `http://127.0.0.1:43141/CodeSpace/`. Rebuild
after edits. The preview has no hot module replacement. English is at
the site root and Korean is under `/ko/`. Maintained pages live under
`/guide/` and `/ko/guide/`. Copy Page copies the current language's
maintained Markdown. Local search stays in the browser.

Set `DOCS_PYTHON` when the Python executable has another name. Use
Python 3.11 or later. CI selects Python 3.14.

## Publication

Pull requests run a read-only docs build and keep a review artifact.
They do not deploy. A `main` push or a manual `main` run packages the
same verified directory and calls the pinned docs-actions reusable
workflow. Grant only that deployment job `pages: write` and
`id-token: write`. Do not use `secrets: inherit`.

The pin is [`.github/docs-pages-deploy.lock.json`](../.github/docs-pages-deploy.lock.json).
Adopt a new SHA only after reviewing that commit, its license scope,
and successful central CI.

GitHub Pages source, the `github-pages` environment, and allowing the
public reusable workflow are repository settings. They are not applied
by this documentation build. A successful local build or retained
artifact is not a live site.

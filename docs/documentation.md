<a id="public-documentation-and-the-documentation-site"></a>

# Maintaining the documentation

[English](documentation.md) | [한국어](ko/documentation.md)

The published site and repository guides share the same maintained Markdown. English is the editorial source; each maintained guide has a reviewed Korean counterpart. Improve unclear English first, then write natural Korean with the same behavior, limits, and examples.

## Editorial review

Check each page against current code, configuration, and tests. Separate implemented behavior, configured availability, test coverage, and actual deployment evidence. Describe what the reader can do before introducing internal implementation names. Explain necessary technical terms on first use.

Avoid unexplained work-package numbers, past-session claims, repeated negative comparisons, and unrelated repository names. Keep dependency attribution and useful source links. Translate table explanations and navigation labels; preserve API identifiers. Korean prose should read naturally on its own, rather than mirror English word order.

Give each fact a primary reference page and link to it elsewhere. Keep installation instructions reproducible, distinguish placeholders from runnable examples, and explain uncertain outcomes instead of implying success. Review the complete pair a second time for consistency before recording hashes.

## Registry and compatibility

The [registry](translations.json) maps stable document IDs, routes, navigation groups, anchors, and reviewed file hashes. Root guides use `.ko.md`; other translations live under `docs/ko/`. Keep links within the current language where a counterpart exists.

Preserve existing routes and old heading anchors when renaming sections, using explicit compatibility anchors near the replacement section. New maintained guides require both languages and a registry entry. Hashes detect later edits; they do not prove semantic equivalence or replace editorial review.

After reviewing each changed pair, record its ID explicitly:

```bash
python3 -B scripts/check_docs.py record --id agent-integration
python3 -B scripts/check_docs.py
```

Do not refresh every hash just to silence a failure. Check that the rendered page, copied Markdown, language navigation, and preserved links still match the intended content.

<a id="documentation-site"></a>

## Build and preview

Use Node 24.21.0, npm 11.19.0, and Python 3.11 or later. CI uses Python 3.14. Set `DOCS_PYTHON` if the executable has another name. The site uses the committed VitePress lockfile; do not update dependencies as part of a wording change.

```bash
npm ci --prefix docs-site --ignore-scripts
npm test --prefix docs-site
npm run build --prefix docs-site
python3 -B docs-site/scripts/site.py check
python3 -B docs-site/scripts/site.py preview
```

Preview is `http://127.0.0.1:43141/CodeSpace/`. After edits, stop preview, rebuild, and restart preview: it serves only files matching the manifest loaded at startup. Use this verified static preview instead of the Vite development server. English is at the root and Korean under `/ko/`; guides use `/guide/` and `/ko/guide/`. Copy page uses the current language's maintained Markdown. Search runs locally in the browser.

Check desktop and narrow layouts in both themes, including long code/table content. Exercise navigation, language switching, search, and copying. A passing registry/build check alone does not establish readability or correctness.

<a id="publication"></a>

## Publication and attribution

PRs build a review artifact and do not deploy. A main push or manual main workflow packages the verified directory for GitHub Pages. Build metadata records the source commit and source hashes; a successful artifact upload is not proof of a live deployment.

The site implementation is MIT; project documentation is Apache-2.0. Preserve [site provenance](../docs-site/PROVENANCE.json), dependency notices, and licensing files. The publication workflow uses a reviewed external action recorded in [the deployment lock](../.github/docs-pages-deploy.lock.json); change it only as a separate reviewed dependency update. Repository Pages/environment settings are operator-managed, not created by this build.

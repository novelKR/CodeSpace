# 공개 문서와 문서 사이트

[English](../documentation.md) | [한국어](documentation.md)

영문 파일이 편집 원본입니다. 루트 가이드는 대응하는 `.ko.md` 파일을
사용합니다. `docs/` 아래 가이드는 `docs/ko/`를 사용합니다. 상호 언어
링크를 유지하세요. 명령, 코드 블록, 식별자, 지원 조건은 각 쌍에서
같아야 합니다.

[문서 레지스트리](../translations.json)는 안정 ID, 탐색 그룹, 보존
앵커, 원본/번역 경로, 검토한 파일 해시 양쪽을 기록합니다. 해시는
드리프트를 감지합니다. 의미 동등이나 사람 승인을 증명하지 않습니다.
쌍을 기록하기 전에 두 문서 전체를 검토하세요.

```sh
python3 -B scripts/check_docs.py
python3 -B scripts/check_docs.py record --id documentation
```

두 번째 명령은 해당 문서 쌍을 검토한 뒤 편집자가 사용합니다. 검토한
ID를 명시적으로 고르세요. 유지하는 새 Markdown 문서는 한국어판과
레지스트리 항목이 필요합니다. CI는 검토 기록을 자동으로 갱신하지
않습니다.

## 문서 사이트

사이트는 커밋된 [npm lock](../../docs-site/package-lock.json)과 함께
VitePress 1.6.4, Node 24.21.0, npm 11.19.0을 사용합니다. 사이트 구현은
MIT이며 docs-actions에서 복사해 여기서 맞췄습니다.
[사이트 출처](../../docs-site/PROVENANCE.json)를 보세요. 프로젝트 문서는
Apache-2.0입니다.

Vite 개발 서버를 시작하지 마세요. 저장소 루트에서:

```sh
npm ci --prefix docs-site --ignore-scripts
npm test --prefix docs-site
npm run build --prefix docs-site
python3 -B docs-site/scripts/site.py check
python3 -B docs-site/scripts/site.py preview
```

미리보기는 `http://127.0.0.1:43141/CodeSpace/`에서 수신합니다. 수정 후
다시 빌드하세요. 미리보기에는 핫 모듈 교체가 없습니다. 영어는 사이트
루트이고 한국어는 `/ko/` 아래입니다. 유지 문서는 `/guide/`와
`/ko/guide/`에 있습니다. 페이지 복사는 현재 언어의 유지 Markdown을
복사합니다. 로컬 검색은 브라우저에 남습니다.

Python 실행 파일 이름이 다르면 `DOCS_PYTHON`을 설정하세요. Python 3.11
이상을 사용합니다. CI는 Python 3.14를 고릅니다.

## 게시

끌어오기 요청은 읽기 전용 문서 빌드를 실행하고 검토 산출물을 보관합니다.
배포하지 않습니다. `main` push 또는 수동 `main` 실행은 같은 검증된
디렉터리를 패키징하고 핀된 docs-actions 재사용 워크플로를 호출합니다.
그 배포 job에만 `pages: write`와 `id-token: write`를 주세요.
`secrets: inherit`를 쓰지 마세요.

핀은 [`.github/docs-pages-deploy.lock.json`](../../.github/docs-pages-deploy.lock.json)입니다.
새 SHA는 해당 커밋, 라이선스 범위, 성공한 중앙 CI를 검토한 뒤에만
채택하세요.

GitHub Pages 소스, `github-pages` 환경, 공개 재사용 워크플로 허용은
저장소 설정입니다. 이 문서 빌드가 적용하지 않습니다. 성공한 로컬 빌드나
보관한 산출물은 라이브 사이트가 아닙니다.

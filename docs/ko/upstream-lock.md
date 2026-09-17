# 업스트림 고정

[English](../upstream-lock.md) | [한국어](upstream-lock.md)

CodeSpace는 OpenAI Codex를 **핀된** git 서브모듈로 재사용합니다. 단일
소스 파일을 벤더하지 않고, 독립 `apply_patch` 바이너리를 보안 경계로
감싸지 않으며, `main`을 따라가지 않습니다.

**현재 코드 재사용**은 `crates/patch`를 통한 `codex-apply-patch`(V4A의
파싱 / 검증 / 적용)입니다. 그 핀은 실행 구현 공급이며, 그만큼 좁은
크레이트만 따라온다는 서약이 아닙니다. 제품 런타임(App Server,
`codex-core`, `codex-exec`, login, models)은 **핵심** 밖에 둡니다.
응집력 있는 **실행 서브그래프**는 격리된 어댑터에서 가져올 수 있습니다
([codex-reuse.md](codex-reuse.md)). Codex 타입은 `crates/domain`이나
MCP 표면으로 새면 안 됩니다. 실행 전용 규칙:
[execution-substrate.md](execution-substrate.md). 오늘은 그래프에 추가
Codex 크레이트가 없습니다.

## 배포 핀 (W06)

| 필드 | 값 |
| --- | --- |
| Project | [openai/codex](https://github.com/openai/codex) |
| License | Apache-2.0 (see root `NOTICE`) |
| Tag | `rust-v0.154.0` |
| Commit | `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` |
| Path | `third_party/codex` git submodule |
| Crate | `codex-apply-patch` via Cargo path dependency from `crates/patch` |
| Apply options | `PreserveLineEndings`, `follow_symlinks: false` |
| Parity | subset in `tests/parity/` and `crates/patch` tests; not the full upstream suite |

`crates/patch`는 **격리된 Cargo 워크스페이스**입니다(저장소 루트
워크스페이스에서 제외). Codex 크레이트가 자체 `workspace.dependencies`를
유지하게 합니다. 그 `Cargo.lock`은 핀된 Codex lockfile에서 시작해
전이 크레이트(예를 들어 맞는 `rama-*` 알파)가 떠다니지 않게 합니다.
Codex `[patch.crates-io]` git 포크는 `crates/patch/Cargo.toml`에
복사됩니다.

어댑터는 `parse_patch`, 그다음 제품 경로 정책(심링크 조상 거절 포함),
그다음 같은 프로세스에서 `LOCAL_FS`로 `apply_patch_with_options`를
호출합니다. `git apply`나 독립 `apply_patch` 바이너리를 호출하지
않습니다. 라이브러리에 `sandbox: None`을 넘기는 것은 제품 샌드박스가
**아닙니다**. 정책 + no-follow I/O + Linux 러너입니다.

macOS에서 `/var`는 `/private/var`의 심링크입니다. 어댑터는 `PathUri`
cwd를 만들기 전에 워크스페이스 루트를 정규화해, no-follow 탐색이 그
호스트 별칭에서 실패하지 않게 합니다.

## 파일 복사 벤더가 금지인 이유

`codex-apply-patch` 0.154.0은 워크스페이스 크레이트입니다. 같은 저장소의
다른 크레이트에 의존합니다. 포함:

- `codex-exec-server`
- `codex-utils-absolute-path`
- `codex-utils-path-uri`
- tree-sitter 관련 워크스페이스 크레이트

`apply-patch` 소스를 `crates/patch`에 복사하면 빌드가 실패하거나 엔진을
조용히 포크합니다. 그래서 CodeSpace는 다음을 사용합니다.

```text
git submodule add https://github.com/openai/codex.git third_party/codex
git -C third_party/codex checkout 6b9826e3aa83b1a5947db50f4332cb9c65f1b340
```

경로 의존성이며 crates.io의 움직이는 버전이 아닙니다.

## 재사용하는 것과 거절하는 것

**이 핀에서, 코드로:** 파싱, 헝크 검증, 적용 API, 그리고 패리티용으로
고른 업스트림 픽스처(`crates/patch` → `codex-apply-patch`).

**그 크레이트가 허용해도 제품 기본값으로 거절:** 심링크 follow,
sandbox `None` 독립 CLI, 모델의 호스트 절대 경로, 조용한 `git apply`.

**CodeSpace *핵심* 의존성으로 거절:** `codex-protocol` 타입을 포함한
어떤 Codex 크레이트 경로 의존성. 제품 런타임은 어디에나 빼 둡니다.
App Server, `codex-core`, `codex-exec`, login, models.
[codex-reuse.md](codex-reuse.md)를 보세요. 이 SHA에서 재사용 선호
(아직 연결 안 함): `codex-process-hardening`, `codex-utils-pty`,
`codex-uds`, path/URI utils, `codex-file-search`. 적극 평가:
`codex-file-system`, `codex-shell-command`, `codex-linux-sandbox`
(전이 `codex-sandboxing`, `codex-network-proxy`; `codex-protocol`은
어댑터에서만 허용). `codex-exec-server-protocol`은 내부 DTO 후보입니다.
`codex-exec-server`는 참고 / 이후 백엔드이며 영구 거절은 아닙니다.

`codex-rs` 독립 `apply_patch`를 감싸고 그것을 샌드박스라고 부르지
마세요. Preview / `check_only`는 라이브러리 파싱과 CodeSpace
프리플라이트로 구현하며, `apply_patch --check`가 있다고 가정하지
않습니다.

Codex 세션 `permissionProfile`을 허용 경로로 취급하지 마세요.
게이트웨이 정책이 유일한 인가 권한입니다.

## 승격 규칙

1. 후보를 기록합니다 (W01).
2. W06: 서브모듈 + 어댑터 + 패리티 부분집합 (이 핀).
3. 패리티가 실패하면 **출하하지 마세요**. 어댑터 옵션을 바꾸거나 다른
   리비전을 고르세요. 불일치를 덮지 마세요.
4. W13: [upstream-update.md](upstream-update.md)를 따르세요. 배포
   단계로 최신 Codex `main`에 `git submodule update --remote`를 하지
   마세요. SHA나 패치 시험이 실패하면 `scripts/check-upstream-pin.sh`는
   빨간 상태로 남아야 합니다.

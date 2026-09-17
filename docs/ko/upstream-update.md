# 업스트림 핀 갱신

[English](../upstream-update.md) | [한국어](upstream-update.md)

Codex 리비전을 바꾸는 것은 **의도적 릴리스**이며, `main`으로
`git submodule update --remote`가 아닙니다. 패리티 부분집합이 실패하면
**출하하지 마세요**. 워크스페이스 의존성을 덮으려고 `apply-patch` 소스를
`crates/patch`에 복사하지 마세요.

현재 핀: [upstream-lock.md](upstream-lock.md).
제품 대 크레이트 기본값: [behavior-differences.md](behavior-differences.md).
apply-patch 외에 재사용할 수 있는 것: [codex-reuse.md](codex-reuse.md).
NOTICE는 Apache-2.0 Codex 귀속을 유지해야 합니다.

## 점검 목록

1. **태그 또는 커밋**을 고르세요(떠 있는 `main` 아님). 이유를 기록하세요.
2. `git submodule update --init third_party/codex`
3. `git -C third_party/codex fetch --tags`
4. `git -C third_party/codex checkout <commit>`
5. 격리된 어댑터를 다시 빌드하세요:
   `cargo test --manifest-path crates/patch/Cargo.toml`
   `cargo clippy --manifest-path crates/patch/Cargo.toml --all-targets -- -D warnings`
6. `docs/upstream-lock.md`의 Commit 칸을 새 SHA로 갱신한 뒤
   `scripts/check-upstream-pin.sh`를 실행하세요(서브모듈 HEAD ≠ lock
   파일이면 스크립트가 실패합니다).
7. 적용 옵션, 심링크 정책, 또는 파싱 오류가 바뀌면
   [behavior-differences.md](behavior-differences.md)를 갱신하세요.
8. 재사용 설명이나 핀 문자열이 바뀌면 [NOTICE](../../NOTICE)를 갱신하세요.
9. 핀의 **실행 서브그래프**를 [codex-reuse.md](codex-reuse.md)에 비춰
   판단하세요: 응집력 있는 실행 대 에이전트 / 모델 의미 대 Gateway 허용
   우회. process-hardening, PTY, UDS, filesystem, linux-sandbox,
   network-proxy의 diff는 실행/보안 변경 로그로 취급하세요. 후보 표를
   갱신하세요. 루트 워크스페이스에 Codex 경로 의존성을 추가하지 마세요.
   격리는 `crates/patch`와, 생기면 `crates/codex-runtime`
   (`codespace-codex-runtime`)에 남습니다.
10. 그 런타임 워크스페이스가 생기기 전까지 게이트는 SHA + 패치 패리티
    뿐입니다. 생기면 추가로: 런타임 어댑터 컴파일, 그리고 PTY /
    sandbox / process 회귀. 이 작업 패키지는 그 스위트를 추가하지
    않습니다.
11. PR을 여세요. CI는 핀 검사 **와** `crates/patch` 시험을 실행해야
    합니다. 빨간 패치 job은 경고가 아니라 실패한 배포입니다.

실패한 패리티 실행을 성공으로 표시하는 경로는 **없습니다**.

## 로컬 게이트

```bash
./scripts/check-upstream-pin.sh
```

서브모듈 SHA가 lock 파일과 다르거나
`cargo test --manifest-path crates/patch/Cargo.toml`이 실패하면
종료 코드가 0이 아닙니다.

`PIN_ONLY=1 ./scripts/check-upstream-pin.sh`는 SHA만 검사합니다.
CI는 그것을 fmt/clippy **전에** 실행한 뒤, SHA를 다시 검사하지 않고
패치 시험을 실행합니다. 로컬에서 접두 없는 스크립트는 여전히 SHA +
`crates/patch` 시험을 합니다.

## 금지

- 워크스페이스 크레이트 없이 `codex-rs/apply-patch`를 파일 복사 벤더
- 독립 `apply_patch` 바이너리를 보안 경계로 감싸기
- 조용한 `git apply` 폴백
- 패치 시험이 실패하는데 출하하기

# 보안 모델

[English](../security-model.md) | [한국어](security-model.md)

CodeSpace는 커널 샌드박스가 아닙니다. CoS와 cokacremote도 아닙니다.
인가는 **게이트웨이 정책**입니다. Linux 컨테이너 격리가 **목표** 실행
OS입니다. 현재 `exec_command`는 워크스페이스 cwd를 가진 호스트
프로세스입니다. Codex 패치 크레이트는 제품 경계를 공급하지 않습니다.
Codex 세션 설정과 `permissionProfile`은 허용 경로가 아닙니다. 게이트웨이와
러너는 둘 다 Rust입니다. 언어를 나눠도 신뢰 경계가 추가되지 않습니다.
프리미티브 대 제품:
[codex-reuse.md](codex-reuse.md). 실행 전용 기반:
[execution-substrate.md](execution-substrate.md).

## 신뢰 경계

1. **MCP client** — 인가에 대해 신뢰하지 않음. `workspace_id`,
   `approved: true`, 절대 경로를 포함해 어떤 도구 인자도 보낼 수
   있습니다. 그 인자는 권리를 부여하지 않습니다.
2. **Gateway (`crates/server` + `crates/policy`)** — 전송을 인증하고
   (HTTP 실험의 선택적 정적 Bearer), 등록된 워크스페이스를 고르며,
   프로필이 허용하지 않는 작업을 거절하도록 신뢰합니다.
3. **Runner (`crates/runner`, 오늘은 프로세스 내부)** — 경로 정책
   (`PathSandbox`)을 강제하고 이미 인가된 동작의 **호스트** 프로세스를
   감독하도록 신뢰합니다. 게이트웨이 비밀을 보도록 신뢰하지 않습니다.
   이후 Unix 소켓 / 컨테이너 분리는 같은 Rust 워크스페이스를 유지합니다.
   프로세스 경계이지 언어 경계가 아닙니다. `deploy/` 아래 compose는
   격리 픽스처이지 이 프로세스가 아닙니다.
4. **Patch helper (`codespace-patch` + `crates/patch`)** — 헬퍼 자식
   **안에서 프로세스 내부로** Codex V4A를 파싱/검증/적용하도록
   신뢰합니다. 게이트웨이는 JSON stdin/stdout으로 그 자식과 대화합니다.
   샌드박스로는 신뢰하지 않습니다(업스트림 독립 apply는 sandbox `None`을
   쓰고 심링크를 따를 수 있음). 폐기된 `native/patch-worker`가 아닙니다.

## 인증 대 선택

- 선택적 정적 Bearer는 **HTTP 실험 전용**입니다. OAuth 서버가 아닙니다.
  토큰은 로그나 오류 페이로드에 절대 나타나면 안 됩니다.
- `workspace_id`는 선택자입니다. id를 아는 것이 인증이 아닙니다.
- `work_id`와 `intent_id`는 선택자입니다. 아는 것이 인증이 아닙니다.
- 사용자 의도 본문은 지시입니다. 워크스페이스 프로필을 올리거나 경로
  정책을 우회하지 않습니다.
- ChatGPT 대화 id는 신뢰 기반이 아닙니다.

## 워크스페이스 레지스트리

워크스페이스는 모델이 아니라 **서버 설정**에 등록됩니다.

각 항목은 `workspace_id` → `{ root, profile }`을 매핑합니다.

알 수 없는 id는 거절됩니다. 경로는 엔진에 넘기기 **전에** 해석됩니다.
상대 경로만. 해석 후 그 워크스페이스 루트 안에 남아 있어야 합니다.

## 프로필 (MVP)

| 프로필 | 의미 |
| --- | --- |
| `read-only` | Default. `read` / `find` / `workspace_info` / `operation_status`. No patch, no shell. |
| `workspace-write` | Explicit. Mutating patch and shell **inside** the workspace. A live shell can delete workspace files; the product says so honestly. |
| `host-admin` | **Excluded from MVP.** |

바쁜 셸은 워크스페이스 쓰기 잠금을 잡습니다. 다른 변경 작업은 기다리거나
`WORKSPACE_BUSY`로 실패합니다.

## 제품 경로 정책 (크레이트가 허용해도 항상)

- 상대 경로만.
- 심링크 대상과 특수 파일(디바이스, 소켓, fifo)을 거절.
- 목적지가 이미 있으면 Add File을 거절.
- 목적지가 이미 있으면 Move를 거절.
- 워크스페이스 밖으로 `..`를 따르지 않음.
- 모델의 호스트 절대 경로를 엔진에 넘기지 않음.

[behavior-differences.md](behavior-differences.md)를 보세요.

## 러너 격리 (Linux)

**현재:** `exec_command`는 argv + 워크스페이스 cwd + `env_clear`로
호스트에서 실행됩니다. 경로 샌드박스는 `read` / `find` / versions /
rollback에 적용되며 Linux 네임스페이스가 아닙니다.

**목표 / 픽스처:** 비특권 컨테이너 사용자. 워크스페이스를 `/workspace`
(또는 동등한 전용 볼륨)에 마운트합니다. 다음을 마운트하지 **마세요**.

- host home
- SSH agent socket
- `/var/run/docker.sock`
- gateway `.env`, Bearer files, SQLite

[`deploy/compose.yml`](../../deploy/compose.yml)은 `sleep infinity`로 그
속성을 보여줍니다. `exec_command`에 연결되어 있지 않습니다. 지금은 러너
제어 소켓이 없습니다.

게이트웨이 단위 시험은 macOS에서 실행할 수 있습니다. 개발 노트북에서
Linux 격리를 검증했다는 주장이 아닙니다.

## 패치 정직성

상태: `applied`, `checked`, `rejected`, `failed_rolled_back`,
`failed_partial`, `unknown`.

- 성공한 `check_only` 미리보기 → 파일 변경 없음 (`checked`).
- 프리플라이트 / 정책 실패 → 파일 변경 없음 (`rejected`).
- 스냅샷을 복원한 적용 실패 → `failed_rolled_back`.
- 남은 드리프트가 있는 적용 실패 → `failed_partial` 또는 `unknown`.
- 디스크 해시가 헬퍼가 주장한 `after_version`과 일치하지 않으면
  `applied`를 보고하지 마세요(삭제는 없어야 함).
- `git reset --hard`를 쓰지 마세요.
- HTTP 타임아웃을 롤백이나 성공으로 취급하지 마세요.

`Store::begin`이 `operation_id`를 발급한 뒤, 실행 오류는 그 id를
`ErrorBody`에 포함합니다. 전송과 `begin` 전 거절은 포함하지 않습니다.

## 프로세스 정직성

`process_id` 값은 서버가 발급합니다. 클라이언트가 핸들을 만들어 낼 수
없습니다. 출력은 커서로 읽고 제한됩니다(프로세스당 256 KiB). 시간, 살아있는
프로세스 수, **완료 핸들 보존**(15분 또는 완료 슬롯 64개)이 제한됩니다.
연결 끊김이 프로세스가 죽었다는 뜻은 아닙니다. 프로세스 상태는 휘발성입니다.
SQLite에 저장되지 않습니다.

## 로깅

Authorization 헤더, Bearer 토큰, `.env` 값을 가리세요. 원본 요청을
덤프하기보다 구조화 필드(`workspace_id`, `operation_id`)를 선호하세요.
별도 감사 서브시스템은 없습니다.

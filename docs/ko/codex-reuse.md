<a id="codex-재사용-제품과-프리미티브"></a>
<a id="codex-재사용-제품과-프리미티브"></a>

# CodeSpace의 Codex 재사용 범위

[English](../codex-reuse.md) | [한국어](codex-reuse.md)

CodeSpace는 특정 버전에 고정한 Codex 소스의 실행 라이브러리를 선택적으로 사용합니다. 계획과 코드 생성은 외부 에이전트가 담당합니다. MCP, 작업 공간 권한, 작업 식별자, 프로세스 관리는 CodeSpace가 담당합니다.

<a id="재사용-단위는-서브그래프"></a>
<a id="재사용-단위는-서브그래프"></a>
<a id="지금-재사용-코드에서"></a>
<a id="지금-재사용-코드에서"></a>

## 실제 연결된 구성 요소

| 어댑터 | Codex 구성 요소 | 현재 역할 |
| --- | --- | --- |
| `crates/patch` | `codex-apply-patch`, `codex-exec-server::LOCAL_FS`, 경로 도구, 프로세스 보호 | `codespace-patch` 안에서 V4A 파싱·적용 |
| `crates/codex-runtime` | `codex-process-hardening`, `codex-uds` | 선택적 worker의 보호 설정과 Unix 소켓 |
| `crates/pty` | `codex-utils-pty` | `tty: true`의 터미널 실행 |
| `crates/file-system` | `codex-file-system`, `LOCAL_FS`, 경로 도구 | 심볼릭 링크를 따라가지 않는 Runner 파일 I/O와 범위가 제한된 탐색 |
| `crates/linux-sandbox` | `codex-linux-sandbox`, `codex-sandboxing`, `codex-protocol`, `codex-network-proxy` | 실행 파일 전용 명령 샌드박스 도우미와 enabled 네트워크 프록시 |

의존성이 존재한다고 그 구성 요소의 서비스 전체가 실행되는 것은 아닙니다. 예를 들어 파일·패치 어댑터는 `codex-exec-server`의 `LOCAL_FS`를 사용하지만 그 서버를 일반 명령 실행 백엔드로 사용하지는 않습니다. Linux 도우미가 프록시와 샌드박스 변환을 관리하며 공개 타입은 CodeSpace 타입으로 유지합니다.

<a id="핵심-대-어댑터"></a>
<a id="핵심-대-어댑터"></a>
<a id="apply-patch-패턴-격리이지-크레이트-너비가-아님"></a>
<a id="apply_patch-패턴-격리이지-크레이트-너비가-아님"></a>
<a id="격리-층-전이-codex-protocol"></a>
<a id="격리-층-전이-codex-protocol"></a>

## 어댑터 경계

```text
에이전트 → CodeSpace MCP·정책·저장소 → Runner 계약
                                      → 어댑터 → Codex 실행 라이브러리 → OS
```

핵심 crate에는 직접적인 Codex 의존성이 없습니다. 어댑터는 고정된 업스트림의 workspace 의존성을 수용하기 위해 별도의 Cargo workspace로 구성합니다. 파일 시스템·PTY 어댑터는 Runner의 라이브러리 의존성이며, 패치와 Linux 샌드박스는 도우미 프로세스를 사용합니다. Cargo workspace를 분리하는 것만으로 프로세스나 보안 경계가 생기지는 않습니다.

Linux 샌드박스 도우미는 실행 파일만 제공합니다. `codespace-linux-sandbox-protocol`에는 CodeSpace가 정의한 핸드셰이크 데이터만 있고 Codex 타입은 없습니다. worker의 UDS 프로토콜 버전 6과 샌드박스 도우미 프로토콜 버전 1은 별개의 계약입니다.

<a id="정책-대-메커니즘"></a>
<a id="정책-대-메커니즘"></a>
<a id="감독-코드가-아직-있는-이유"></a>
<a id="감독-코드가-아직-있는-이유"></a>
<a id="코드는-허용-권한은-금지"></a>
<a id="코드는-허용-권한은-금지"></a>
<a id="거절"></a>
<a id="거절"></a>
<a id="codespace에-남는-것"></a>
<a id="codespace에-남는-것"></a>

## 권한 결정은 CodeSpace에서 수행

게이트웨이 정책은 작업 공간에서 허용할 행동을 결정합니다. Codex 실행 코드는 no-follow 파일 접근, PTY 생성, 샌드박스 설정 같은 기능을 구현합니다. Codex 세션 권한, 로그인, 모델 선택, 에이전트 루프를 가져오는 것은 이 책임 구분을 바꾸는 일이며 현재 제품에 포함되지 않습니다.

현재 진입점은 `codex-core`, `codex-exec`, Codex App Server를 제품 런타임으로 내장하지 않습니다. 간접 의존성 그래프의 범위와 실제 호출 경로는 별도로 평가합니다. [보안 경계](security-model.md)를 참고하세요.

<a id="단계적-가져오기-그-wp가-생길-때"></a>
<a id="단계적-가져오기-그-wp가-생길-때"></a>
<a id="핀-6b9826e의-후보"></a>
<a id="핀-6b9826e의-후보"></a>
<a id="재사용-선호-그-wp가-올-때"></a>
<a id="재사용-선호-그-wp가-올-때"></a>
<a id="조건부-적극-평가"></a>
<a id="조건부-적극-평가"></a>
<a id="내부-프로토콜-후보"></a>
<a id="내부-프로토콜-후보"></a>
<a id="실험적-백엔드-지금은-아님"></a>
<a id="실험적-백엔드-지금은-아님"></a>
<a id="이후-environment-p0-아님"></a>
<a id="이후-environment-p0-아님"></a>
<a id="다음-구현-wp"></a>
<a id="다음-구현-wp"></a>

## 업데이트와 향후 검토

재사용하는 구성 요소는 현재 모두 [같은 고정 버전](upstream-lock.md)에서 가져옵니다. 업스트림 수정은 버전 갱신과 검증을 거쳐야 반영되며 자동으로 들어오지 않습니다. [업데이트 검사](upstream-update.md)는 패치뿐 아니라 연결된 모든 어댑터를 포함합니다.

`codex-file-search`, 셸 명령 파싱, worktree 준비, 범용 `codex-exec-server` 백엔드는 아직 연결되지 않은 후보입니다. 도입 시 제공하는 실행 기능, 빌드·업데이트 비용, 모델이나 권한 결정 책임이 어댑터 경계를 넘는지를 검토합니다. 사용자에게 영향을 주는 현재 제약은 [Agent Loop 연동](agent-integration.md)에 정리되어 있습니다.

<a id="업스트림-고정"></a>
<a id="업스트림-고정"></a>

# Codex 고정 버전

[English](../upstream-lock.md) | [한국어](upstream-lock.md)

현재 Codex 실행 어댑터는 모두 `third_party/codex`의 Git 서브모듈을 사용합니다. 커밋을 고정하여 공통 구현을 재현할 수 있게 하며 업스트림의 새 릴리스가 자동으로 반영되지는 않습니다.

<a id="배포-핀-w06"></a>
<a id="배포-핀-w06"></a>

## 배포 기준 버전

| 항목 | 값 |
| --- | --- |
| 프로젝트 | [OpenAI Codex](https://github.com/openai/codex) |
| 라이선스 | Apache-2.0. 출처는 [NOTICE](../../NOTICE)에 기록 |
| 태그 | `rust-v0.154.0` |
| 커밋 | `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` |
| 경로 | `third_party/codex` |
| 하위 패치 | 1개. 이 커밋의 파일에 적용함. [하위 패치](#codex-downstream-patch) 참고 |
| 사용하는 구성 요소 | 패치, 런타임 worker, PTY, 파일 시스템, Linux 샌드박스·프록시 어댑터 |
| 패치 옵션 | `PreserveLineEndings`, `follow_symlinks: false` |

정확한 역할은 [연결된 구성 요소](codex-reuse.md)를 참고하세요. 패치 테스트는 선택된 동작 일치 사례를 검사하며 업스트림 전체 테스트나 Codex의 모든 동작을 검증하지는 않습니다.

<a id="codex-downstream-patch"></a>

## 하위 패치

CodeSpace는 `codex-utils-pty`에 대한 패치 하나를 유지합니다. 서브모듈은 `openai/codex`의 위 커밋에 그대로 두고 그 파일에 패치를 적용하며, 나머지 Codex crate는 모두 태그와 같습니다. `scripts/codex_patch.py`는 이 표에서 패치 파일, 그 digest, 패치를 적용한 tree를 읽습니다.

| 항목 | 값 |
| --- | --- |
| 패치 | `third_party/codex-patches/0001-utils-pty-pre-reap-observation.patch`, SHA-256 `6f61579830f311e3c259c0642864ea9eee3c653f12c7e838e97e9b1dd35b1fba` |
| 패치 적용 tree | `1999937194e3a68d982be85679557f0e50fe5408` |
| 파일 | `codex-rs/utils/pty/src`의 `observe.rs`(신규), `observe_tests.rs`(신규, 테스트 전용), `pty.rs`, `lib.rs`. 제품 코드 304줄 추가, 5줄 삭제, 테스트 9개 |
| 기능 | spawn 전에 `observe::ExitObserver`를 등록하는 `spawn_pty_process_observed`. 자식의 단일 waiter가 회수하지 않고 종료를 보고(`WNOWAIT`를 쓴 `waitid`), 관찰자에게 그 bound 안에서 한 번의 기회를 준 뒤 전과 같이 회수하고 `Completed`, `TimedOut`, `Failed`, `Unavailable` 중 하나를 보고. 관찰자가 없으면 모든 경로가 그대로. Unix가 아닌 플랫폼은 관찰 spawn을 거부 |
| 이유 | Codex가 루트를 회수하기 전에 관리 PTY 실행의 scope를 관찰하기 위함([CSRG-U5](devguard-integration.md#devguard-lifecycle)). 2026-10-11 소유자 결정. 계약은 [업스트림 어댑터 작업 명세](https://github.com/novelKR/DevGuard/blob/668750b4ab6ad582904c3d05ffceffa062b26243/docs/handoff/2026-09-28-cs-dg-upstream-adapter-work-spec-1.md#8-preferred-investigation-b-an-upstream-owned-bounded-pre-reap-observation-contract) 8절 |
| CodeSpace 사용 | 아직 없음. PTY 어댑터는 여전히 `spawn_pty_process`를 호출하며, 관찰자 연결은 별도 변경에서 함 |
| 출처 | W3 실험 B의 관찰자([DevGuard 결정 패킷 3.2절](https://github.com/novelKR/DevGuard/blob/668750b4ab6ad582904c3d05ffceffa062b26243/docs/handoff/2026-09-28-w3-decision-packet.md#32-experiment-b-backend-owned-pre-reap-observation-packet-132))를 이 릴리스의 두 Unix PTY 경로로 옮긴 것. 수정한 파일마다 CodeSpace 수정 고지가 있음(Apache-2.0 4(b)) |
| 검증 | 패치 적용 tree에서 함. Linux x86_64: crate 테스트 36개를 30회 반복해 모두 통과. macOS: 테스트 38개를 30회 반복해 모두 통과, DevGuard `6e7e065`로 W3 실험 B의 B1-B13 통과. Linux, macOS, Windows 대상 clippy 통과 |
| 업스트림 | Codex는 외부 pull request를 받지 않음. 기능 요청 초안만 있고 제출하지 않음 |
| 제거 조건 | Codex 릴리스가 같은 관찰(단일 waiter, bound, 결과 보고)을 제공하고 CodeSpace가 그것으로 옮기거나, CodeSpace가 더 이상 회수 전에 PTY 실행을 관찰하지 않으면 패치를 뺌 |
| 핀 변경 시 | 패치를 되돌리고 서브모듈을 옮긴 뒤, 후보에 패치를 다시 옮기거나 제거 조건에 따라 빼고, Linux와 macOS에서 테스트를 다시 실행하고 이 기록을 갱신 |
| 되돌리기 | CodeSpace의 관찰자 사용을 먼저 되돌린 뒤, 패치 파일과 이 기록을 함께 되돌림 |

<a id="devguard-client-pin"></a>

## DevGuard client 고정 버전

gateway([CSRG-U1](devguard-integration.md#devguard-status-connection))와 UDS worker([CSRG-U2](devguard-integration.md#devguard-owner-registration))의 opt-in `devguard` feature는 검토를 거친 변경 불가능한 커밋 하나에서 DevGuard의 범용 client를 링크합니다.

| 항목 | 값 |
| --- | --- |
| 프로젝트 | [DevGuard](https://github.com/novelKR/DevGuard) |
| 라이선스 | Apache-2.0. 저작권자는 CodeSpace와 같음 |
| 커밋 | `6e7e065a0dd5b3876dc7f4546ab76069489937d1`. [DevGuard PR #27](https://github.com/novelKR/DevGuard/pull/27) 병합 후의 DevGuard `main` |
| 소스 | 그 `rev`의 Cargo Git 의존성 |
| crate | 제품에는 `devguard-client`와 `devguard-contract`. `test-fixtures`를 켠 `devguard-daemon`은 어댑터 테스트에서만 사용 |
| launch helper | `devguard-launch`는 링크하지 않음. 운영자는 socket을 제공하는 DevGuard release의 helper를 지정하고(`--devguard-launch-helper`, [CSRG-U4](devguard-integration.md#devguard-launch)), launch 테스트는 DevGuard 자체 lockfile로 이 커밋에서 빌드함 |
| 사용하는 구성 요소 | `crates/devguard`(`codespace-devguard`). `codespace-server`, `codespace-runner`, `codespace-codex-runtime`은 각자의 `devguard` feature를 켰을 때만 링크 |
| 검사 | `scripts/upstream_dependencies.py`(`DEVGUARD_SOURCE`) |

의존성 검사는 모든 제품 그래프에서 각 `devguard-*` package를 이 커밋에서만, 각 `codex-*` package를 Codex gitlink에서만 받아들입니다. 제품 그래프에는 `devguard-client`와 `devguard-contract`만 허용하므로 DevGuard의 테스트 fixture는 개발 의존성으로만 남습니다. feature 없이 빌드한 그래프의 DevGuard package와, DevGuard crate가 도달하는 CodeSpace·Codex package는 거부합니다. root와 worker 그래프는 feature를 켠 경우와 끈 경우를 모두 검사합니다. 고정 버전은 어댑터 manifest, 이를 담은 세 lockfile(root, 어댑터, worker), `DEVGUARD_SOURCE`, 이 기록을 함께 갱신하는 검토된 PR로만 바꿉니다.

<a id="파일-복사-벤더가-금지인-이유"></a>
<a id="파일-복사-벤더가-금지인-이유"></a>
<a id="재사용하는-것과-거절하는-것"></a>
<a id="재사용하는-것과-거절하는-것"></a>

## Cargo workspace와 잠금 파일

패치 파서를 복사하여 별도 구현으로 유지하지 않고 업스트림의 workspace 의존성을 사용할 수 있도록 어댑터를 별도 Cargo workspace로 구성합니다. 어댑터 잠금 파일과 필요한 업스트림 Cargo 패치를 유지하세요. 파일 시스템·샌드박스 어댑터는 호환되지 않는 alpha·stable 혼합을 피하기 위해 일치하는 Rama alpha 의존성을 고정합니다.

패치 어댑터는 `codespace-patch` 내부에서 라이브러리를 호출하며 업스트림의 독립 `apply_patch` 실행 파일을 호출하지 않습니다. `LOCAL_FS`와 경로 도구는 구현 의존성입니다. Codex 사용자 설정은 작업 공간 접근 권한을 부여하지 않습니다.

<a id="승격-규칙"></a>
<a id="승격-규칙"></a>

## 확인과 갱신

```bash
python3 scripts/codex_patch.py apply
PIN_ONLY=1 ./scripts/check-upstream-pin.sh
```

clone하거나 서브모듈을 갱신할 때마다 빌드 전에 [기록된 패치](#codex-downstream-patch)를 적용합니다. `scripts/validate-upstream.py`는 검증할 때마다 먼저 적용합니다. 고정 버전 검사는 서브모듈 커밋이 이 문서와 일치하는지, 서브모듈의 파일이 기록된 패치 적용 tree와 정확히 같은지만 검사합니다. `PIN_ONLY`를 생략하면 패치 테스트도 실행하지만 다른 어댑터 검증을 대신하지는 않습니다. 서브모듈을 옮기기 전에는 `python3 scripts/codex_patch.py revert`를 실행하고, 버전을 바꾸기 전에 [전체 업데이트 절차](upstream-update.md)를 따르세요. 배포 과정에서 `git submodule update --remote`로 최신 버전을 따라가지 않습니다.

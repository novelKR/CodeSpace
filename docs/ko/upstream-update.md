<a id="업스트림-핀-갱신"></a>
<a id="업스트림-핀-갱신"></a>

# Codex 의존성 업데이트

> **상태: 구현 지시로서 효력 중지.** ‘향후 crate와 백엔드의 경계’ 절에서 언급한 CS-RG의 새 실행 backend 계획은 구현하지 않습니다. 의존성과 CI 규칙은 그대로입니다. CodeSpace 통합 계획에 대해 소유자가 지시한 검토인 CS-RG 통합 경계 재검증(작업 단위가 아닙니다)이 끝날 때까지 적용됩니다. 대체 구조는 승인되지 않았으며, 소유자가 재검증 결과를 검토한 뒤 결정합니다. 이 표기는 지시의 효력만 중지하며 어떤 안전 요구도 완화하지 않습니다. 아래 내용은 이력 추적을 위해 바꾸지 않고 남깁니다.

[English](../upstream-update.md) | [한국어](upstream-update.md)

고정 버전은 검토 가능한 PR로 변경합니다. 업데이트는 [Codex 재사용 범위](codex-reuse.md)에 있는 모든 어댑터의 프로세스·파일 시스템·네트워크 동작에 영향을 줄 수 있습니다.

<a id="점검-목록"></a>
<a id="점검-목록"></a>

## 후보 검토

태그나 커밋을 명시적으로 선택하고 이유를 기록합니다. 패치 처리, PTY, 프로세스 보호, 파일 시스템, 샌드박스, 프록시의 관련 변경을 확인하세요. 런타임 의존성과 개발용 의존성은 구분합니다. 개발용 의존성이 있다는 사실만으로 제품 실행 파일에 포함된다고 볼 수는 없습니다.

서브모듈, [버전 기록](upstream-lock.md), 영향받는 어댑터 잠금 파일, 필요한 출처 고지를 함께 갱신합니다. 핵심 타입과 Codex 어댑터 타입의 분리를 유지하세요. 호환되지 않는 의존성을 감추기 위해 업스트림 crate 하나를 제품에 복사하지 않습니다.

현재 업스트림 코드의 제한적 adaptation은 없습니다. [adaptation 정책](codex-reuse.md)에 따라 추가한다면 고정 버전을 바꿀 때마다 검토합니다. 기록된 재검토 조건에 따라 후보의 업스트림 소스와 비교하고, 출처와 달라진 동작 기록을 갱신하고, 테스트를 다시 실행하며, 제거 조건을 충족하면 제거하거나 업스트림으로 다시 수렴합니다. 새 업스트림 API를 도입하려면 이 별도의 버전 갱신이 필요하며 다른 변경의 부수 효과로 도입하지 않습니다.

<a id="로컬-게이트"></a>
<a id="로컬-게이트"></a>

## 제출 전 검증

1. `PIN_ONLY=1 ./scripts/check-upstream-pin.sh`로 고정 커밋을 확인합니다.
2. 루트 workspace와 분리된 patch, codex-runtime, pty, file-system, linux-sandbox 어댑터에서 `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, 테스트를 수행합니다.
3. 패치·런타임·Linux 도우미를 빌드하고 통합 테스트가 의도한 실행 파일을 사용하게 설정합니다. workspace 통합·프로토콜 테스트를 실행합니다.
4. bubblewrap과 네임스페이스를 지원하는 Linux에서 `CODESPACE_REQUIRE_LINUX_SANDBOX=1`로 격리 테스트를 실행합니다. restricted 차단과 enabled 프록시 동작을 포함합니다.
5. [CI](../../.github/workflows/ci.yml)에 정의된 의존성 정책 검사와 Runner의 금지된 샌드박스 도우미 라이브러리 의존성을 확인합니다.
6. 기본값·오류·제약이 바뀌면 동작 문서와 두 언어를 함께 검토합니다.

실행 어댑터도 바뀌는 업데이트를 패치 일부 테스트만으로 검증할 수는 없습니다. 명령 결과를 후보 SHA와 연결하고 수행하지 못한 플랫폼 검사는 명시하세요. 현재 검사 목록의 실행 가능한 기준은 CI workflow입니다.

<a id="금지"></a>
<a id="금지"></a>

## 반영과 되돌리기

PR에 이전·새 SHA, 동작 변경, 테스트 근거를 기록합니다. 호환성 검사가 실패한 채 병합하거나 다른 패치 엔진으로 조용히 대체하지 않습니다. 되돌릴 때는 서브모듈, 어댑터 잠금 파일, 필요한 Cargo 패치, 문서를 함께 되돌리고 영향받는 검사를 다시 수행합니다. 고정 커밋 불일치는 경고가 아닌 오류입니다.

## 재현 가능한 검증 보고서

전체 로컬 검증은 `python3 scripts/validate-upstream.py all`로 실행합니다.
CI도 같은 이름의 단계를 병렬 실행하며 목록은 `--help`로 확인합니다.
보고서와 명령 로그는 기본적으로 `target/upstream-reports/local`에 생성됩니다.
시도마다 `--output`으로 다른 Git 무시 디렉터리를 지정하거나 이전 결과를 보관합니다.
보고서에는 소스 HEAD, Codex SHA, Rust 호스트, 소스와 lockfile 해시가 기록되며
실행 중 입력이 바뀌면 실패합니다. `passed`는 기록된 단계만의 통과를 뜻합니다.
macOS에서는 Linux 격리를 `not_run`, 전체 결과를 `incomplete`로 표시합니다.
Linux CI 근거가 별도로 필요하며 macOS CI는 PTY와 파일 시스템 계약도 검사합니다.
한 플랫폼 결과만으로 다른 플랫폼 검증을 대체하지 않습니다.

CI는 변경마다 모든 job을 실행하지 않습니다. 계획 job이 `scripts/ci-policy.json`에
따라 필요한 leg를 고릅니다. crate를 바꾸면 그 crate를 컴파일하는 모든 leg를,
문서만 바꾸면 아무 leg도 실행하지 않고, 빌드 입력·CI 파일·알 수 없는 경로를
바꾸면 전체를 실행합니다. policy·Python·pin·format 단계는 항상 실행합니다.
필수 `rust` job은 계획된 모든 leg가 검사한 커밋의 보고서와 함께 성공하고
나머지 leg는 모두 건너뛰었을 때만 통과합니다. 매일 예약 실행과 수동 실행은
전체 검증입니다. 계획은 `python3 scripts/ci_plan.py --base <revision>`으로
미리 볼 수 있습니다. CI는 증분 데이터와 debuginfo 없이 빌드하며, 각 job은
`main`만 저장하는 의존성 캐시를 복원합니다.

의존성 검사는 `--locked`와 대상 플랫폼 필터를 사용한 Cargo metadata에서
제품 root의 일반·빌드 의존성을 탐색하고 개발용 관계는 제외합니다.
에이전트·제품 crate 및 Runner에서 샌드박스 라이브러리로 향하는 경로는 실패합니다.
이는 패키지 도달 가능성 검사이며 모든 API가 실제 실행됨을 뜻하지 않습니다.
Cargo feature 통합으로 선택적 관계가 보수적으로 포함될 수 있습니다.

동일한 Rust target의 보관된 결과와 후보 결과를 비교할 수 있습니다.
기준 결과를 자동 갱신하지 않습니다.

```bash
python3 scripts/upstream_dependencies.py --target x86_64-unknown-linux-gnu \
  --compare target/baseline/dependencies.json \
  --output target/candidate/dependencies.json
```

보고서는 추가·삭제된 패키지와 관계를 나열합니다. 버전·출처 변경은 삭제와 추가로
표시됩니다. 일반 변화는 검토 대상이며 금지 의존성, 잘못된 metadata, 누락된 root,
Cargo 실패는 검증 실패입니다. 경로 식별자는 저장소 상대 경로를 사용합니다.
CI는 실패 시에도 보고서와 로그를 업로드합니다. 기존 pin 검사 스크립트는
SHA와 패치 테스트만 확인하며 전체 검증 명령을 대체하지 않습니다.

## 향후 crate와 백엔드의 경계

> **상태: 구현 지시로서 효력 중지.** CS-RG 통합 경계 재검증이 끝날 때까지 아래에서 언급한 CS-RG의 새 실행 backend 계획을 구현하지 않습니다. 대체 구조는 승인되지 않았습니다. 이 절의 의존성과 CI 규칙은 그대로입니다. 다만 이 절 끝에 2026-09-30 날짜로 추가한 항목은 DevGuard binding의 Codex 정체성에 대한 DevGuard 설계 개정 2의 규칙을 기록합니다.

CS-RG는 DevGuard 자원 client crate와 새 실행 백엔드를 계획합니다
([CS-RG 작업 패키지](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/planning/milestones/CS-RG.md)의
CSRG-C01과 CSRG-C03). 아직 존재하지 않으므로 의존성·CI 정책에는 이들의 제품 root나
component가 없습니다. 아래 규칙은 이를 추가하는 PR을 열 때 적용하며, 실행 가능한
검사가 아니라 문서 정책입니다.

현재 검사는 이미 다음 경계를 강제합니다.

- `scripts/upstream_dependencies.py`는 루트 workspace와 `ADAPTERS`의 각 workspace를
  검사합니다. workspace 구성원이 `PRODUCTS` 항목과 다르면 실패합니다("missing or
  unexpected product roots"). `FORBIDDEN`(`codex-core`, `codex-exec`,
  `codex-app-server`, `codex-login`)은 모든 제품 root에서, `RUNNER_FORBIDDEN`
  (`codespace-linux-sandbox`, `codex-linux-sandbox`)은 Runner에서 거부합니다.
- `scripts/ci-policy.json`의 `components`는 `Cargo.toml`이 있는 `crates/` 디렉터리와
  같아야 합니다(`test_components_are_the_crates`). 추적되는 모든 경로는 분류되어야
  하며, 각 leg의 `compiles` 목록은 그 단계가 빌드하는 crate와 일치해야 합니다
  (`scripts/tests/test_ci_plan.py`).
- `scripts/check-no-model-deps.sh`는 기존 어댑터마다 직접 Codex 키를 그 어댑터의
  허용 목록과 비교합니다.

`scripts/ci-policy.json`의 `full` 목록에는 `Cargo.toml`, `Cargo.lock`,
`**/Cargo.toml`, `**/Cargo.lock`, `third_party/**`, `.gitmodules`,
`.github/**`, `scripts/**`, `docs/upstream-lock.md`와 toolchain·`.cargo`·`.gitignore` 파일이
있습니다. 따라서 manifest·lockfile, Codex 서브모듈, Codex 버전 기록, CI 파일,
정책 스크립트를 바꾸면 이미 모든 leg를 실행합니다. 기존 crate 안에 추가하는 실행
계약·spawn guard·백엔드 코드는 그 crate의 component(예: `crates/runner/**`)가
다룹니다. crate를 추가하는 PR은 manifest를 추가하므로 모든 leg를 실행하지만, 이후
그 crate의 소스 변경은 해당 component를 통해서만 leg를 선택합니다. DevGuard client
pin과 helper 출처를 기록할 위치는 아직 정하지 않았으므로(CSRG-C01) 기존 trigger가
그 기록을 다룬다고 주장하지 않습니다. 그 기록을 만드는 PR이 이를 `full`이나
component에 추가합니다.

crate, 백엔드, 테스트 workspace를 추가하는 PR은 다음을 함께 갱신합니다. manifest와
lockfile, `PRODUCTS`(분리된 workspace이면 `ADAPTERS`도), 직접 Codex 키를 사용하는
경우 `check-no-model-deps.sh`의 어댑터 허용 목록, `ci-policy.json`의 component와
그 crate를 빌드하는 모든 leg의 `compiles` 목록, 그리고 이를 다루는 테스트입니다.
`FORBIDDEN`, `RUNNER_FORBIDDEN`, 전체 그래프 검증이나 다른 검사를 제거하거나 좁히지
않습니다. 계획된 자원 client는 간접 Codex 의존성을 가져오면 안 되며, DevGuard
client 타입은 공개 MCP 타입에 들어가지 않습니다.

*2026-09-30 추가,
[DevGuard 설계 개정 2](https://github.com/novelKR/DevGuard/blob/637627ecff26a9f71730d00b926b40fb05b47107/docs/ko/design-revision-2.md#33-실행-파일-정체성)에서:*
CodeSpace 실행 파일에 링크되는 DevGuard binding은 이 저장소의 gitlink를 통해 Codex를
소비하므로, 그 실행 파일은 검토된 Codex 소스 정체성을 하나만 유지합니다. 제품 root
안에서 각 `codex-*` package를 gitlink 경로에서만 받아들이는 실행 파일 단일 정체성
검사는 DevGuard crate를 CodeSpace graph에 처음 넣는 PR에서 추가됩니다. 그때까지 이
규칙은 실행 가능한 검사가 아니라 문서 정책입니다.

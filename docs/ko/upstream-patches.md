<a id="upstream-patches"></a>

# 고정 버전의 Codex 수정

[English](../upstream-patches.md) | [한국어](upstream-patches.md)

CodeSpace는 [고정한 릴리스](upstream-lock.md) 하나의 Codex 코드를 실행합니다. 이 문서는 그 릴리스로 CodeSpace에 필요한 일을 할 수 없을 때 무엇을 할지, 그리고 답이 Codex 코드를 바꾸는 것이라면 어떻게 바꿀지를 정합니다. 바꾸는 방식은 공식 릴리스 위에 기록된 패치를 얹는 것 하나이며, fork는 쓰지 않습니다. 아래 [이전 사례](#earlier-cases)는 CodeSpace가 이런 차이를 지금까지 어떻게 처리했는지 보여 줍니다. 정책은 그 사례에서 나왔고, 이후의 결정도 같은 길을 따르게 하려는 것입니다. 지금 적용 중인 패치는 [버전 기록](upstream-lock.md#codex-downstream-patch)에 있습니다.

<a id="patch-policy"></a>

## 정책

<a id="when-to-patch"></a>

### 패치를 쓰는 경우

선택지를 다음 순서로 검토하고, 앞의 선택지가 맞지 않는 이유를 기록합니다.

1. 고정한 릴리스의 공개 API를 쓰고, 모자란 부분은 CodeSpace 쪽 어댑터에서 처리
2. 그 기능이 이미 있는 Codex 릴리스로, 검토를 거친 [버전 갱신](upstream-update.md)을 통해 옮김
3. 이 정책에 따라 고정한 릴리스에 기록된 패치를 얹음
4. 해당 Codex 메커니즘을 CodeSpace가 직접 구현해 대체

작업을 미룰 수 있는 동안에는 모자란 부분을 문서화된 제약으로 두고 테스트로 고정해 둘 수도 있습니다.

패치는 CodeSpace가 지금 하기로 한 일을 고정한 릴리스가 막고 있고, 어댑터로 메울 수 없으며, 그 일만을 위해 버전을 옮기는 것이 타당하지 않을 때만 고릅니다. 범위는 프로세스, PTY, 터미널, descriptor, 입출력 도구 같은 실행 메커니즘으로 제한합니다. `codex-core`의 제품 의미, 세션 권한, 에이전트 루프는 바꾸지 않으며, 검사가 봐야 할 의존성을 숨기지 않습니다.

패치가 CodeSpace의 직접 구현보다 앞서는 이유는 CodeSpace가 Codex에 맡긴 메커니즘 위에서 얇은 어댑터로 남기 때문입니다. DevGuard 연동만을 이유로 그런 메커니즘, 특히 PTY를 CodeSpace 안으로 옮기지 않습니다([경계 재검토](https://github.com/novelKR/DevGuard/blob/668750b4ab6ad582904c3d05ffceffa062b26243/docs/handoff/2026-09-27-cs-rg-boundary-revalidation.md#1-why-this-review-happened) 1절). [Codex 재사용 범위](codex-reuse.md)의 '관리 실행의 재사용 결정' 절도 관리 실행에 대해 같은 순서를 기록했지만, 그 절은 구현 지시로서 효력이 중지된 상태입니다. 따라서 고정 버전에서 하는 모든 변경의 순서는 이 문서가 정합니다.

<a id="patch-form"></a>

### 형식

- 서브모듈은 `openai/codex`의 공식 릴리스 커밋에 그대로 둡니다. gitlink는 수정한 커밋을 가리키지 않고, CodeSpace는 Codex fork를 두지 않습니다.
- 패치 하나는 `third_party/codex-patches/`의 `git format-patch` 파일 하나입니다. 고정 커밋을 기준으로 만들고, 적용 순서대로 번호를 붙입니다.
- `scripts/codex_patch.py`가 서브모듈의 파일에 패치를 적용하며, 결과는 기록된 패치 적용 tree와 같아야 합니다. 고정 버전 검사와 `scripts/validate-upstream.py`는 패치가 빠졌거나 체크아웃에 다른 변경이 있으면 거부합니다.
- Codex crate를 CodeSpace로 복사하지 않고, Cargo `[patch]` 항목으로 Codex crate를 수정한 사본에 연결하지 않습니다. Codex 자체 workspace에서 가져온 `[patch]` 항목은 서드파티 crate를 OpenAI의 fork에서 빌드하게 하는 것으로, CodeSpace의 변경이 아니므로 그대로 둡니다.
- 패치는 호출하는 쪽이 선택하지 않는 한 업스트림 동작을 바꾸지 않습니다. 수정한 파일마다 수정 고지(Apache-2.0 4(b))를 넣고, Linux와 macOS에서 실행되는 테스트를 패치한 crate에 더합니다.

<a id="patch-record"></a>

### 기록

[버전 기록](upstream-lock.md#codex-downstream-patch)에는 패치마다 파일과 SHA-256, 패치 적용 tree, 바꾼 파일, 기능, 이유, CodeSpace에서의 사용, 출처, Linux와 macOS 검증, 업스트림 상태, 제거 조건, 버전 변경 때의 처리, 되돌리기를 적습니다. 기록과 패치는 같은 PR에서 바뀝니다. `scripts/codex_patch.py`가 기록을 읽으므로 둘이 서로 어긋날 수 없습니다.

<a id="patch-upstream"></a>

### 업스트림 우선

Codex는 코드 변경을 자기 팀에서만 받고, 기능 요청은 요청자의 사용 사례와 함께 issue tracker에서 받습니다([기여 안내](https://github.com/openai/codex/blob/rust-v0.154.0/docs/contributing.md)). 패치의 기능은 Codex 사용자에게도 필요할 때만 그곳에 제안하고, CodeSpace를 위한 API가 아니라 그 사용자들의 문제로 설명합니다. CodeSpace에만 필요한 기능은 요청 없이 패치로 유지합니다([첫 패치](#case-pre-reap)). 요청을 제출할지는 소유자가 정합니다. 업스트림 상태는 기록에 남기고, Codex 릴리스가 같은 기능을 제공하고 CodeSpace가 그 릴리스로 옮기면 패치를 제거합니다.

<a id="patch-pin-changes"></a>

### 버전 변경

버전 갱신은 패치를 되돌리고(`python3 scripts/codex_patch.py revert`) 서브모듈을 옮깁니다. 같은 PR에서 각 패치를 후보 버전으로 옮기거나 제거 조건에 따라 빼고, 패치의 테스트와 영향받는 모든 어댑터를 Linux와 macOS에서 다시 실행하고, 기록을 갱신합니다. 패치 때문에 버전이 저절로 바뀌는 일은 없습니다.

<a id="patch-approval"></a>

### 승인

패치를 더하거나 바꾸거나 빼는 일은 버전 변경과 같이 검토를 거친 CodeSpace PR로 하며, 소유자의 승인을 받아 정확한 헤드로 병합합니다.

<a id="earlier-cases"></a>

## 이전 사례

2026년 9월 처음 버전을 고정한 때부터 2026년 10월 PTY 관찰 패치까지, 고정한 릴리스에는 어댑터에 필요한 것이 빠져 있는 경우가 많았습니다. PTY 관찰 패치 전까지는 모든 사례를 Codex 소스를 바꾸지 않고, 버전도 옮기지 않고 해결했습니다. 사례는 여섯 갈래로 나뉩니다. 각 링크는 그 결정을 기록한 issue나 PR입니다.

<a id="case-taking-code"></a>

### Codex 코드를 가져오는 방식

- **문제.** `codex-apply-patch`는 `codex-exec-server`, 경로 도구, tree-sitter 같은 Codex workspace의 다른 crate에 의존하므로 그것만 떼어 복사할 수 없습니다.
- **검토한 것.** crate 하나를 복사하는 방식은 처음부터 범위 밖이었습니다([#1](https://github.com/novelKR/CodeSpace/issues/1)). [#25](https://github.com/novelKR/CodeSpace/pull/25)는 Codex의 독립 실행 파일 `apply_patch`를 감싸는 방식, 패치 형식을 다시 구현하는 방식, `git apply`로 대신하는 방식도 배제했습니다.
- **결정.** `rust-v0.154.0`의 `third_party/codex` 서브모듈([#25](https://github.com/novelKR/CodeSpace/pull/25)). 고정 버전 검사는 다른 커밋을 거부하며, `git submodule update --remote`는 배포 단계로 쓰지 않습니다.
- **이후 릴리스.** `rust-v0.157.1`, `rust-v0.159.0-alpha.11`, 정식 `rust-v0.159.0`부터 `rust-v0.159.2`까지는 Codex의 descriptor 정책이나 `ChildFds`처럼 CodeSpace가 쓸 만한 기능이 있었습니다. 어느 것도 다른 작업의 일부로 들이지 않았습니다([#76](https://github.com/novelKR/CodeSpace/issues/76)). 2026-09-30 소유자의 계획 방향은 별도의 검증된 버전 PR이 CodeSpace를 검토한 `rust-v0.159.x`로 옮길 때까지 `rust-v0.154.0`을 유지하며, PTY 어댑터의 `ChildFds` 수정은 그 PR에 함께 넣습니다([DevGuard #14](https://github.com/novelKR/DevGuard/issues/14#issuecomment-5914489840)).

<a id="case-workspace"></a>

### Codex workspace 안에서 빌드하기

- **문제.** 고정한 crate는 Codex 자체 Cargo workspace를 전제로 합니다. 공유 의존성 버전과, `tungstenite`·`tokio-tungstenite`를 crates.io 릴리스에 없는 기능과 함께 OpenAI의 fork에서 빌드하게 하는 `[patch]` 항목이 그 예입니다. Cargo는 빌드의 루트 manifest에 있는 `[patch]`만 적용합니다.
- **결정.** 어댑터마다 독립된 Cargo workspace와 잠금 파일을 두고, 루트 workspace는 이를 제외합니다([#25](https://github.com/novelKR/CodeSpace/pull/25)). Codex에 닿는 workspace는 필요한 `[patch]` 항목을 Codex manifest에서 그대로 가져오고, 이 항목은 버전과 함께 바뀌고 함께 되돌립니다([#25](https://github.com/novelKR/CodeSpace/pull/25), [#51](https://github.com/novelKR/CodeSpace/pull/51)).
- **Rama.** Codex는 `rama` crate를 `=0.3.0-alpha.4`로 고정하지만, 그 crate들은 하위 crate를 `^0.3.0-alpha.4`로 받아들입니다. 그래서 새로 의존성을 풀면 정식 0.3.0 하위 crate가 섞여 빌드가 깨졌습니다. 파일 시스템 어댑터와 Linux 샌드박스 어댑터는 `rama-error`, `rama-macros`, `rama-utils`를 정확한 버전으로 고정하고, CI는 `--locked`로 빌드합니다([#53](https://github.com/novelKR/CodeSpace/pull/53), [#54](https://github.com/novelKR/CodeSpace/pull/54)).
- **잠금 파일.** 러너의 의존성이 바뀌면 워커의 잠금 파일에도 반영해야 합니다([#53](https://github.com/novelKR/CodeSpace/pull/53), [#54](https://github.com/novelKR/CodeSpace/pull/54)). glob이 서브모듈 안까지 들어가 CI가 깨진 뒤로 CI 캐시 키는 잠금 파일을 하나씩 명시하며([#67](https://github.com/novelKR/CodeSpace/pull/67)), 테스트 빌드는 오래된 잠금 파일을 더 이상 다시 쓰지 않습니다([#69](https://github.com/novelKR/CodeSpace/pull/69)).

<a id="case-graph"></a>

### Codex 의존성 그래프를 핵심 crate 밖에 두기

- **패치 엔진.** `tokio-tungstenite` 기능 충돌 때문에 게이트웨이가 루트 workspace에서 `codex-apply-patch`를 링크할 수 없었습니다. 그래서 엔진은 stdin으로 JSON을 받고 stdout으로 답하는 `codespace-patch` 도우미 프로세스에서 실행합니다([#27](https://github.com/novelKR/CodeSpace/pull/27)).
- **파일 시스템.** 링크를 따라가지 않는 파일 입출력(`LOCAL_FS`)은 `codex-exec-server`에서 오며, 이 때문에 `codex-protocol`을 비롯한 Codex crate가 루트 잠금 파일에 들어옵니다. 공개 타입은 CodeSpace 것으로 두고, 핵심 crate는 Codex에 직접 의존하지 않으며, 의존성 검사는 모든 제품 그래프에서 `codex-core`, `codex-exec`, `codex-app-server`, `codex-login`을 거부합니다([#51](https://github.com/novelKR/CodeSpace/pull/51), [#57](https://github.com/novelKR/CodeSpace/pull/57)).
- **Linux 샌드박스.** `codex-linux-sandbox`는 bubblewrap 안에서 자기 실행 파일을 다시 실행합니다. 이를 실행 파일 전용 도우미로 감싸고, 러너는 작은 프로토콜 crate에만 의존하며, 의존성 검사가 도우미의 그래프를 러너 밖에 둡니다([#53](https://github.com/novelKR/CodeSpace/pull/53), [#54](https://github.com/novelKR/CodeSpace/pull/54), [#57](https://github.com/novelKR/CodeSpace/pull/57)).

<a id="case-defaults"></a>

### CodeSpace가 바꿔 쓰는 업스트림 기본값

- Codex의 workspace-write 프로필은 `.git`, `.agents`, `.codex`를 읽기 전용으로 두고, 기본 `NO_PROXY`는 loopback 주소로 가는 트래픽이 프록시를 거치지 않게 합니다. CodeSpace의 작업 공간은 전체가 쓰기 가능하며, loopback 트래픽도 CodeSpace의 프록시를 거칩니다([#53](https://github.com/novelKR/CodeSpace/pull/53), [#55](https://github.com/novelKR/CodeSpace/pull/55)).
- `codex-uds`는 이미 있는 소켓 디렉터리의 권한을 0700으로 바꿉니다. CodeSpace 게이트웨이는 대신 새 0700 디렉터리를 만들고, 워커는 그 안에서 bind만 합니다([#46](https://github.com/novelKR/CodeSpace/pull/46)).
- PTY 크기는 업스트림 기본값에 기대지 않고 24×80으로 명시합니다([#49](https://github.com/novelKR/CodeSpace/pull/49)).

<a id="case-missing"></a>

### 고정 버전에 없는 기능

모두 CodeSpace 안에서 메우거나, 문서화하고 테스트로 고정했습니다. 버전을 옮긴 경우는 없습니다.

- **파일 권한.** Codex의 파일 시스템 인터페이스에는 chmod가 없고, 링크를 따라가지 않는 `chmodat`은 Linux에서 실패했으며, `/`부터 내려가는 Codex의 경로 탐색은 `/var`가 심볼릭 링크인 macOS에서 실패합니다. CodeSpace는 링크를 따라가지 않고 파일을 연 뒤 열린 descriptor로 권한을 바꾸고, 작업 공간 루트를 정규화합니다. 경로 기반 chmod로 대신하는 방식은 링크를 따라가지 않는다는 보장을 약하게 하므로 배제했습니다([#25](https://github.com/novelKR/CodeSpace/pull/25), [#51](https://github.com/novelKR/CodeSpace/pull/51), [#52](https://github.com/novelKR/CodeSpace/pull/52)).
- **PTY의 신호 종료.** 고정한 PTY는 신호로 끝난 프로세스를 종료 코드 1로 보고합니다. pipe 경로는 이제 신호를 보고하고, PTY의 동작은 문서화해 테스트로 고정했습니다([#85](https://github.com/novelKR/CodeSpace/issues/85), [#87](https://github.com/novelKR/CodeSpace/pull/87)).
- **macOS의 descriptor 상속.** 동시에 실행한 spawn이 서로의 descriptor를 넘길 수 있었고, Codex 자체 descriptor 정책은 이후 릴리스에만 있습니다. CodeSpace는 자기 spawn에서 상속된 descriptor에 close-on-exec를 표시합니다. 버전 변경은 대안 가운데 하나였지만 고르지 않았습니다([#79](https://github.com/novelKR/CodeSpace/issues/79), [#81](https://github.com/novelKR/CodeSpace/pull/81)).
- **macOS의 fork 안전성.** fork한 자식이 libnotify 안에서 중단될 수 있었고, PTY의 fork는 고정한 코드에 속합니다. CodeSpace는 fork 전에 libnotify 초기화를 끝내며, 이는 구조적 해결이 아닌 완화책으로 기록했습니다([#86](https://github.com/novelKR/CodeSpace/pull/86)).
- **관리형 실행의 descriptor.** 고정한 PTY는 이미 상속 가능한 descriptor만 넘기며, `ChildFds::Attached`는 이후 릴리스가 필요합니다. CodeSpace는 실행용 descriptor를 그 spawn 동안에만 상속 가능하게 하고, 그동안 다른 spawn은 이를 제외합니다([#90](https://github.com/novelKR/CodeSpace/pull/90)).
- **툴체인.** 루트 manifest는 Rust 1.88을 선언하지만 고정한 체크아웃은 1.95.0을 고릅니다. [운영 가이드](operations.md)는 1.88이 검증된 최소 버전이 아니라고 밝힙니다([#56](https://github.com/novelKR/CodeSpace/pull/56)).

<a id="case-pre-reap"></a>

### 회수 전 관찰: 첫 패치

- **문제.** 고정한 `codex-utils-pty`는 자기 task에서 자식을 회수하며 회수 전 hook이 없습니다. 그래서 관리형 PTY 실행은 루트가 회수되기 전에 관찰할 수 없습니다([DevGuard 연동](devguard-integration.md#devguard-lifecycle)).
- **검토한 것**(2026-10-11, [DevGuard #14](https://github.com/novelKR/DevGuard/issues/14#issuecomment-6110596020)): 문서화된 제약으로 두기, Codex의 공개 `spawn_from_driver` 위에 CodeSpace 자체 PTY를 두기, Codex에 회수 전 hook을 넣기. [업스트림 어댑터 명세](https://github.com/novelKR/DevGuard/blob/668750b4ab6ad582904c3d05ffceffa062b26243/docs/handoff/2026-09-28-cs-dg-upstream-adapter-work-spec-1.md#8-preferred-investigation-b-an-upstream-owned-bounded-pre-reap-observation-contract)는 업스트림이 소유하고 시간 제한이 있는 회수 전 관찰을 우선하며, CodeSpace 자체 PTY는 CodeSpace가 Codex에 맡긴 메커니즘을 CodeSpace 안으로 옮기는 일입니다.
- **결정.** 소유자는 hook을 골랐고, 수명 주기 작업과 병행했습니다. [#92](https://github.com/novelKR/CodeSpace/pull/92)는 PTY를 회수 뒤에 관찰하는 상태로 병합했고, hook은 별도 변경으로 준비했습니다. 관찰 기능은 먼저 Codex 사전 릴리스에서 만들어 시험했고([DevGuard W3 결정 자료](https://github.com/novelKR/DevGuard/blob/668750b4ab6ad582904c3d05ffceffa062b26243/docs/handoff/2026-09-28-w3-decision-packet.md#32-experiment-b-backend-owned-pre-reap-observation-packet-132) 3.2절), 그 뒤 `rust-v0.154.0`으로 옮겼습니다. 이어서 후속 변경이 이를 관리 대상 PTY 실행에 연결했습니다([실행 뒤 생명주기](devguard-integration.md#devguard-lifecycle)).
- **형식.** 서브모듈이 가리킬 커밋을 담은 fork, 또는 공식 릴리스 위의 패치 파일. 어느 쪽이든 버전을 옮길 때 다시 옮겨야 하며, `rust-v0.159.x`에서는 이 PTY 코드가 바뀝니다. 소유자는 패치 파일을 골랐습니다. 서브모듈은 공식 릴리스에 남고, 변경과 그 기록, 이후의 이식은 모두 CodeSpace에서 검토하며, 따로 최신으로 유지할 저장소가 생기지 않습니다. 이 문서는 그 선택을 규칙으로 정한 것입니다.
- **업스트림.** 이 기능을 2026-10-12 소유자 승인으로 [openai/codex#53298](https://github.com/openai/codex/issues/53298)에 제출했습니다. 2026-10-14 소유자가 not planned로 닫았습니다. DevGuard 연동에만 필요한 기능을 Codex 내부 크레이트의 새 API로 요청한 것이었고, Codex 사용자에게는 필요하지 않았습니다. 그래서 [업스트림 우선](#patch-upstream)은 Codex 사용자에게도 필요한 기능만 제안합니다.

<a id="case-patterns"></a>

### 사례에서 드러난 원칙

- Codex 코드는 릴리스 태그의 서브모듈에서 가져오며, crate를 복사하지 않습니다.
- 어댑터는 독립된 workspace에 두고, Codex workspace의 설정인 `[patch]` 항목과 정확한 버전을 그대로 따르며, 잠금 파일대로 빌드합니다.
- 핵심 crate가 지면 안 되는 Codex 의존성 그래프는 도우미 프로세스와 의존성 검사 뒤에 둡니다.
- 업스트림 기본값이 CodeSpace의 정책을 약하게 하는 곳에서는 CodeSpace의 정책을 따릅니다.
- 고정 버전에 없는 기능은 CodeSpace에서 메우거나, 문서화하고 테스트로 고정합니다. 버전 이동은 언제나 그 자체로 별도의 결정이며 다른 작업의 일부가 아닙니다.
- Codex 코드는 어댑터로도 버전 이동으로도 해결되지 않고 CodeSpace가 그 메커니즘을 넘겨받아서도 안 될 때만 바꾸며, 그때도 공식 릴리스 위의 기록된 패치로만 바꿉니다.

<a id="case-open-points"></a>

### 남은 점

사례를 모으면서 아직 해결되지 않은 세 가지가 드러났습니다.

- `crates/patch`에는 Rama 버전 고정이 없어, 파일 시스템 어댑터나 Linux 샌드박스 어댑터와 달리 잠금 파일에만 기댑니다.
- 패치 도우미의 이유로 기록된 충돌([#27](https://github.com/novelKR/CodeSpace/pull/27))은 루트가 Codex의 `[patch]` 항목을 가져오기([#51](https://github.com/novelKR/CodeSpace/pull/51)) 전의 일입니다. 지금 루트 workspace가 `codex-apply-patch`를 링크할 수 있는지는 확인되지 않았습니다.
- 기록은 `rust-v0.154.0`을 후보 버전으로만 적고 고른 이유는 적지 않았습니다. [업데이트 절차](upstream-update.md)는 그 이유를 기록하도록 요구합니다.

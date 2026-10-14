<a id="cs-rg-qualification-report"></a>

# CS-RG qualification 보고서

[English](../cs-rg-qualification-report.md) | [한국어](cs-rg-qualification-report.md)

이 문서는 CSRG-U6 단위가 [CS-RG qualification 절차](cs-rg-qualification.md)에 따라 CodeSpace의 DevGuard 사용(CS-RG)을 qualification한 결과를 보고합니다. 실행이 무엇을 보여야 하는지는 절차가 정하고, 이 문서는 고정한 후보에서 한 실행이 실제로 무엇을 보였는지와 그 증거가 어디 있는지를 기록합니다. 횟수, 시간, 백분위수는 한 호스트에서 이 실행들이 관찰한 값이며, 실패 확률이 아닙니다.

**결과:** CS-RG는 아래 [조합](#report-combination)으로 macOS에서 qualification을 마쳤습니다. 대상은 `in-process`와 `--runtime-bin`을 쓴 `uds` runner 모드의 `required` 참여(각각 pipe와 PTY 실행)와, 한 gateway 안의 `off`와 `required` workspace입니다. 다른 모드는 아래 [주장](#report-claims)대로 지원하지 않거나 기존 동작을 유지합니다. Linux `required`는 DG-LINUX 전까지 지원하지 않습니다. 운영자가 qualification한 조합을 도입하는 방법과 되돌리는 방법은 [qualification과 도입](devguard-integration.md#devguard-qualification)에 있습니다.

<a id="report-claims"></a>

## 주장하는 것

작업 지시서는 결과를 네 수준으로 나눕니다. 여기서 **구현**은 CodeSpace main 브랜치에 병합되었다는 뜻입니다. **기능 검증**은 fake, 스크립트로 흉내 낸 worker, DevGuard fixture authority를 쓴 시험으로 회귀 단계나 CI에서 보였다는 뜻입니다. **플랫폼 검증**은 절차의 스위트가 macOS에서 DevGuard 자신의 구성 요소를 상대로 보였다는 뜻입니다. **qualification**은 정확한 조합이 그 주장에 절차가 요구하는 모든 스위트를 통과하고, 봉인한 증거를 확인하고 검토했다는 뜻입니다.

| 모드나 동작 | 수준 | 근거 |
| --- | --- | --- |
| macOS, `in-process`, pipe와 PTY, `required` | qualification | [기능 행렬](#report-matrix), [장애 사례](#report-faults), [authority 장애](#report-authority), [제어 경로](#report-control), [회귀](#report-regressions) |
| macOS, `--runtime-bin`을 쓴 `uds`, pipe와 PTY, `required` | qualification | 위와 같음 |
| macOS, 한 gateway 안의 `off`와 `required` workspace, pipe와 PTY | qualification | 두 runner 모드의 matrix `mixed` |
| macOS, `--runner-socket`을 쓴 `uds`, `required` | 지원하지 않음. 거부는 플랫폼 검증 | matrix `runner-socket` |
| macOS, `--devguard status`에서의 `required` | 지원하지 않음. 거부는 플랫폼 검증 | matrix `status-mode` |
| macOS, 쓸 수 있는 launch helper가 없는 `required` | 지원하지 않음. 거부는 플랫폼 검증 | matrix `no-helper`, `unusable-helper` |
| macOS와 Linux, `off` | 기존 동작, 기능 검증 | 후보의 회귀 단계와 CI |
| Linux, `required` | DG-LINUX 전까지 지원하지 않음. 거부는 기능 검증 | 후보의 Linux CI |
| 등록 뒤 요청에 대한 응답 유실, launch 도중 worker 유실 | 기능 검증 | DevGuard client 시험, 스크립트 worker를 쓴 CodeSpace runner와 gateway 시험 |
| runner wire에서 준비나 launch 메시지 재전송 | 시험하지 않음. 알려진 공백 | |
| journal이나 저장소 거부 | 주입하지 않음. 기능 검증 | DevGuard 자체 스위트, CodeSpace의 코드 대응 시험 |
| DG-1 warm 부하에서의 제어 | 측정하지 않음 | qualification은 cold 부하를 씀 |
| 전경 브라우저 응답성 | DG-1의 것이며 여기서 측정하지 않음 | 그 release에 대한 DevGuard의 DG-1 qualification |

<a id="report-combination"></a>

## qualification한 조합

`candidate` 스위트가 2026-10-13 16:23 UTC에 기록했고, code tree hash를 다시 계산해 commit과 따로 대조했습니다.

| 구성 요소 | 값 |
| --- | --- |
| CodeSpace | [#98](https://github.com/novelKR/CodeSpace/pull/98)의 병합 commit `ca306eeaec42818c5fdb2a6ca3b6765507e8af8c`, 커밋하지 않은 변경 없음. code tree SHA-256 `32235f28b930ec1487f6ca508ff6b0c7f77772275462080972c3cfe1d2b4245f`. 이 보고서를 추가하는 commit은 문서만 바꾸므로 같은 code tree를 유지함 |
| DevGuard client | `novelKR/DevGuard`의 `6e7e065a0dd5b3876dc7f4546ab76069489937d1`에서 가져온 `devguard-client` 0.1.0과 `devguard-contract` 0.1.0([client pin](upstream-lock.md#devguard-client-pin)) |
| Codex | gitlink `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`(`rust-v0.154.0`)에 [기록된 패치](upstream-lock.md#codex-downstream-patch)를 적용한 tree `1999937194e3a68d982be85679557f0e50fe5408`. `codex_patch.py check` 통과 |
| wire와 protocol | CodeSpace runner wire 7, DevGuard protocol 1, 하네스가 쓰는 MCP `2025-11-25` |
| 바이너리 | `codespace-mcp` 0.6.0(`-p codespace-server --features devguard`, release) `1e091e02b648c24bb933d90b1347ecc22119ba0745d401c1f6482364c6500480`, 38,849,664바이트. `codespace-codex-runtime`(`--features devguard`, release) `a1aebb98919a4a8c67783b0590f1016d6472095a0f72bda01645e3ca03efddaa`, 26,867,360바이트. fixture 스위트용으로 client pin에서 빌드한 `fixture_authority` `ea234acf2a3ddf645d4e5a3ad8ab163cfa81df003d323070ac754c035b3ff124`와 `devguard-launch` `85c2165e95e19ca52e8a6c213b3efa4f48184de335a828b5409b737dce0e5bf0`, 부하용 `devguard-qualify` `694c838621b14762dc2ad4af5139a48041764fe44fd62bed005a501d6eb68cc1` |
| DevGuard release | `6e7e065`에서 빌드한 `0.1.0-6e7e065-94d85365`. `MANIFEST.json` SHA-256 `4535b52cc6466e892ebee2a2ad52f396e6b7520d7560e7e25c20ffa3647ed0df`. 설치된 `devguardd`, `devguard`, `devguard-launch`가 모두 manifest와 맞고, 서비스는 정상이며 이 release를 실행함 |
| 정책 | 구성 지문 `8cbb019bbb58e92e2a0d65f797cd5eb5f1696ce9e254bfdb1af97395382efc6f`. `devguard doctor --require admission,registration,macos-cooperative` 만족. capability `durable_admission`, `fenced_launch`, `per_resource_evidence`, `static_control_reservations`, `macos_cooperative`. `codespace` 소비자는 role `control_service`, generation `codespace-g1`, 인스턴스 하나, control 예약 150 mCPU, 536,870,912바이트, 40 tasks. `host.toml`의 SHA-256은 봉인한 기록에 있음 |
| DG-1 qualification | 이 release에 대한 DevGuard의 기록, SHA-256 `fd4fed318e0fbc640d9e6174a8dd731db80f9607d2235bdfa773c4101f6b17a4`. 2026-10-13에 같은 `host.toml`, 지문, capability에서 cold와 warm 부하 모두 qualification을 마치고 promote함([DevGuard #34](https://github.com/novelKR/DevGuard/pull/34)) |
| 도구 | `rustc 1.95.0 (59807616e 2026-04-14)`, `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`, Python 3.9.6, Xcode 27.0(27A266a) |
| 호스트 | Mac mini(Macmini9,1, Apple M1), 논리 CPU 8개, 16 GiB, AC 전원, macOS 27.0(26A428). 소비자 예약을 뺀 work capacity 5,350 mCPU, 12,079,595,520바이트, 104 tasks |

<a id="report-runs"></a>

## 실행

모든 실행은 후보의 바이너리를 `PATH`에서 Rust 1.95.0이 먼저 잡히는 최소 환경에서 썼고, 자원을 차지한 작업과 활성 `codespace` 인스턴스가 없으며 메모리 pressure가 정상인지 확인한 뒤 시작했습니다. 각 실행은 봉인했고, `verify`가 원시 파일에서 판정을 다시 계산해 바뀌지 않았음을 확인했습니다.

| 실행 | 시각(UTC) | 결과 |
| --- | --- | --- |
| `candidate` | 2026-10-13 16:23 | 통과 |
| `matrix` | 2026-10-13 16:40–16:42 | 통과: 시나리오 45개 중 45개 |
| `faults` | 2026-10-13 16:42–16:45 | 통과: 사례 27개 중 27개 |
| `authority` | 2026-10-13 16:45–16:49 | 통과: 단계 10개 중 10개 |
| `control --rehearsal` | 2026-10-13 16:50–16:53 | 판정 불가. 예행이 절차보다 짧다는 사유뿐이며, 설계대로임 |
| 회귀 단계, `all` | 2026-10-13 17:00–17:53 | Linux가 필요한 `linux-isolation` 말고 모든 단계 통과 |
| 회귀 단계, `macos-core`와 `macos-fork-race` | 2026-10-13 17:57–18:01 | 통과 |
| `control --rehearsal` | ⟨CTRL-REH-WHEN⟩ | ⟨CTRL-REH-RESULT⟩ |
| `control` | ⟨CTRL-WHEN⟩ | ⟨CTRL-RESULT⟩ |
| Linux CI | 2026-10-13 23:25–23:42 | 통과: 전체 프로필의 모든 작업([실행 38484155990](https://github.com/novelKR/CodeSpace/actions/runs/38484155990)) |

<a id="report-matrix"></a>

## 기능 행렬

`matrix`는 설치된 서비스를 상대로 두 runner 모드에서 실행했습니다. `in-process` 시나리오 22개와 `uds` 시나리오 23개가 모두 통과했습니다.

| 시나리오 | `in-process` | `uds` |
| --- | --- | --- |
| registration | 통과 | 통과 |
| exits, io, termination, timeout, exec-failed, shortage, unsupported-minimum, mixed | pipe와 PTY 모두 통과 | pipe와 PTY 모두 통과 |
| no-helper, unusable-helper, status-mode, authority-unavailable | 통과 | 통과 |
| runner-socket | | 통과 |
| settled | 통과 | 통과 |

모든 관리 대상 실행이 DevGuard journal과 맞았습니다. 예약하고 적용한 양은 workspace 요청과 같고, CPU·메모리·tasks가 각각 `applied`였고, scope root는 실행한 프로세스였으며, attempt는 아무것도 붙잡지 않은 채 `scope_terminated`로 해제되었고 추적은 끝까지 온전했습니다. 거부된 호출의 attempt는 모두 기대한 단계에서 아무것도 붙잡지 않았고, 거부가 먼저 오는 경우에는 attempt가 없었습니다.

<a id="report-faults"></a>

## 장애 사례

`faults`는 사례마다 새로 띄운 DevGuard fixture authority를 상대로 두 runner 모드에서 실행했습니다. `in-process` 사례 13개와 `uds` 사례 14개가 모두 통과했습니다.

| 사례 | `in-process` | `uds` |
| --- | --- | --- |
| descendant, escape, stalled-observer | pipe와 PTY 모두 통과 | pipe와 PTY 모두 통과 |
| authority-lost, authority-restart, authority-stalled, helper-ends, credential, terminate-race, shutdown | 통과 | 통과 |
| worker-killed | | 통과 |

각 사례가 무엇을 보이는지는 절차의 [장애 사례](cs-rg-qualification.md#fault-cases)에 있습니다. 함께 보면 재실행도, 이른 해제도 없다는 것을 보입니다. 자손이 남으면 끝날 때까지 attempt가 자원을 차지한 채 남고, 빠져나갔거나 관찰하지 못한 자손이 있으면 Suspect로 자원을 차지한 채 남으며, 불확실하거나 재시작 전의 attempt는 다시 실행하지 않고, 아무것도 시작하지 않았음이 증명된 호출은 아무것도 charge하지 않습니다.

<a id="report-authority"></a>

## authority 장애

`authority`는 fixture authority를 상대로 두 runner 모드에서 다섯 단계를 실행했고, 열 개 모두 통과했습니다. 이미 실행 중인 관리 대상의 프로세스 status, I/O echo, 종료는 모든 단계에서 동작했습니다. 새 관리 대상 실행은 authority가 답할 수 있을 때 허용되고, 답할 수 없을 때 거부되었습니다.

| 단계 | 새 관리 대상 실행 | 종료 대상이 허용된 시점(`in-process` / `uds`) |
| --- | --- | --- |
| baseline | 허용 | 0.09초 / 0.09초 |
| paused | 허용 | 0.09초 / 0.09초 |
| stalled | 거부 | 0.09초 / 0.10초, hold 전 |
| restarted | 허용 | 36번 거부 뒤 36.61초 / 34번 거부 뒤 34.61초 |
| stopped | 거부 | 0.12초 / 0.16초, kill 전 |

`stalled`가 잠금을 잡고 있는 동안 오래된 호스트 표본 때문에 DevGuard는 표본이 30초 동안 정상일 때까지 새 작업을 닫아 두었습니다. 그래서 `restarted`의 대상은 `RESOURCE_UNAVAILABLE`로 거부되었고, 절차가 정한 90초 안에서 1초마다 다시 시도했습니다.

<a id="report-control"></a>

## 제어 경로 가용성

⟨CTRL-SECTION⟩

<a id="report-accounting"></a>

## 자원 회계

작업 지시서의 회계 항목을 CodeSpace의 관점이 아니라 DevGuard의 기록으로 확인했습니다.

| 항목 | 근거 | 결과 |
| --- | --- | --- |
| 요청, 예약, 지원, 적용 값 | [기능 행렬](#report-matrix): 모든 관리 대상 실행에 대한 DevGuard journal 기록 | 두 runner 모드, pipe와 PTY 모두 일치 |
| headroom과 control 예약 | 소비자 예약은 work capacity에서 빠짐. 소비자를 준비할 때 5,500에서 5,350 mCPU로, 12,616,466,432에서 12,079,595,520바이트로, 144에서 104 tasks로 줄었음. matrix `shortage`는 들어가지 않는 요청을 거부함. [제어 경로](#report-control)는 실행 소유자의 사용량을 예약과 비교함 | ⟨ACCT-FOOTPRINT⟩ |
| pressure | [authority 장애](#report-authority): 오래된 호스트 표본 뒤에는 표본이 30초 동안 정상일 때까지 새 작업이 닫히고, 실행 중인 대상은 계속 제어됨. [제어 경로](#report-control): DG-1 부하에서의 호스트 pressure. 서비스 자체의 pressure 동작은 DG-1이 보임 | ⟨ACCT-PRESSURE⟩ |
| 자손이 남아 있으면 root 종료만으로 해제하지 않음 | faults `descendant`, pipe와 PTY | 통과 |
| 알 수 없는 상태는 보수적으로 회계 | faults `escape`, `stalled-observer`, `authority-restart`: attempt가 Suspect로 자원을 차지한 채 남음 | 통과 |
| 불확실한 launch 뒤 재실행 없음 | faults `authority-stalled`, `authority-restart`(launch 한 번), `worker-killed`, 제어 경로의 중복 실행 수 | 통과. ⟨ACCT-DUPLICATES⟩ |
| 시작하지 않았음이 확실한 경우의 정리 | matrix `no-helper`와 `unusable-helper`(cancelled), `shortage`(denied), `exec-failed`(scope가 끝나면 해제). faults `helper-ends`(`no_helper_created`로 해제), `authority-stalled`와 `worker-killed`(붙잡은 것 없음) | 통과 |

<a id="report-regressions"></a>

## 회귀와 동등성

회귀 단계는 후보에서, qualification 호스트의 최소 환경으로, DevGuard 시험 바이너리를 반드시 쓰게 해서 실행했습니다.

| 단계 | 초 | 결과 |
| --- | ---: | --- |
| `pin` | 0.67 | 통과 |
| `policy` | 0.13 | 통과 |
| `python` | 8.91 | 통과 |
| `format` | 0.93 | 통과 |
| `clippy-root` | 192.45 | 통과 |
| `clippy-adapters` | 235.36 | 통과 |
| `clippy-codex` | 149.78 | 통과 |
| `integration` | 2,196.63 | 통과 |
| `dependencies` | 5.19 | 통과 |
| `unit-patch` | 21.76 | 통과 |
| `unit-codex-runtime` | 31.37 | 통과 |
| `unit-pty` | 7.94 | 통과 |
| `unit-file-system` | 307.08 | 통과 |
| `unit-linux-sandbox` | 14.34 | 통과 |
| `unit-devguard` | 11.32 | 통과 |
| `unit-linux-sandbox-protocol` | 4.72 | 통과 |
| `linux-isolation` | | macOS에서는 실행하지 않음. Linux CI |
| `macos-core` | 75.28 | 통과 |
| `macos-fork-race` | 129.40 | 통과: 시도 400회, 실패 0, 자식 72,946개 모두 exit code 있음, 새 crash report 없음 |

`all`은 시험 782개 통과, 실패 0, 무시 9였고, macOS 전용 단계는 189개 통과, 실패 0, 무시 9였습니다. adapter의 알려진 간헐 실패("another authority owns this state directory")는 나타나지 않았습니다. 이 단계들은 작업 지시서가 꼽은 모든 영역의 기존 시험을 담고 있습니다. workspace 권한, 승인, pipe와 PTY 실행, stdin과 출력, resize, timeout, 종료, shutdown, patch helper, sandbox helper, UDS worker, feature를 끈 동작이며, 마지막은 `dependencies` 단계와 `devguard` feature 없는 빌드가 다룹니다. `macos-core`는 macOS에서만 도는, DevGuard fixture authority와 launch helper를 상대로 한 gateway 시험도 실행합니다.

Linux CI는 예약된 [실행 38484155990](https://github.com/novelKR/CodeSpace/actions/runs/38484155990)에서 2026-10-13 23:25부터 23:42 UTC까지 후보에 전체 프로필을 실행했고, 모든 작업이 통과했습니다. 형식과 pin 검사, 정책 검사, Clippy 빌드 세 개와 단위 단계 일곱 개, Ubuntu의 `integration`과 `linux-isolation`, 호스팅된 macOS runner의 `macos-core`, `macos-fork-race`, `dependencies`입니다. Linux에서는 DevGuard에 실제 호스트 증거가 없으므로 소유자가 등록할 수 없습니다. adapter의 단위 시험은 등록이 `incompatible`인지 확인하고, `integration`의 gateway 시험은 관리 대상 실행이 `RESOURCE_AUTHORITY_UNAVAILABLE`로 거부되고 아무것도 시작되지 않는지 확인합니다. 이것이 위 [주장](#report-claims)에서 기능 검증이라고 한 Linux `required`의 거부입니다.

<a id="report-amendments"></a>

## 수정 기록

- **회귀 명령(절차 수정).** 절차는 `validate-upstream.py all macos-core macos-fork-race`라는 명령 하나를 적었습니다. 그런데 `all`은 다른 단계 이름을 모두 대신하고 macOS 전용 단계를 빼므로([`scripts/validate-upstream.py`](../../scripts/validate-upstream.py)), 그 명령은 `macos-core`도 `macos-fork-race`도 실행하지 않습니다. 두 단계는 같은 후보에서 두 번째 실행으로 돌렸고, 이제 절차는 두 번의 실행을 적습니다. 문서만 바뀌었으며 다시 하거나 바꾼 실행은 없습니다.
- **고정 전 수정.** qualification 호스트에서 `7a0a748`의 하네스([#97](https://github.com/novelKR/CodeSpace/pull/97))를 점검해 하네스 결함 세 가지를 찾았고, 병합 전에 고쳤습니다(`8ed8b08`). SIGTERM으로 죽은 관리 대상 PTY 자식의 기대 status(launch descriptor를 물려받은 자식을 고정 Codex PTY가 그렇게 보고하므로 exit code 143), 활성 인스턴스를 기다리지 않고 한 번만 읽던 settled 확인, `stalled` 뒤 DevGuard가 다시 작업을 받기 전에 `restarted`의 대상을 시작하던 `authority` 스위트입니다. 제품 결함도 하나 찾았습니다. `uds` 모드에서 gateway가 종료할 때 worker를 바로 죽였기 때문에 관리 대상 pipe 실행이 worker보다 오래 남았고, 그 scope는 명령이 스스로 끝난 약 30초 뒤에야 끝났습니다. [#98](https://github.com/novelKR/CodeSpace/pull/98)이 이를 고쳤습니다. 이제 gateway는 worker 연결을 닫고, 종료하면서 자기 프로세스를 끝내는 worker를 기다리며, 5초 뒤에도 실행 중일 때만 worker를 죽입니다. 같은 사례에서 두 scope가 바로 끝났습니다. 후보는 #98 뒤에 고정했고 모든 qualification 실행은 그 후보에서 했습니다. 실패했거나 끝나지 않은 점검은 그 뒤 통과한 점검과 함께 보존합니다.

<a id="report-evidence"></a>

## 증거

증거 디렉터리는 qualification 호스트의 DevGuard checkout 아래 `evidence/` 디렉터리에 있습니다. 각 디렉터리는 모든 파일의 SHA-256을 담은 `MANIFEST.json`으로 봉인했고, 파일은 읽기 전용입니다. credential, credential digest, 자식의 환경은 어느 디렉터리에도 없으며, 각각 검사했습니다. 하네스 실행의 디렉터리는 `MANIFEST.json`의 SHA-256이 아래와 같고 `qualify-devguard.py verify`가 받아들이면 확인된 것입니다. 회귀 디렉터리에는 `validate-upstream.py`의 보고서와 로그를 그대로 복사했습니다. 제어 실행의 콘솔 출력과 `verify` 출력은 디렉터리 옆에 남겼습니다.

| 디렉터리 | 실행 | `MANIFEST.json` SHA-256 |
| --- | --- | --- |
| `csrg-u6-candidate-2026-10-13-ca306ee` | candidate | `3e3587d1694e4108912c742b6825684ff95b00e658c6f78fbe63143e37aadc95` |
| `csrg-u6-qual-matrix-2026-10-13-ca306ee` | matrix | `faf673aa028f4d4c4646eb271e617395f2b753740eecb3501241936e2b2ad688` |
| `csrg-u6-qual-faults-2026-10-13-ca306ee` | faults | `b000a94a70c68ea74658e37e2da275fe48a1c7f8b74cd6c7e4bdbb2525ae5025` |
| `csrg-u6-qual-authority-2026-10-13-ca306ee` | authority | `c4a7fb26703939eb56627848affba8d4790e72c6337663533356ca5662b78256` |
| `csrg-u6-qual-control-rehearsal-2026-10-13-ca306ee` | 제어 예행 | `2c9c7fc08f4204e8f9c1a921459b50adc0c2ee439219ec8232124d785f0c66b1` |
| `csrg-u6-qual-regressions-2026-10-13-ca306ee` | 회귀 단계, `all` | `2825fb3ca091a5d0280376a3f4134e3d0050e0718cbc95409633fe259840e52e` |
| `csrg-u6-qual-regressions-macos-2026-10-13-ca306ee` | `macos-core`와 `macos-fork-race` | `5d160894fbf4b63fd822da63bf137bd8ab6a1d7a1aba005c256f5ba3ff59c2bd` |
| `csrg-u6-control-rehearsal-2026-10-14-ca306ee` | 제어 예행 | `ac79024e237ee496b64f524696228e171a7161ccfde7652e02b023c67c25039f` |
| `csrg-u6-control-2026-10-14-ca306ee` | 제어 | `7706aac71c2f793dbc273b5c78f888dcc2af50269c0039d132b21c236b47e9be` |

고정 전의 점검도 보존합니다.

| 디렉터리 | commit | 결과 | `MANIFEST.json` SHA-256 |
| --- | --- | --- | --- |
| `csrg-u6-harness-check-candidate-2026-10-13` | `7a0a748` | 통과 | `6b1cc6a10df9f3ca2b80472b5c0d9090a7e1a3a3517a99c1124d10b08a4d9400` |
| `csrg-u6-harness-check-matrix-2026-10-13` | `7a0a748` | 실패: 하네스 결함 두 가지 | `bf20056a7351b155f2e73388dbb22cd34d3a54721db0ed0b420842b3a2681f4a` |
| `csrg-u6-harness-check-faults-2026-10-13` | `7a0a748` | 실패: 27개 중 26개, `uds` shutdown | `e1568fcb5ccb298e2eb581e6687c0947ccf40c8dee1960bcedddb57473792c1a` |
| `csrg-u6-harness-check-authority-2026-10-13` | `7a0a748` | 끝나지 않음: `restarted` | `42f8ef5f6b20665120e059add4fd1d7fa3f4ad6f98c8a5b8588fe35f5def1290` |
| `csrg-u6-harness-check-control-2026-10-13` | `7a0a748` | 예행, 설계대로 판정 불가 | `2a8507da10ac571259e4a7f3437102dd24a63cf7d129d9a30492c80f30940681` |
| `csrg-u6-harness-check-matrix-2026-10-13-8ed8b08` | `8ed8b08` | 통과 | `64198825c697e333aaab14469224f297b59852067192afd4614c8e0c77302aa2` |
| `csrg-u6-harness-check-authority-2026-10-13-8ed8b08` | `8ed8b08` | 통과 | `ffe9332ace437ff926ea2b787daf94ed58a8a691f00e23374fbefaa7797372e4` |
| `csrg-u6-harness-check-faults-2026-10-13-b74e5c2` | `b74e5c2` | 통과 | `6057f26fb81a59f4f8ef6f8c12ff95235ae25c10b7503c7a6f2aaa5264ddae8b` |
| `csrg-u6-harness-check-matrix-2026-10-13-b74e5c2` | `b74e5c2` | 통과 | `577962c15c89e70b4debfe93c2f7513876fa592c6d15ff86bf7535a585121a76` |
| `csrg-u6-harness-check-authority-2026-10-13-b74e5c2` | `b74e5c2` | 통과 | `dad385b738361ef883928329f3ed6879f9a874ebdc32f3bdd5aed464ba6dc679` |

정책 행이 근거로 삼는 그 release의 DG-1 측정도 같은 곳에 봉인되어 있고, DevGuard에 기록되어 있습니다.

<a id="report-outside"></a>

## CS-RG 밖에 남는 것

CS-RG는 여기서 끝납니다. 다음은 CS-RG 밖에 남으며, 이 문서는 그것을 주장하지 않습니다.

- **P1-RECOVERY.** critical path의 다음 마일스톤으로, 독립 runner가 프로세스를 유지하는 동안 gateway를 재시작하고 다시 연결하는 일입니다. 소유자의 별도 승인이 있어야만 시작합니다.
- **DG-LINUX.** Linux에서의 강제 보호입니다. 이것이 qualification을 마치기 전까지 Linux에서 `required`는 거부되고, Linux 경로는 `off`뿐입니다.
- **DG-CACHE**와 **DG-ADAPTERS.** DevGuard의 cache와 추가 adapter입니다.
- **Codex 수렴.** upstream이 hook을 제공할 때까지([openai/codex#53298](https://github.com/openai/codex/issues/53298)) 관리 대상 PTY 실행은 CodeSpace의 [기록된 Codex 패치](upstream-lock.md#codex-downstream-patch)로 reap 전에 관찰합니다. 다음 Codex pin 이동은 별도 변경이며 새 후보입니다.
- **다른 조합.** DevGuard client pin, release, 정책, 호스트가 다르거나, 코드가 바뀌거나, 도구가 다르면 새 후보이며 따로 실행해야 합니다.
- **실행이 보이지 않은 것:** 제어 경로의 DG-1 warm 부하, 전경 브라우저 응답성, runner wire의 메시지 재전송, 주입한 journal 거부, 임의 크기의 요청이나 파일에 대한 보호.

<a id="recovery-qualification-report"></a>

# Gateway 복구 qualification 보고서

[English](../recovery-qualification-report.md) | [한국어](recovery-qualification-report.md)

이 문서는 P1R-C06 단위가 [Gateway 복구 qualification 절차](recovery-qualification.md)에 따라 Gateway 복구(P1-RECOVERY)를 qualification한 결과를 보고합니다. 실행이 무엇을 보여야 하는지는 절차가 정하고, 이 문서는 고정한 후보에서 한 실행이 실제로 무엇을 보였는지와 그 증거가 어디 있는지를 기록합니다. 횟수, 시간, 백분위수는 한 호스트에서 이 실행들이 관찰한 값이며, 실패 확률이 아닙니다.

**결과:** [독립 Runner](runner-isolation.md#independent-runner)는 아래 [조합](#recovery-report-combination)으로 macOS에서 qualification을 마쳤습니다. 대상은 `--runner-lifecycle independent`를 쓰고 운영자가 Runner를 시작하는 `uds`이며, pipe와 PTY 실행, 참여 `off`와 `required`, 그리고 한 gateway 안의 두 참여입니다. 아래 [주장](#recovery-report-claims)대로 Linux `off`는 CI 기능 확인일 뿐이고, Linux `required`는 DG-LINUX 전까지 지원하지 않으며, Runner 자신이 재시작한 뒤의 프로세스 복원은 지원하지 않습니다. 같은 후보에서 CS-RG가 qualification한 모드는 CS-RG의 스위트를 다시 통과했습니다. 독립 모드는 계속 opt-in이며, 기본값으로 바꾸는 것은 별도 결정입니다. 운영자가 이 모드를 도입하고 되돌리는 방법은 [도입과 되돌리기](operations.md#independent-runner-adoption)에 있습니다.

<a id="recovery-report-claims"></a>

## 주장하는 것

수준은 [CS-RG 보고서](cs-rg-qualification-report.md#report-claims)의 것입니다. **구현**은 CodeSpace main 브랜치에 병합되었다는 뜻입니다. **기능 검증**은 fake, 스크립트로 흉내 낸 worker, DevGuard fixture authority를 쓴 시험으로 CodeSpace의 시험이나 CI에서 보였다는 뜻입니다. **플랫폼 검증**은 절차의 스위트가 macOS에서 DevGuard 자신의 구성 요소를 상대로 보였다는 뜻입니다. **qualification**은 정확한 조합이 그 주장에 절차가 요구하는 모든 스위트를 통과하고, 봉인한 증거를 확인하고 검토했다는 뜻입니다.

| 모드나 동작 | 수준 | 근거 |
| --- | --- | --- |
| macOS, `--runner-lifecycle independent`를 쓴 `uds`, pipe와 PTY, `required` | qualification | `required`로 실행한 [기능 스위트](#recovery-report-functional), [재시작 측정](#recovery-report-measurement), [회귀](#recovery-report-regressions) |
| macOS, 같음, `off` | qualification | `off`로 실행한 기능 스위트, 측정의 관리하지 않는 workspace |
| macOS, 한 gateway 안의 `off`와 `required` workspace | qualification | 모든 gateway가 관리 대상 프로세스와 관리하지 않는 프로세스를 함께 다루는 측정 |
| Linux, 같음, `off` | 기능 검증: CI 기능 시험일 뿐이며 Linux qualification이 아님(결정 D7) | 후보 commit의 Linux CI에서 실행한 CodeSpace의 독립 Runner 시험. 후보의 하네스와 코드로 Linux에서 하네스의 `off` 시나리오 34개도 통과함([#115](https://github.com/novelKR/CodeSpace/pull/115)) |
| Linux, 같음, `required` | DG-LINUX 전까지 지원하지 않음 | Linux의 모든 `required` 실행처럼 아무것도 실행하기 전에 거부. DevGuard는 Linux에 실제 호스트 증거가 없어 거기서는 실행 소유자가 등록하지 못하며, CS-RG의 [Linux CI](cs-rg-qualification-report.md#report-regressions)가 이를 보여 줌 |
| Runner 자신이 재시작한 뒤 프로세스 복원 | 지원하지 않으며 P1-RECOVERY 밖. 거부는 플랫폼 검증 | observation `runner-replaced` |
| 호스트 재시작 | 기능 검증. 실제 재시작은 qualification하지 않음 | CodeSpace 시험이 기록에 다른 boot ID를 주면 `host_rebooted`로 보고됨 |
| gateway가 Runner를 제어하지 않는 동안 같은 사용자의 다른 프로세스가 Runner를 Claim함 | 설계상 가능(결정 D6). 기록되고 fence된다는 것은 플랫폼 검증 | fence `stale-epoch` |
| gateway가 읽지 않는 동안 Runner의 256 KiB ring을 넘는 출력 | 설계상 잃음. 잃었다고 밝힌다는 것은 플랫폼 검증 | observation `ring-overflow`, 측정의 한꺼번에 쓰기 |
| Runner를 잃은 뒤의 PTY 프로세스 | 그 루트를 기록하지 않으므로 운영자가 기록을 해제하기 전에 끝냄. 플랫폼 검증 | PTY에서 실행한 observation `runner-loss` |
| runner wire에서 준비나 launch 메시지 재전송 | 시험하지 않음. 알려진 공백 | epoch fence가 이전 controller의 요청을 거부함(fence 스위트) |
| DG-1 warm 부하에서의 복구 | 측정하지 않음 | 측정은 cold 부하를 씀 |
| CS-RG가 qualification한 모드 | 이 후보에서 CS-RG의 스위트를 다시 통과함. CS-RG 자신의 qualification 조합은 그대로 [`ca306ee`](cs-rg-qualification-report.md#report-combination) | [회귀](#recovery-report-regressions) |

<a id="recovery-report-combination"></a>

## qualification한 조합

`candidate` 스위트가 2026-10-18 16:31 UTC에 기록했습니다. 그 전에 같은 commit과 바이너리로 검사 스위트 다섯 개가 통과했습니다. 예행과 측정은 이 기록이 검사를 실행한 조합과 일치할 때만 시작했고, code tree hash도 commit에서 따로 다시 계산했습니다.

| 구성 요소 | 값 |
| --- | --- |
| CodeSpace | commit `1c0eaddd81fdd17d35da673b0c8fbe750fe4ac60`([#115](https://github.com/novelKR/CodeSpace/pull/115)의 병합), 커밋하지 않은 변경 없음. code tree SHA-256 `cb18534dedbd001a1de2f42c930ee303d41791dee279a36a7e5fe1b7de808d38`. 이 보고서를 더하는 commit은 문서만 바꾸므로 이 code tree를 유지함 |
| DevGuard client | `novelKR/DevGuard`의 `6e7e065a0dd5b3876dc7f4546ab76069489937d1`에 있는 `devguard-client` 0.1.0과 `devguard-contract` 0.1.0([client pin](upstream-lock.md#devguard-client-pin)). CS-RG와 같음 |
| Codex | gitlink `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`(`rust-v0.154.0`)에 [기록된 패치](upstream-lock.md#codex-downstream-patch)를 적용, 패치한 tree `1999937194e3a68d982be85679557f0e50fe5408`. `codex_patch.py check` 통과 |
| wire와 프로토콜 | CodeSpace runner wire 11, DevGuard 프로토콜 1, 하네스가 쓰는 MCP `2025-11-25` |
| 바이너리 | `codespace-mcp` 0.6.0(`-p codespace-server --features devguard`, release) `18e6f6da6becd3be35f4a3591c7ec047d999eade561d346b2ea6ea4bb6065027`, 40,183,376 bytes. `uds` worker이자 독립 Runner인 `codespace-codex-runtime`(`--features devguard`, release) `fa3959b430fa93f7a874970dd0e4c77b274f97720d9849ba7bc6e548eeb80237`, 27,718,336 bytes. fixture 시나리오에는 둘 다 client pin으로 빌드한 `fixture_authority` `ea234acf2a3ddf645d4e5a3ad8ab163cfa81df003d323070ac754c035b3ff124`와 `devguard-launch` `85c2165e95e19ca52e8a6c213b3efa4f48184de335a828b5409b737dce0e5bf0`, 부하에는 `devguard-qualify` `694c838621b14762dc2ad4af5139a48041764fe44fd62bed005a501d6eb68cc1`. 이 셋은 CS-RG의 것임 |
| DevGuard release | `6e7e065`에서 빌드한 `0.1.0-6e7e065-94d85365`. `MANIFEST.json` SHA-256 `4535b52cc6466e892ebee2a2ad52f396e6b7520d7560e7e25c20ffa3647ed0df`. 설치된 `devguardd`, `devguard`, `devguard-launch`가 이와 일치하고, 서비스는 정상이며 CS-RG 때처럼 프로세스 ID 23023으로 이 release를 실행함 |
| 정책 | CS-RG 이후 바뀌지 않음. 구성 fingerprint `8cbb019bbb58e92e2a0d65f797cd5eb5f1696ce9e254bfdb1af97395382efc6f`. `devguard doctor --require admission,registration,macos-cooperative` 충족. capability `durable_admission`, `fenced_launch`, `per_resource_evidence`, `static_control_reservations`, `macos_cooperative`. `codespace` 소비자는 역할 `control_service`, generation `codespace-g1`, 인스턴스 하나, 제어 예약 150 mCPU, 536,870,912 bytes, 40 tasks. `host.toml`의 SHA-256은 봉인한 기록에 있음 |
| DG-1 qualification | 이 release에 대한 DevGuard의 기록, SHA-256 `fd4fed318e0fbc640d9e6174a8dd731db80f9607d2235bdfa773c4101f6b17a4`. 같은 `host.toml`, fingerprint, capability에서 cold와 warm 부하가 qualification을 통과함 |
| 도구 | `rustc 1.95.0 (59807616e 2026-04-14)`, `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`, Python 3.9.6, Xcode 27.0(27A266a) |
| 호스트 | 논리 CPU 8개와 16 GiB를 갖춘 Mac mini(Macmini9,1, Apple M1), AC 전원, macOS 27.0(26A428). 소비자 예약을 뺀 작업 용량 5,350 mCPU, 12,079,595,520 bytes, 104 tasks |

<a id="recovery-report-runs"></a>

## 실행

qualification 호스트의 모든 실행은 최소 환경에서 후보의 바이너리를 썼고, `PATH` 맨 앞에 Rust 1.95.0을 두었으며, 자원을 차지한 것이 없고 활성 `codespace` 인스턴스가 없고 메모리 pressure가 정상임을 확인한 뒤 시작했습니다. 실행은 모두 봉인했고, `verify`가 원시 파일에서 판정을 다시 계산해 바뀌지 않았음을 확인했습니다. 검사 스위트, 고정, 예행, 측정은 소유자의 터미널에서 검토한 명령 하나로 실행했고, 각 단계는 앞 단계가 깨끗할 때만 시작했으며, 터미널의 열린 파일 한도 256 아래에서 돌았습니다. CS-RG 스위트는 그 뒤 바이너리를 빌드한 세션에서 CS-RG 때처럼 실행했습니다.

| 실행 | 시각(UTC) | 결과 |
| --- | --- | --- |
| 검사 스위트 다섯 개: `recovery-lifecycle`, `recovery-store`, `recovery-fence`, `recovery-reconcile`, `recovery-observation` | 2026-10-18 16:26–16:31 | 통과: 시나리오 55개, 검사 650개, 실패 없음. 공백 두 개는 이유와 함께 실행하지 않음으로 기록 |
| `candidate` | 2026-10-18 16:31 | 통과 |
| `recovery --rehearsal` | 2026-10-18 16:31–16:42 | 예행이 절차보다 짧다는 사유 하나로만 판정 불가(설계대로). 기능 시나리오와 두 구간은 통과 |
| `recovery` | 2026-10-18 16:42–18:52 | qualification 통과: 기능 시나리오와 세 반복의 여섯 구간이 모두 통과 |
| CS-RG의 `candidate`, `matrix`, `faults`, `authority` | 2026-10-19 02:22–02:31 | 통과([회귀](#recovery-report-regressions)) |
| P1R-C06의 모든 디렉터리에 대한 `verify` | 2026-10-19 02:31 | 이전 후보의 것을 포함해 35개 모두 확인됨 |
| 후보 commit의 CI | 2026-10-18 09:18–09:31, 21:51–22:02 | 통과: 병합 뒤 push 실행과 예약된 전체 실행의 모든 작업 |

<a id="recovery-report-functional"></a>

## 기능 스위트

기능 시나리오는 후보에서 세 번 실행했습니다. 고정 전 검사 스위트 다섯 개에서, 그리고 `recovery` 예행과 `recovery` 실행의 첫 부분으로입니다. 매번 macOS에서 실행하는 시나리오 55개(`off` 34개, `required` 21개)가 같은 검사 650개로 모두 통과해 실패한 검사가 없었고, 절차가 [공백](recovery-qualification.md#recovery-gaps)으로 적은 두 시나리오, 곧 호스트 재시작과 Runner 재시작 뒤 프로세스 복원은 이유와 함께 실행하지 않음으로 기록했습니다.

| 스위트 | `off`로 | `required`로 | 검사 (`off` / `required`) |
| --- | --- | --- | ---: |
| `recovery-lifecycle` | exit-kinds와 original-timeout(각각 pipe와 PTY), runner-sigterm, invalid-settings | exit-kinds와 original-timeout(각각 pipe와 PTY), runner-sigterm | 98 / 95 |
| `recovery-store` | records(pipe와 PTY), writer-lock, schema, records-command | records(pipe와 PTY) | 49 / 28 |
| `recovery-fence` | foreign-connection(pipe와 PTY), stale-epoch, stopped-gateway, concurrent-claims, other-database, concurrent-gateways | foreign-connection(pipe와 PTY) | 58 / 30 |
| `recovery-reconcile` | running, terminal, kill-during-exec, kill-during-terminate(각각 pipe와 PTY), approval, missing-record | running, terminal, kill-during-exec, kill-during-terminate(각각 pipe와 PTY), approval, authority-outage | 89 / 110 |
| `recovery-observation` | ring-overflow와 runner-loss(각각 pipe와 PTY), retention, runner-replaced | runner-loss(pipe와 PTY) | 55 / 38 |

각 시나리오가 무엇을 보여 주는지는 절차의 [기능 스위트](recovery-qualification.md#recovery-functional)에 있습니다. `required` 시나리오는 모두 인스턴스 하나짜리 `codespace` 소비자를 따로 둔 새 fixture authority를 상대로 실행했습니다. [#114](https://github.com/novelKR/CodeSpace/pull/114)가 더한 대기도 기록했습니다. `required`의 `kill-during-terminate`에서는 `recovery-reconcile` 검사, 예행, 실행 각각의 round 16개 가운데 6개가 다음 gateway에 새 작업을 보낼 때 거부를 한 번 만났습니다. 그 gateway가 workspace를 대조하는 중이었거나 앞 프로세스를 위해 아직 workspace를 잡고 있었기 때문이며, 각 round는 두 번째 시도에서 첫 시도 뒤 85–102 ms 만에 시작했습니다. `off`의 round는 거부를 만나지 않았습니다.

<a id="recovery-report-measurement"></a>

## gateway 재시작 측정

`recovery`는 2026-10-18 16:42 UTC부터 18:52 UTC까지 전체 절차를 실행했습니다. 바로 앞의 예행(16:31–16:42)은 예행이 절차보다 짧다는 사유 하나로만 판정 불가였습니다. 기능 시나리오(16:42–16:46) 뒤에 측정은 10분 유휴 구간과 30분 이상의 부하 구간을 관리 대상으로 세 번 반복했고, 설치된 release를 상대로 DG-1의 cold 부하를 걸었습니다. 구간은 예정된 끝 뒤에 마지막 주기를, 부하는 진행 중인 실행을 마치므로 유휴 구간은 606–607초, 부하 구간은 1,837–1,850초였습니다. 여섯 구간과 세 반복이 모두 통과해 조합이 qualification을 통과했고, 실행 전체도 통과했습니다. 모든 구간은 자원을 차지한 것도 활성 `codespace` 인스턴스도 없는 상태에서 시작했고, 서비스는 모든 반복의 앞뒤에서 프로세스 ID 23023으로 그 release를 실행했습니다.

유휴 구간마다 gateway를 26번, 부하 구간마다 76번이나 77번 재시작했습니다. 계획은 25초마다 한 번이고, 절차는 그 80%를 요구합니다. gateway는 입력 닫기, SIGTERM, SIGUSR1, SIGKILL로 번갈아 끝냈고 모두 요청한 대로 끝났으며, 다음 gateway는 매번 다음 epoch로 Runner를 Claim하고 오래 실행되는 프로세스가 모두 `running`임을 확인했습니다. 요청을 받은 초는 gateway가 MCP `initialize`에 답한 때부터 요청을 받은 시간이며, 표본은 이 시간을 따릅니다. 나머지는 대부분 gateway가 없는 동안 deadline이 오게 하려고 두는 4초입니다. 시간은 밀리초입니다.

| 반복 | 구간 | 초 | 요청을 받은 초 | 재시작 (계획) | 입력 닫기, SIGTERM, SIGUSR1, SIGKILL | 시작부터 요청을 받기까지 p50 / 최대 | 판정 |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | --- |
| 1 | 유휴 | 607 | 558 | 26 (24) | 6, 7, 7, 6 | 28.9 / 131.8 | 통과 |
| 1 | 부하 | 1,850 | 1,654 | 76 (72) | 19, 19, 19, 19 | 14.5 / 4,062.6 | 통과 |
| 2 | 유휴 | 607 | 558 | 26 (24) | 6, 7, 7, 6 | 31.2 / 111.4 | 통과 |
| 2 | 부하 | 1,842 | 1,656 | 77 (72) | 19, 20, 19, 19 | 15.2 / 4,117.7 | 통과 |
| 3 | 유휴 | 606 | 557 | 26 (24) | 6, 7, 7, 6 | 29.3 / 88.8 | 통과 |
| 3 | 부하 | 1,837 | 1,658 | 77 (72) | 19, 20, 19, 19 | 15.1 / 1,659.5 | 통과 |

유휴 구간에서는 새 gateway가 시작한 뒤 132 ms 안에 요청을 받았습니다. 부하 아래에서도 중앙값은 15 ms였지만, 가장 느린 시작은 처음 두 반복에서 4.1초, 셋째 반복에서 1.7초 걸렸습니다. 절차에 이 시간의 목표는 없으므로 판정하지 않고 보고만 합니다.

| 반복 | 구간 | 다른 프로세스의 요청 (경쟁 gateway) | timeout 대상 (gateway가 없을 때 온 deadline) | deadline 뒤 가장 늦은 끝 | 승인 (SIGKILL 때 재개 진행 중) | 가장 긴 재개 | 태그를 붙인 명령: 시작, 거부 |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 유휴 | 405 (27) | 25 (12) | 103.5 | 26 (6) | 269.8 | 56, 20 |
| 1 | 부하 | 1,155 (77) | 75 (35) | 104.1 | 76 (19) | 273.5 | 161, 58 |
| 2 | 유휴 | 405 (27) | 25 (12) | 101.6 | 26 (6) | 279.6 | 56, 18 |
| 2 | 부하 | 1,170 (78) | 76 (38) | 113.0 | 77 (19) | 2,014.1 | 164, 57 |
| 3 | 유휴 | 405 (27) | 25 (12) | 119.4 | 26 (6) | 261.3 | 55, 18 |
| 3 | 부하 | 1,170 (78) | 76 (37) | 103.7 | 77 (19) | 331.1 | 162, 62 |

재시작할 때마다 다른 프로세스의 Claim은 거부되었고, 이전 epoch, 현재 epoch, epoch 없이 보낸 요청 15개도 각각 `STALE_CONTROLLER`로 거부되어 아무것도 바꾸지 않았습니다. 경쟁 gateway는 모두 writer를 기다린 뒤 Runner에 닿지 않고 포기했습니다. timeout 대상은 모두 `timeout`으로 보고되었고, deadline이 gateway가 있을 때 왔든 없을 때 왔든 원래 deadline 뒤 120 ms 안에 끝났습니다. 절차가 허용하는 것은 2초입니다. 먼저 끝나거나 deadline이 바뀐 대상은 없었습니다. 승인은 모두 다음 gateway가 재개했고, 두 번째 재개는 같은 프로세스를 돌려줬으며, 그 명령은 한 번 실행되었습니다. SIGKILL로 gateway가 끝날 때 재개가 진행 중이던 75개도 마찬가지입니다. 재개는 workspace의 대조를 기다린 시간을 포함해 첫 시도부터 답까지 최대 331 ms 걸렸고, 둘째 반복의 부하 구간에서만 가장 긴 것이 2,014 ms였습니다. 두 번 실행된 태그를 붙인 명령은 없었고, 거부된 명령이 실행된 적도 없습니다.

제어 경로의 목표는 gateway가 요청을 받은 시간에 대해 `process_status` p99 500 ms 이하와 종료 확인 응답 p99 1초 이하입니다. 다른 시간은 정해진 목표가 없어 보고만 합니다. 종료처럼 표본이 100개보다 적으면 nearest-rank p99는 가장 큰 값입니다. 일정은 gateway가 요청을 받은 시간을 따르므로 표본 수는 일정보다 조금 많거나 적을 수 있습니다.

| 반복 | 구간 | status p99 / 최대 | status 표본 / 일정 | 종료 확인 p99 | 종료 / 일정 | scope 소멸 p99 | 해제 p99 | I/O p99 | 거부 p99 |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 유휴 | 3.283 / 7.786 | 1,120 / 1,116 | 0.540 | 27 / 27 | 79.1 | 188.6 | 22.942 | 49.405 |
| 1 | 부하 | 1.882 / 41.516 | 3,297 / 3,306 | 0.487 | 82 / 82 | 92.6 | 195.3 | 2.390 | 48.449 |
| 2 | 유휴 | 3.137 / 9.269 | 1,116 / 1,114 | 0.739 | 27 / 27 | 77.5 | 185.7 | 6.239 | 46.620 |
| 2 | 부하 | 1.475 / 누락 | 3,312 / 3,312 | 0.508 | 84 / 82 | 66.7 | 182.2 | 17.081 | 47.541 |
| 3 | 유휴 | 2.651 / 8.858 | 1,116 / 1,114 | 0.543 | 26 / 27 | 65.6 | 161.2 | 5.523 | 41.619 |
| 3 | 부하 | 1.497 / 15.581 | 3,316 / 3,314 | 0.486 | 82 / 82 | 76.8 | 184.3 | 2.640 | 53.347 |

둘째 반복의 부하 구간에서는 각 status 대상의 일정을 하나씩 놓쳤습니다. 앞 표본이 아직 실행 중일 때 그 시각이 지나갔으므로, DevGuard 절차가 세는 대로 누락된 표본으로 셉니다. 누락된 표본은 모든 응답보다 높은 순위이므로 그 구간의 최대는 누락으로 표시되며, p99는 1.475 ms였습니다. 오류이거나 이미 실행 중이 아닌 대상에 대한 status 표본은 없었습니다. 확인된 종료는 모두 대상을 끝냈고 해제되었으며, 각 status 대상의 마지막 종료도 효과가 있었습니다. I/O 줄은 모두 돌아왔고 가장 느린 것은 506 ms였습니다. 거부는 모두 `RESOURCE_UNAVAILABLE`이었고, 거부되어야 할 요청이 허용된 적은 없습니다. 종료 대상 두 개와 거부 표본 두 개는 막 시작한 gateway에게서 `RECOVERY_PENDING`을 한 번 받고, 절차가 허용하는 대로 다시 보내졌습니다. 첫째 반복의 유휴 구간과 셋째 반복의 부하 구간에서 각각 하나씩입니다. MCP 연결이 끊긴 표본은 없었습니다.

두 출력 흐름은 모든 gateway를 거쳐 읽었습니다. PTY의 번호 붙은 줄은 건너뛰거나 되풀이한 것 없이 모두 왔습니다. pipe의 40,000줄 한꺼번에 쓰기는 매번 gateway가 읽지 않는 동안 일어났고, 그때마다 Runner는 256 KiB ring을 넘는 22,524줄에 공백(`ring_overflow`)을 알렸으며, 나머지 17,476줄은 모두 왔습니다. 알린 공백 밖에서 잃거나 되풀이한 줄은 없었습니다.

| 반복 | 구간 | PTY에서 읽은 줄 | pipe에서 읽은 줄 | Runner가 알린 공백 | 알린 공백의 줄 |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | 유휴 | 2,825 | 52,428 | 3 | 67,572 |
| 1 | 부하 | 8,795 | 157,284 | 9 | 202,716 |
| 2 | 유휴 | 2,825 | 52,428 | 3 | 67,572 |
| 2 | 부하 | 8,756 | 174,760 | 10 | 225,240 |
| 3 | 유휴 | 2,822 | 52,428 | 3 | 67,572 |
| 3 | 부하 | 8,765 | 174,760 | 10 | 225,240 |

**Launch 기록.** 실행의 gateway가 기록하거나 표본이 가리킨 attempt 967개가 모두 DevGuard journal에 있었습니다. 두 번 launch된 attempt, 다시 허용된 불확실한 attempt, 자원을 차지한 채 남은 것은 없었습니다. journal은 유휴 구간마다 4개, 부하 구간마다 13개나 14개의 launch를 gateway 로그보다 많이 기록했습니다. gateway가 기록하기 전에 죽으면, Runner가 그 gateway를 위해 한 launch는 journal에는 있고 어느 gateway 로그에도 없습니다. 이 수는 보고만 하고 중복으로 세지 않으며, 태그를 붙인 명령이 실행될 때마다 한 줄씩 쓰는 marker 파일이 두 번 실행된 명령이 없음을 보여 줍니다.

**부하.** DG-1의 소비자 여섯은 부하 구간 세 개에서 583번(구간마다 193–196번) 실행했고 모두 성공했습니다. 0이 아닌 종료, 시작 실패, 불확실한 실행은 없었고, 허용된 부하가 각 부하 구간의 99.99%를 덮었습니다. 모든 소비자가 모든 구간에서 실행했습니다. `cargo`는 19–21번, `cpu`와 `memory`는 각각 15번, `io`는 58번, `output`은 58–59번, `slow-input`은 28번입니다.

**호스트와 사용량.** 모든 구간의 호스트 표본이 메모리 pressure 수준 1(정상)이었고, swap은 최대 1,378 MiB, load average는 둘째 반복의 부하 구간에서 최대 7.92였습니다. 이 모드에서 모든 실행을 소유하고 `codespace` 소비자의 인스턴스로 등록하는 Runner는 모든 구간에서 소비자의 예약 150 mCPU, 512 MiB, 40 tasks 안에 있었습니다. 최대값은 RSS 15.25 MiB와 thread 15개였고, CPU 평균은 최대 2.9 mCPU였습니다. 각 gateway는 최대 21.3 MiB와 thread 12개, CPU 평균 최대 4.9 mCPU였습니다. 이 비교는 판정하지 않고 보고만 합니다.

**증거 writer.** 하네스의 원시 증거 writer는 유휴 구간에서 최대 33 ms, 부하 구간에서 최대 6.7초 늦었습니다. 그동안 DG-1의 cold 빌드가 증거 디스크를 함께 썼습니다. [#115](https://github.com/novelKR/CodeSpace/pull/115) 뒤로 각 gateway의 로그는 writer가 저장하기 전에 분석하며, 모든 종료 표본을 DevGuard가 해제할 때까지 지켜봤습니다.

**예행과 그 뒤.** 예행의 두 구간(유휴 127초와 부하 216초, 재시작 5번과 7번)도 같은 0의 기록으로 통과했고, status p99는 최대 6.329 ms, 종료 확인 응답 p99는 최대 0.916 ms였습니다. 실행이 끝났을 때 마지막 구간의 Runner는 drain된 상태였고, 자원을 차지한 것도 lease도 없었으며, DevGuard journal의 `codespace` 인스턴스는 모두 retire되었고, 서비스는 프로세스 ID 23023으로 그 release를 계속 실행했습니다. 예행과 실행 동안 서비스는 재시작하지 않았습니다.

<a id="recovery-report-completion"></a>

## 완료 증거

절차의 [완료 증거](recovery-qualification.md#recovery-completion)와, 후보에서 한 실행이 보여 준 것입니다.

| P1-RECOVERY가 보여야 하는 것 | 보여 주는 곳 | 결과 |
| --- | --- | --- |
| 재시작 뒤 같은 실행에 다시 닿음 | 측정의 재시작 308번. 매번 다음 gateway가 오래 실행되는 프로세스가 모두 `running`임을 확인하고 두 출력 흐름을 계속 읽음. 생명주기 `exit-kinds`, fence `stale-epoch` | 보임: 잃은 오래 실행되는 프로세스가 없고, Runner가 알린 공백 밖에서 잃거나 되풀이한 줄이 없음 |
| 두 번 실행된 실행 없음 | 측정의 승인 308개, 시작한 태그를 붙인 명령 654개와 거부된 233개, gateway 로그와 대조한 DevGuard journal. 대조 `approval`, `kill-during-exec`, `kill-during-terminate` | 보임: 명령마다 한 번 실행되었고, 거부된 명령은 실행되지 않았으며, 두 번 launch된 attempt가 없음 |
| stale mutation 없음 | 주기마다 다른 프로세스의 요청 15개(모두 4,710개)와 경쟁 gateway(모두 314개). fence 스위트 | 보임: 요청은 모두 stale로 거부되어 아무것도 바꾸지 않았고, Runner에 닿은 경쟁 gateway가 없음 |
| timeout 연장 없음 | 측정의 timeout 대상 302개(그중 146개는 gateway가 없는 동안 deadline이 옴). 생명주기 `original-timeout` | 보임: 연장되거나 먼저 끝나거나 잘못 보고된 대상이 없고, 가장 늦은 것도 deadline 뒤 119 ms에 끝남 |
| 명확한 공백 | 절차의 [공백](recovery-qualification.md#recovery-gaps) | `recovery-observation` 검사, 예행, 실행에 이유와 함께 실행하지 않음으로 기록 |
| 기존 모드가 계속 통과 | [회귀](#recovery-report-regressions) | 보임: 후보에서 CS-RG 스위트 네 개, 그 commit의 CI |
| 지원 모드 | 위의 [주장](#recovery-report-claims) | macOS `off`와 `required`는 qualification 통과, Linux `off`는 CI 기능 확인일 뿐, Linux `required`는 지원하지 않음 |

<a id="recovery-report-regressions"></a>

## 회귀

실행이 끝났을 때 마지막 Runner는 drain된 상태였고 DevGuard에 활성 `codespace` 인스턴스도 차지한 자원도 없었으므로, CS-RG 스위트가 자기 gateway를 등록할 수 있었습니다. CS-RG 스위트는 2026-10-19 02:22 UTC부터 02:31 UTC까지 qualification 호스트에서 같은 후보로, 바이너리를 빌드한 세션에서 그 세션의 열린 파일 한도 1,048,576 아래에서 실행했습니다. 그러므로 이 실행은 터미널의 한도 256에 대해서는 아무것도 보여 주지 않습니다. 모두 통과했고 확인되었습니다.

| 스위트 | 시각(UTC) | 결과 | CS-RG의 후보 `ca306ee`에서 |
| --- | --- | --- | --- |
| `candidate` | 02:22 | 통과. 기록은 고정한 기록과 기록한 시각만 다름 | 통과 |
| `matrix` | 02:22–02:23 | 통과: 시나리오 45개 가운데 45개(`in-process` 22개, `uds` 23개), 검사 210개와 213개 | 통과: 45개 가운데 45개 |
| `faults` | 02:23–02:26 | 통과: 사례 27개 가운데 27개(13개와 14개), 검사 83개와 89개 | 통과: 27개 가운데 27개 |
| `authority` | 02:27–02:31 | 통과: 단계 10개 가운데 10개, MCP 연결이 끊긴 표본 없음 | 통과: 10개 가운데 10개 |

그 뒤 자원을 차지한 것도 활성 `codespace` 인스턴스도 없었고, 서비스는 프로세스 ID 23023으로 그 release를 계속 실행했습니다.

후보 commit의 CI는 두 번 실행되었고 두 번 모두 모든 작업이 통과했습니다. [#115](https://github.com/novelKR/CodeSpace/pull/115)를 병합한 뒤의 push 실행([실행 39114783991](https://github.com/novelKR/CodeSpace/actions/runs/39114783991), 2026-10-18 09:18–09:31 UTC)과 전체 프로필의 예약 실행([실행 39169645390](https://github.com/novelKR/CodeSpace/actions/runs/39169645390), 21:51–22:02 UTC)입니다. 각 실행은 형식과 pin 검사, 정책 검사, Clippy 빌드 세 개와 단위 단계 일곱 개, Ubuntu의 `integration`과 `linux-isolation`, 호스팅된 macOS runner의 `macos-core`, `macos-fork-race`, `dependencies`를 실행했습니다. 이것이 독립 Runner 시험을 포함한 Linux와 macOS의 회귀 단계이며, Linux에서는 위에서 [주장](#recovery-report-claims)한 `off`의 기능 확인입니다.

<a id="recovery-report-amendments"></a>

## 수정 기록

후보는 네 번 고정했습니다. 처음 세 번은 각각 그 후보에서 검사 스위트 다섯 개가 통과한 뒤 고정했고, 각각 나중에 하네스의 결함으로 실패했으며 CodeSpace나 DevGuard의 결함은 없었습니다. 고칠 때마다 절차와 함께 고정한 하네스가 바뀌었으므로 새 후보가 되었고, 실행은 검사부터 다시 했습니다. 이전 후보의 실행은 모두 보존했으며 [증거](#recovery-report-evidence)에 있습니다.

- **열린 파일 한도, `b2cb042`(정정).** 첫 후보는 [#112](https://github.com/novelKR/CodeSpace/pull/112)의 병합이었습니다. 검사 스위트 다섯 개는 열린 파일 한도가 높은 세션에서 통과했지만, 기본 한도가 256인 터미널에서 실행한 예행은 `harness_failed`로 실패했습니다. 하네스가 gateway마다 pipe와 transcript를 열어 둔 채였기 때문에, 27번째 기능 시나리오부터는 하네스 자체에서 `Too many open files`로 모두 실패했고, 부하 구간의 drain은 journal을 열지 못했습니다. [#113](https://github.com/novelKR/CodeSpace/pull/113)이 더는 읽지 않는 것을 하네스가 닫게 해, 예행 전체에서 열린 파일이 많아야 43개가 되었고, 모든 보고서가 실행한 한도를 기록합니다. 다음 후보 `41f83b6`은 한도 256에서 검사를 통과했습니다.
- **재시작 직후의 경쟁, `41f83b6`(정정).** 전체 실행이 gateway의 처음 몇 밀리초에 생기는 하네스 경쟁 세 가지로 실패했습니다. 그동안 gateway는 제품 문서대로 새 작업을 거부하고 있었습니다. gateway의 Claim 5 ms 뒤, 표본의 workspace를 대조하기 전에 보낸 종료 표본이 `RECOVERY_PENDING`으로 거부되어 오류로 셌습니다. `required`의 대조 `approval`에서는 두 round가 새 gateway가 workspace를 대조하기 전에 실행을 잡아 두었습니다. `required` pipe의 `kill-during-terminate`에서는 `process_status`가 앞 프로세스가 끝났다고 보고한 지 1 ms도 안 되어 새 작업이 `WORKSPACE_BUSY`로 거부되었습니다. 두 번 실행된 명령은 없었습니다. [#114](https://github.com/novelKR/CodeSpace/pull/114)가 이런 거부를 만나는 동안 하네스가 그 작업을 다시 보내게 했고(지금 절차의 [기능 스위트](recovery-qualification.md#recovery-functional)와 [구간](recovery-qualification.md#recovery-intervals)에 적혀 있음), 표본과 round마다 기다린 시간을 기록하게 했습니다.
- **바쁜 디스크에서의 gateway 로그, `b5ddd59`(정정).** 전체 실행이 둘째 부하 구간의 종료 표본 84개 가운데 하나를 하네스가 지켜보지 못해 실패했습니다. 하네스가 gateway 로그를 읽으면서 각 줄을 분석하기 전에 증거에 먼저 썼는데, DG-1의 cold 빌드가 증거 디스크를 바쁘게 하는 동안 attempt ID가 표본의 5초 대기를 넘겨 도착했습니다. DevGuard는 그 attempt를 `scope_terminated`로 해제했습니다. [#115](https://github.com/novelKR/CodeSpace/pull/115)가 각 줄을 먼저 분석하고 로그는 별도 스레드에서 쓰게 했고, 지금 [절차](recovery-qualification.md#recovery-running)에 그렇게 적혀 있습니다. 로그를 쓰지 못하면 그 구간은 판정 불가입니다. `1c0eadd`에서도 부하 아래 writer는 최대 6.7초 늦었지만 놓친 launch 줄은 없었습니다.
- **검사와 고정을 실행한 사람(보완).** `41f83b6`에서 qualification 호스트를 다루는 세션은 `candidate`를 실행하지 못했습니다. 그 안전 검사가 명령을 거부했습니다. 그 뒤로는 소유자가 검사 스위트, `candidate`, 예행, 측정을 터미널에서 검토한 명령 하나로 실행했습니다. 이 명령은 앞 단계가 깨끗할 때만 다음 단계를 시작했고, 예행은 기록이 검사한 조합과 일치할 때만 시작했습니다. 대조한 것은 commit, code tree, 바이너리 다섯 개, release, 정책 fingerprint, DG-1의 정책, 메모리 pressure입니다. 절차의 순서는 바뀌지 않았습니다.
- **`WORKSPACE_BUSY`가 이어지는 시간(참고).** 이 모드에서 gateway의 안내문은 실행 중으로 확인된 프로세스가 workspace를 다시 잡으므로, 그 프로세스가 "끝나거나 종료될 때까지" 그 workspace의 새 작업은 `WORKSPACE_BUSY`를 돌려준다고 말합니다. 실제로 workspace는 프로세스의 출력이 닫힌 뒤 Runner의 종료 이벤트가 올 때 풀리며, 이는 `process_status`가 처음 프로세스가 끝났다고 보고한 때보다 늦을 수 있습니다. 이 거부 뒤에 새 작업을 다시 보내는 클라이언트는, 지금 하네스처럼, 영향을 받지 않습니다. 이 보고서는 문서만 바꾸므로 안내문은 여기서 바꾸지 않았고, 정확하게 고치는 일은 별도 변경입니다.

<a id="recovery-report-evidence"></a>

## 증거

증거 디렉터리는 qualification 호스트의 DevGuard checkout 아래 `evidence/` 디렉터리에 있고, 모든 실행의 콘솔 출력과 `verify` 출력은 그 디렉터리 옆에 보관했습니다. 각 디렉터리는 모든 파일의 SHA-256을 담은 `MANIFEST.json`으로 봉인했고, 파일은 읽기 전용입니다. 디렉터리에도, 그 옆의 로그에도 자격 증명, 자격 증명의 digest, 자식 프로세스의 환경은 없습니다. 2026-10-19에 파일 13,454개를 모두 검사해 하나도 찾지 못했습니다. 디렉터리는 그 `MANIFEST.json`의 SHA-256이 아래와 같고 `qualify-devguard.py verify`가 받아들이면 확인된 것입니다.

| 디렉터리 | 실행 | `MANIFEST.json` SHA-256 |
| --- | --- | --- |
| `p1r-c06-check-recovery-lifecycle-2026-10-18-1c0eadd` | `recovery-lifecycle` | `ea8f534f434080e9eb33909a8b4aebb98bd6c3791996fb8ca424c0307d0b1da3` |
| `p1r-c06-check-recovery-store-2026-10-18-1c0eadd` | `recovery-store` | `45123bc349de4341202cfdbcfce19ee182584ad4f1272b8d131d3b276b506197` |
| `p1r-c06-check-recovery-fence-2026-10-18-1c0eadd` | `recovery-fence` | `299e646f77497f7ae8a2d7c58f92b89ea59499f562d86ca648d64f3d9675e6fb` |
| `p1r-c06-check-recovery-reconcile-2026-10-18-1c0eadd` | `recovery-reconcile` | `a176626961017483061f4d4a9334ebf4811037782170084697c43bbf66d4164c` |
| `p1r-c06-check-recovery-observation-2026-10-18-1c0eadd` | `recovery-observation` | `37fcf791b8bfca09bf1c805141ee7108a44448a790551611e2c62699a6ffa407` |
| `p1r-c06-candidate-2026-10-18-1c0eadd` | `candidate` | `c7b26e3dc118e4b07b27255e038dcb02d2e1b411ef073d6027754f60fb4622ad` |
| `p1r-c06-recovery-rehearsal-2026-10-18-1c0eadd` | `recovery --rehearsal` | `ef5ab907a9fbe03fa737b2b23d0f2023e64b36685b466248465a32a5a0880894` |
| `p1r-c06-recovery-2026-10-18-1c0eadd` | `recovery` | `1f6f7ba8e4694d4aba6aa371db2edaeeb6fc0b0d7812dd17847444d1e06f790b` |
| `p1r-c06-qual-candidate-2026-10-19-1c0eadd` | CS-RG의 `candidate` | `caff08ef246a57f5e673e7b54a41fd3d99376b3c14d12d463270911c2750898f` |
| `p1r-c06-qual-matrix-2026-10-19-1c0eadd` | `matrix` | `33443fce5b6d0261e74cfe77a317a7de3bcbd019836331c3a63cefca40e2011f` |
| `p1r-c06-qual-faults-2026-10-19-1c0eadd` | `faults` | `bb29b613eff8df0c8df8108f0d1ba953b5cc9298a2c79cafbee6f4a280b55936` |
| `p1r-c06-qual-authority-2026-10-19-1c0eadd` | `authority` | `7cebeacfb92cb39b6956607b9cc2e80663102ce307152cf438a7bb4229bfbb65` |

이전 후보의 실행도 보존했습니다. 각각 실행할 때 자신의 하네스로 확인했고, 2026-10-19에 후보의 하네스로 실행한 `verify`도 모두 다시 받아들였습니다. `harness_failed`로 기록된 `b2cb042`의 예행은 다시 계산하면 실패입니다.

| 디렉터리 | 결과 | `MANIFEST.json` SHA-256 |
| --- | --- | --- |
| `p1r-c06-check-recovery-lifecycle-2026-10-17-b2cb042` | 통과 | `b401d3c878d95148f2aa3f42d0b6013fa0a5e0a723c2f0acf352809e5d5b9ab6` |
| `p1r-c06-check-recovery-store-2026-10-17-b2cb042` | 통과 | `c0aaca7ed5f5c917ec5711c4f8b435a8fb49b616c284ddef8298c990974c7b0a` |
| `p1r-c06-check-recovery-fence-2026-10-17-b2cb042` | 통과 | `5efb6bf4d9830fbbe50cbce31a0ac0f570ec3a67787eb9b481a18a42063dc778` |
| `p1r-c06-check-recovery-reconcile-2026-10-17-b2cb042` | 통과 | `d70cd660821dd558c75d0fa5a8f9c5a97029c8147b28cd446335112077a59193` |
| `p1r-c06-check-recovery-observation-2026-10-17-b2cb042` | 통과 | `971fc90242a11cbd4d9db405d26f6e79ecf12f41ddf6ad3b415f3655383f52d9` |
| `p1r-c06-candidate-2026-10-17-b2cb042` | 통과 | `15c6735be844292b3f84ad74ce20a534d1ec6b9d9bdc38ab7988f98d971f87c7` |
| `p1r-c06-recovery-rehearsal-2026-10-17-b2cb042` | `harness_failed`: 열린 파일 한도 | `5f58673c69f35c993852953daf4d5a09ff0dbaf2c857a7fca343aeb507fac631` |
| `p1r-c06-check-recovery-lifecycle-2026-10-17-41f83b6` | 통과 | `0614d740f7eef0a96941e6c36ec974666b22a301f3f958e0642f29d05a6efb62` |
| `p1r-c06-check-recovery-store-2026-10-17-41f83b6` | 통과 | `395b4c051b483c3e9609bf3aaee4ecfbb19df82ad3ae213156fa652fc3cddfe8` |
| `p1r-c06-check-recovery-fence-2026-10-17-41f83b6` | 통과 | `f81578c3398efefda24a9f2758b53499d5ebb969d93b80800825ba5218824271` |
| `p1r-c06-check-recovery-reconcile-2026-10-17-41f83b6` | 통과 | `9ad23449bb1bc765f99b937ee9b7ad8716b8864f6ee72a97e3b2d53e0d54dfa1` |
| `p1r-c06-check-recovery-observation-2026-10-17-41f83b6` | 통과 | `66434755ffe5ffb3aa0c60fb28c4397f188d0ff6bd0fdc91bacd92813c3a8921` |
| `p1r-c06-candidate-2026-10-17-41f83b6` | 통과 | `37ed6aa3967f25602dc00f7ce7c40bb8b773ecea102329e2c50ccdb5c94b4fdc` |
| `p1r-c06-recovery-rehearsal-2026-10-17-41f83b6` | 판정 불가(설계대로) | `aad4c41e36b6d6ddcffe76e24de32fd013f765d781c3de7e70ae5c1e28cfefd6` |
| `p1r-c06-recovery-2026-10-17-41f83b6` | 실패: 재시작 직후의 경쟁 | `1b2b57fbea66ae3a08b78dcd76d86f992dc4709a21e22a51343b97cf0f0b22a2` |
| `p1r-c06-check-recovery-lifecycle-2026-10-18-b5ddd59` | 통과 | `221a31cd238d47ec57190b1355994f473c7fa2592185752bcc81ae647c9ff501` |
| `p1r-c06-check-recovery-store-2026-10-18-b5ddd59` | 통과 | `b8533e38c2a663dac17dd23e9789d0e30aec9c02cb65870d1bd1cf4800c0991b` |
| `p1r-c06-check-recovery-fence-2026-10-18-b5ddd59` | 통과 | `9109c738e3f1e4e0279368a37fcefd6b4ef9a86414dbe7b0921ea6b16ebe4048` |
| `p1r-c06-check-recovery-reconcile-2026-10-18-b5ddd59` | 통과 | `2f07f807876c13f1b7cf63120bb6e92763e19de5a9d5cebeaa3d5e753c60dfb8` |
| `p1r-c06-check-recovery-observation-2026-10-18-b5ddd59` | 통과 | `bec8ab11383407e43fa041ffdfe163c606ecbf8ad996d1738ac0a93b48b37f50` |
| `p1r-c06-candidate-2026-10-18-b5ddd59` | 통과 | `1fe4928c3666310f83da9914985627dcc9fb72ff8202d1b7ae5d95b5b7d2abf9` |
| `p1r-c06-recovery-rehearsal-2026-10-18-b5ddd59` | 판정 불가(설계대로) | `5b2237d8c70037cd6ef1a1fa8e346063a4cf43363a3b62a99c4ca603756f1226` |
| `p1r-c06-recovery-2026-10-18-b5ddd59` | 실패: 바쁜 디스크에서의 gateway 로그 | `dbdee96032d7d715a56a729cb1b6b97ace2901bc93f834e9fdf85e2dc7945f02` |

<a id="recovery-report-outside"></a>

## P1-RECOVERY 밖에 남는 것

P1-RECOVERY는 여기서 끝납니다. 다음은 그 밖에 남으며, 이 문서는 어느 것도 주장하지 않습니다.

- **Runner 자신이 재시작한 뒤 프로세스 복원.** gateway는 잃은 Runner 자리에 새 Runner를 받아들이지 않으며, 잃은 Runner가 실행하던 것은 운영자가 정리합니다([Runner 손실](operations.md#runner-loss)).
- **독립 모드를 기본값으로 바꾸는 일.** DevGuard ADR-002에 따른 별도 결정입니다.
- **DG-LINUX**, Linux에서의 강제. 이것이 qualification을 통과하기 전까지 Linux에서 `required`는 거부되고, `off`는 Linux에서 기능 확인만 합니다.
- **DevGuard release나 정책의 변경.** 예를 들어 `codespace` 소비자에 인스턴스를 둘 이상 두는 것(결정 D12)이며, 그러면 DG-1을 다시 측정해야 합니다.
- **다른 조합.** 다른 DevGuard client pin, release, 정책, 호스트, 코드 변경, 다른 도구는 새 후보이며 자기 실행이 필요합니다.
- **실행이 보이지 않은 것:** 실제 호스트 재시작, DG-1의 warm 부하, runner wire에서의 메시지 재전송, 이 후보에서의 CS-RG 제어 경로 측정.

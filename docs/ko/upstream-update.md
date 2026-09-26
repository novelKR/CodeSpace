<a id="업스트림-핀-갱신"></a>
<a id="업스트림-핀-갱신"></a>

# Codex 의존성 업데이트

[English](../upstream-update.md) | [한국어](upstream-update.md)

고정 버전은 검토 가능한 PR로 변경합니다. 업데이트는 [Codex 재사용 범위](codex-reuse.md)에 있는 모든 어댑터의 프로세스·파일 시스템·네트워크 동작에 영향을 줄 수 있습니다.

<a id="점검-목록"></a>
<a id="점검-목록"></a>

## 후보 검토

태그나 커밋을 명시적으로 선택하고 이유를 기록합니다. 패치 처리, PTY, 프로세스 보호, 파일 시스템, 샌드박스, 프록시의 관련 변경을 확인하세요. 런타임 의존성과 개발용 의존성은 구분합니다. 개발용 의존성이 있다는 사실만으로 제품 실행 파일에 포함된다고 볼 수는 없습니다.

서브모듈, [버전 기록](upstream-lock.md), 영향받는 어댑터 잠금 파일, 필요한 출처 고지를 함께 갱신합니다. 핵심 타입과 Codex 어댑터 타입의 분리를 유지하세요. 호환되지 않는 의존성을 감추기 위해 업스트림 crate 하나를 제품에 복사하지 않습니다.

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

<!-- CI fixture: documentation-only selection; not for merging. -->

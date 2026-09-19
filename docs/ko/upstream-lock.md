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
| 사용하는 구성 요소 | 패치, 런타임 worker, PTY, 파일 시스템, Linux 샌드박스·프록시 어댑터 |
| 패치 옵션 | `PreserveLineEndings`, `follow_symlinks: false` |

정확한 역할은 [연결된 구성 요소](codex-reuse.md)를 참고하세요. 패치 테스트는 선택된 동작 일치 사례를 검사하며 업스트림 전체 테스트나 Codex의 모든 동작을 검증하지는 않습니다.

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
PIN_ONLY=1 ./scripts/check-upstream-pin.sh
```

이 명령은 서브모듈 커밋과 문서의 일치 여부만 검사합니다. `PIN_ONLY`를 생략하면 패치 테스트도 실행하지만 다른 어댑터 검증을 대신하지는 않습니다. 버전을 바꾸기 전에 [전체 업데이트 절차](upstream-update.md)를 따르세요. 배포 과정에서 `git submodule update --remote`로 최신 버전을 따라가지 않습니다.

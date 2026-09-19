<a id="codespace-문서"></a>
<a id="codespace-문서"></a>

# CodeSpace 문서 안내

[English](../index.md) | [한국어](index.md)

CodeSpace를 직접 구성한 코딩 에이전트의 실행 계층으로 연결하는 방법을 안내합니다. 먼저 로컬 작업 공간을 등록하고 MCP로 연결한 다음, 에이전트에 실행 결과 처리와 복구 규칙을 적용하세요.

<a id="다음-단계"></a>
<a id="다음-단계"></a>

## 목적에 맞는 문서 찾기

| 필요한 정보 | 문서 |
| --- | --- |
| 제품의 역할과 제약 이해 | [시작하기](../../README.ko.md) |
| 설치와 작업 공간 연결 | [운영 가이드](operations.md) |
| 읽기·수정·실행·복구 루프 구성 | [Agent Loop 연동](agent-integration.md) |
| ChatGPT 연결 가능성 검토 | [ChatGPT 연결 상태](chatgpt-connector.md) |
| 모듈별 책임 이해 | [아키텍처](architecture.md), [실행 계약](execution-substrate.md) |
| 권한과 격리 조건 확인 | [보안 모델](security-model.md), [러너 격리](runner-isolation.md) |
| 프로토콜과 도구 결과 처리 | [프로토콜 호환성](protocol-compatibility.md), [패치 동작](behavior-differences.md), [오류 코드](error-codes.md) |
| Codex 의존성 유지보수 | [재사용 범위](codex-reuse.md), [고정 버전](upstream-lock.md), [업데이트 절차](upstream-update.md) |
| 문서 수정과 게시 | [문서 유지보수](documentation.md) |

지원 동작은 실제 구현을 기준으로 설명합니다. 테스트 안내는 검사 범위를 나타내며, 특정 설치 환경이나 외부 계정에서 검증을 마쳤다는 뜻은 아닙니다. 실제 사용할 작업 공간의 실행 조건은 `workspace_info` 응답에서 확인하세요.

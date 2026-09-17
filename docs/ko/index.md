# CodeSpace 문서

[English](../index.md) | [한국어](index.md)

CodeSpace는 개인용 실행 도구 MCP 서버입니다. 바깥 클라이언트가 무엇을
할지 판단합니다. 이 프로세스는 모델을 호출하지 않습니다. 파일을 읽고,
Codex 형식 패치를 적용하며, 등록된 워크스페이스에서 관리형 명령을
실행합니다.

## 다음 단계

- [시작하기](../../README.ko.md) — 도구, 실행 명령, 라이선스
- [운영](operations.md) — 설치, 실행, 로그, 복구
- [ChatGPT 커넥터](chatgpt-connector.md) — stdio, HTTP, 미검증 계정 검사
- [아키텍처](architecture.md) — 현재와 목표 프로세스 배치
- [실행 기반](execution-substrate.md) — 모델 없는 실행 불변식
- [프로토콜 호환성](protocol-compatibility.md) — MCP 2025-11-25 기준
- [동작 차이](behavior-differences.md) — 제품 정책 대 크레이트 기본값
- [보안 모델](security-model.md) — 게이트웨이 정책과 신뢰 경계
- [러너 격리](runner-isolation.md) — 현재 호스트 exec와 목표 Linux 컨테이너
- [오류 코드](error-codes.md) — 전송 실패 대 실행 오류
- [Codex 재사용](codex-reuse.md) — 제품 대 프리미티브
- [업스트림 고정](upstream-lock.md) — 핀된 Codex 서브모듈
- [업스트림 핀 갱신](upstream-update.md) — 의도적 릴리스 절차
- [문서 사이트](documentation.md) — 영·한 레지스트리와 Pages

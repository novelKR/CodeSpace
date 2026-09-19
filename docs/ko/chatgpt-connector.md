<a id="chatgpt-커넥터-실험"></a>
<a id="chatgpt-커넥터-실험"></a>

# ChatGPT 연결 상태

[English](../chatgpt-connector.md) | [한국어](chatgpt-connector.md)

먼저 [설치](operations.md)와 [Agent Loop 연동](agent-integration.md)에 따라 로컬 MCP 연결을 확인하세요. CodeSpace는 stdio와 Streamable HTTP를 구현하지만 저장소의 로컬 테스트로 실제 ChatGPT 계정 연결까지 검증한 것은 아닙니다.

<a id="로컬-stdio-cargo-test로-검증"></a>
<a id="로컬-stdio-cargo-test로-검증"></a>
<a id="로컬-streamable-http-실험"></a>
<a id="로컬-streamable-http-실험"></a>

## 로컬 MCP 클라이언트

stdio를 지원하는 클라이언트가 빌드된 `codespace-mcp`를 실행하도록 설정하고, `CODESPACE_CONFIG`, `CODESPACE_PATCH_BIN`, 필요하면 `CODESPACE_OPERATIONS_DB`를 절대 경로로 전달하세요. 클라이언트별 설정 파일 형식은 다르지만 실행 파일, 인자, 환경변수는 공통으로 필요한 항목입니다.

HTTP 클라이언트에서는 설정된 서버를 `--http`로 시작하고 `http://127.0.0.1:8787/mcp`에 연결합니다. `CODESPACE_HTTP_TOKEN`이 설정되어 있으면 같은 값의 Bearer 헤더를 보내세요. 초기화를 마친 뒤 등록된 작업 공간으로 `workspace_info`를 호출합니다. 서버가 시작되었다는 사실만으로 연결 검증이 끝나지는 않습니다.

<a id="chatgpt-계정-연결"></a>
<a id="chatgpt-계정-연결"></a>

## ChatGPT에서 별도로 확인할 조건

2026-09-19에 확인한 OpenAI의 [개발자 모드 문서](https://developers.openai.com/api/docs/guides/developer-mode#how-to-use)는 streaming HTTP와 OAuth 등을 사용하는 원격 MCP 앱을 설명합니다. 그러나 이것이 CodeSpace의 정적 Bearer 설정을 ChatGPT 인증 흐름으로 사용할 수 있다는 근거는 아닙니다.

CodeSpace에는 OAuth 인증 서버가 없습니다. HTTP Host도 별도의 공개 호스트 설정이 아닌 바인드 설정을 바탕으로 검증합니다. 공개 HTTPS 접근, Host 처리, 인증, 도구 조회, 안전한 시험 호출을 각각 확인해야 합니다. 인증 방식이 맞지 않는다고 쓰기 가능한 작업 공간을 인증 없이 공개해서는 안 됩니다.

## 검증 상태

| 확인 항목 | 근거 또는 남은 작업 |
| --- | --- |
| 로컬 stdio 초기화와 도구 호출 | 저장소의 전송·프로토콜 테스트 |
| 로컬 Streamable HTTP 초기화와 도구 호출 | 저장소의 HTTP·프로토콜 테스트 |
| 실제 ChatGPT 계정 연결 | 미검증. 사용할 계정과 배포 환경에서 확인 필요 |
| 공개 HTTPS·프록시 구성 | 루프백 테스트로는 확인되지 않음 |
| ChatGPT의 정적 Bearer 수용 여부 | 확인되지 않았으므로 지원한다고 가정하지 않기 |

저장소가 테스트하는 버전은 [프로토콜 호환성](protocol-compatibility.md)에 있습니다. 그 테스트로 ChatGPT의 협상 버전이나 선택적 MCP 기능의 필요 여부를 추정하지 마세요.

<a id="비밀"></a>
<a id="비밀"></a>

## 인증 정보

토큰을 이슈에 공유하는 명령, 로그, 커밋된 예시에 넣지 마세요. HTTP 인증 실패 응답에는 `{"error":"unauthorized"}`만 포함됩니다. 계정 비밀과 공개 범위는 운영자가 관리하며 이 가이드가 자동으로 설정하지 않습니다.

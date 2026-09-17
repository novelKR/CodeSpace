# 동작 차이

[English](../behavior-differences.md) | [한국어](behavior-differences.md)

제품 정책은 “`codex-apply-patch`가 기본으로 하는 일”이 **아닙니다**.
`crates/patch`는 원본 파서와 적용 함수를 프로세스 내부에서 호출한 뒤,
게이트웨이/러너가 크레이트가 받아들일 수 있는 작업을 거절합니다.

| 주제 | Codex 라이브러리 / 독립 기본값 (0.154 후보) | CodeSpace 제품 |
| --- | --- | --- |
| Paths from the model | 호스트 절대 경로를 받을 수 있음. 프로세스 cwd 기준으로 상대 해석 | **상대 경로만**, 등록된 워크스페이스 루트 안에서 해석 |
| Symlinks | 적용 옵션이 follow / keep going 가능 | 심링크 파일과 심링크 탈출을 **거절** |
| Special files | 제품 게이트가 아님 | 디바이스, 소켓, fifo를 **거절** |
| Add File | 헝크에 따라 기존 경로와 상호작용할 수 있음 | 목적지가 이미 있으면 Add File을 **거절** |
| Move destination | 헝크가 그렇게 말하면 엔진이 적용할 수 있음 | 이동 목적지가 이미 있으면 **거절** |
| Newlines | 여러 모드가 있음. 가정하지 말 것 | **Preserve-newline 모드 선호**. 패리티는 같은 모드 사용 |
| `git apply` | V4A 엔진이 아님. 다른 제품은 가끔 폴백 | **조용한 git-apply 폴백 없음** |
| Sandbox | 독립 `apply_patch`는 sandbox `None` 사용 | 패치 크레이트는 샌드박스가 **아님**. 지금은 게이트웨이 정책 + PathSandbox. Linux 컨테이너가 **목표** |
| Unified diff | 다른 곳의 다른 도구 | MVP 밖(`git_apply_patch`는 나중, 절대 자동 변환하지 않음) |
| Rollback | 크레이트에 N/A | 파일 스냅샷 복원. `git reset --hard` **없음** |

## 패치 요청 계약

```json
{
  "workspace_id": "demo",
  "patch": "*** Begin Patch\\n*** Update File: src/config.ts\\n...",
  "expected_versions": {
    "src/config.ts": "sha256:<from read>"
  },
  "operation_key": "change-timeout-001",
  "check_only": false
}
```

- `expected_versions` 값은 `read`의 내용 버전이거나, 아직 없어야 하는
  파일의 `"absent"`입니다. Move는 출발지와 목적지를 검증합니다.
- `operation_key`는 멱등이지 능력 토큰이 아닙니다.
- `check_only: true`는 모든 대상 파일을 바이트 단위로 그대로 두고
  `status: "checked"`를 반환해야 합니다.
- 성공한 적용은 `files`(경로 목록)와 함께 `path`, `before_version`,
  `after_version`, `kind`(`add` / `update` / `delete` / `move`)를 담은
  `changes`를 반환합니다. `applied`는 헬퍼가 주장한 해시가 새로 읽은
  디스크 해시와 일치한다는 뜻입니다.

결과 `status`: `applied` | `checked` | `rejected` |
`failed_rolled_back` | `failed_partial` | `unknown`.

## 쓰기 잠금

`workspace-write` 셸은 변경하는 점유자입니다. 살아 있는 동안 그
워크스페이스의 다른 변경 패치/exec 작업은 막히거나(`WORKSPACE_BUSY`)
문서화된 큐에 따라 기다립니다. 제품은 셸이 워크스페이스 파일을 지울 수
없다고 가장하지 않습니다.

## 전송

stdio와 Streamable HTTP는 **같은** 도구 스키마를 노출합니다. 선택적 정적
Bearer는 HTTP 실험 전용이며, 실제 계정 검사가 그렇게 말하기 전에는
ChatGPT Custom Connector를 만족한다고 **가정하지 않습니다**.

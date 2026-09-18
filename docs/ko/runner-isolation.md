# 러너 격리

[English](../runner-isolation.md) | [한국어](runner-isolation.md)

**목표** 실행 격리 OS는 Linux 컨테이너입니다. **현재** `exec_command`는
호스트 프로세스입니다(`tokio::process::Command`, 워크스페이스 cwd,
`env_clear`). 게이트웨이 단위 시험은 macOS에서 실행할 수 있습니다. 그것은
개발 노트북에서 Linux 격리를 검증했다는 주장이 아닙니다.

## compose 픽스처가 하는 일

[`deploy/compose.yml`](../../deploy/compose.yml)은 **격리 픽스처**입니다.
비특권 사용자(`uid 10001`)로 실행하고, 워크스페이스만 `/workspace`에
바인드 마운트한 뒤 sleep합니다. `codespace-mcp` / `codespace-patch`를
실어 보내지 않으며 `exec_command`에 **연결되어 있지 않습니다**.

마운트하지 않는 것:

- host `$HOME`
- SSH agent socket
- `/var/run/docker.sock`
- gateway `.env`, Bearer files, or SQLite

지금은 compose 픽스처에 러너 제어 소켓이 없습니다. 이 픽스처에 호스트
Docker 소켓이나 이후 제어 소켓을 실수로 추가하지 마세요. 선택적
`CODESPACE_RUNNER=uds`는 이 픽스처 밖의 **비공개** 게이트웨이↔워커
Unix 소켓을 씁니다. 게이트웨이가 unique 0700 leaf를 만듭니다
(`$TMPDIR/codespace-runner-<pid>-<rand>/` 또는
`$CODESPACE_RUNNER_DIR/run-<pid>-<rand>/`). 소켓은 항상
`$dir/runner.sock`입니다. 살아있는 소켓은 `connect`로 조사하며
`ConnectionRefused` leftover만 unlink합니다. `/tmp` 자체는 chmod하지
않습니다.

## macOS / Docker 없음

`codespace-runner::PathSandbox`는 단위 시험과 `read` / `find` / versions에
같은 상대 경로 + 심링크 + 특수 파일 규칙을 적용합니다. 워크스페이스
인가이지, 레이스에 안전한 I/O가 아닙니다. 파일 바이트, metadata, mkdir,
chmod, remove, 제한된 walk는 격리된 `crates/file-system`(`codespace-fs`,
no-follow `LOCAL_FS`)을 탑니다. PathSandbox의 lstat과 어댑터 open 사이에
살아 있는 프로세스가 트리를 바꿀 수 있습니다. 안전 경계는 no-follow
I/O입니다. Docker를 쓰지
않으면 **Linux 컨테이너 격리는 검증되지 않습니다**. 호스트
seccomp/AppArmor와 Docker Desktop 대 Linux 엔진 차이도 검증되지 않습니다.

## 이후 프로세스 분리

오늘은 기본으로 `codespace-mcp`가 한 프로세스입니다. `crates/runner`가
`InProcessRunner`(`PathSandbox`, `apply_patch` 트랜잭션 하나, 호스트
감독)와 선택적 Unix 소켓 `UdsRunner` 클라이언트를 호스팅합니다.
워커는 격리된 `crates/codex-runtime`(`codespace-codex-runtime`)입니다.
`codex_process_hardening::pre_main_hardening()`은 `main`의 첫 줄로
유지합니다(워커/헬퍼 **프로세스** 강화이지 command sandbox가 아닙니다.
`ctor` 없음). 그다음 `$dir/runner.sock`에 bind만 합니다(부모 chmod
없음). 프로세스당 `InProcessRunner`는 **하나**입니다. 와이어는 **u32
length-prefix + CodeSpace JSON**입니다 (`protocol: 3`, Hello 핸드셰이크,
`request_id` `rrpc-…`, 이벤트에 `ProcessExited`). App Server가 아닙니다.
P0 UDS는 1:1입니다. 게이트웨이가 워커 자식을 소유합니다(`kill_on_drop`).
연결 끊김이나 게이트웨이 종료는 워커와 호스트 자식을 죽입니다.
`process_id`는 살아남지 않으며 재연결은 없습니다. 러너 `Replay`는 같은
연결에서만 동작합니다. 그 **전송**은 구현되어 있으며 선택적입니다
(`CODESPACE_RUNNER=uds` / `CODESPACE_RUNTIME_BIN`). **같은 호스트**이며
Linux 격리를 주장하지 않습니다. 다음 WP는
linux-sandbox / network이며 두 번째 전송 재작성이 아닙니다. 소켓
프리미티브로 `codex-uds`를 선호하세요. Runner RPC는 CodeSpace 계약으로
남습니다.

Linux 격리는 여전히 목표 OS입니다. Landlock, seccomp, PTY 헬퍼, UDS,
파일시스템 역학, 네트워크 격리는 **기본 자체 스택이 아닙니다**. 업스트림
실행 서브그래프를 선호하세요
([codex-reuse.md](codex-reuse.md)). 단계:
process-hardening → PTY → UDS/path → filesystem → linux-sandbox →
network (filesystem은 `crates/file-system`으로 가져옴).
`codex-linux-sandbox`는 컨테이너 옆에 둘 수 있습니다. 그
`codex-core` **dev-dep**는 제품 그래프에서 빼 두세요. `codex-exec`는
거절된 채로 남습니다. `codex-exec-server`는 참고 / 이후 백엔드이며
영구 거절은 아닙니다. 게이트웨이 정책이 유일한 허용 경로입니다. compose
픽스처에 호스트 Docker 소켓이나 이후 제어 소켓을 실수로 마운트하지
마세요.

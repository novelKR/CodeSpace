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

지금은 러너 제어 소켓이 없습니다. 이 픽스처에 호스트 Docker 소켓이나
이후 제어 소켓을 실수로 추가하지 마세요.

## macOS / Docker 없음

`codespace-runner::PathSandbox`는 단위 시험과 `read` / `find` / versions에
같은 상대 경로 + 심링크 + 특수 파일 규칙을 적용합니다. Docker를 쓰지
않으면 **Linux 컨테이너 격리는 검증되지 않습니다**. 호스트
seccomp/AppArmor와 Docker Desktop 대 Linux 엔진 차이도 검증되지 않습니다.

## 이후 프로세스 분리

오늘은 `codespace-mcp`가 한 프로세스입니다. `crates/runner`가 프로세스
내부 `Runner`를 호스팅합니다(`PathSandbox`, `apply_patch` 트랜잭션 하나,
호스트 감독). 이후 Unix 소켓 / `ContainerRunner` 워커는 같은 크레이트에
살며, 양쪽 모두 Rust로 남습니다. 그 **전송** 분리가 다음 구현 WP이며
이 문서가 아닙니다. 소켓 프리미티브로 `codex-uds`를 선호하세요. Runner
RPC는 CodeSpace 계약으로 남습니다.

Linux 격리는 여전히 목표 OS입니다. Landlock, seccomp, PTY 헬퍼, UDS,
파일시스템 역학, 네트워크 격리는 **기본 자체 스택이 아닙니다**. 업스트림
실행 서브그래프를 선호하세요
([codex-reuse.md](codex-reuse.md)). 단계:
process-hardening → PTY → UDS/path → filesystem → linux-sandbox →
network. `codex-linux-sandbox`는 컨테이너 옆에 둘 수 있습니다. 그
`codex-core` **dev-dep**는 제품 그래프에서 빼 두세요. `codex-exec`는
거절된 채로 남습니다. `codex-exec-server`는 참고 / 이후 백엔드이며
영구 거절은 아닙니다. 게이트웨이 정책이 유일한 허용 경로입니다. compose
픽스처에 호스트 Docker 소켓이나 이후 제어 소켓을 실수로 마운트하지
마세요.

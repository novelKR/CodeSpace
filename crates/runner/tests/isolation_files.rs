//! Guards that the published isolation files do not expose the host.

#[test]
fn dockerfile_is_unprivileged_and_workspace_only() {
    let text = include_str!("../../../deploy/Dockerfile");
    assert!(text.contains("USER 10001:10001"), "must drop root");
    assert!(
        !text.contains("docker.sock"),
        "must not mention docker.sock"
    );
    assert!(!text.contains("/root"), "must not copy host root");
}

#[test]
fn compose_does_not_mount_host_secrets() {
    let text = include_str!("../../../deploy/compose.yml");
    assert!(!text.contains("docker.sock"));
    assert!(!text.contains("${HOME}"));
    assert!(!text.contains("$HOME"));
    assert!(!text.contains("/run/host-services/ssh-auth.sock"));
    assert!(!text.contains("SSH_AUTH_SOCK"));
    assert!(!text.contains(".env"));
    assert!(!text.contains(".sqlite"));
    assert!(text.contains("target: /workspace"));
    assert!(text.contains("user: \"10001:10001\""));
    assert!(text.contains("no-new-privileges:true"));
}

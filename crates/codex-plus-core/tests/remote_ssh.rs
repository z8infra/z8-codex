use codex_plus_core::remote_ssh::*;
use serde_json::json;

#[test]
fn resolve_ssh_target_from_global_state_for_codex_managed_connection() {
    let state = json!({
        "codex-managed-remote-connections": [{
            "hostId": "remote-ssh-codex-managed:remote",
            "displayName": "remote",
            "source": "codex-managed",
            "hostname": "longnv@192.168.100.31",
            "sshPort": null,
        }]
    });

    let target =
        resolve_ssh_target_from_global_state(&state, "remote-ssh-codex-managed:remote")
            .unwrap();

    assert_eq!(
        target,
        SshTarget {
            user: "longnv".to_string(),
            host: "192.168.100.31".to_string(),
            port: None,
        }
    );
}

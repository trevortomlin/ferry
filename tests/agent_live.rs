// Talks to whatever ssh-agent the environment provides.
//
// Skipped when there is no agent, so it is safe in CI and on a bare machine.
// To exercise it against the project's test key:
//
//   eval $(ssh-agent -s) && ssh-add testing/keys/id_ed25519
//   cargo test -p ferry-creds -- --nocapture

use ferry::secrets::agent::{self, AgentError};

#[test]
fn identities_round_trip_through_a_real_agent() {
    if std::env::var_os("SSH_AUTH_SOCK").is_none() {
        eprintln!("no SSH_AUTH_SOCK; skipping");
        assert_eq!(agent::identities(), Err(AgentError::NotRunning));
        return;
    }

    let keys = agent::identities().expect("a running agent should answer");
    eprintln!("agent holds {} key(s)", keys.len());
    for key in &keys {
        eprintln!("  {}", key.describe());
        assert!(key.algorithm.starts_with("ssh-") || key.algorithm.starts_with("ecdsa"));
        assert!(!key.describe().is_empty());
    }
}

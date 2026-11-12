// Credentials: where secrets are kept, and how a saved connection becomes
// something a protocol can authenticate with.
//
// The point of this crate is that `vfs` never reads the keychain and never
// parses a connection. It is handed a `Credentials` and uses it. Adding FTP or
// Azure later means adding a variant here and a match arm there — no new
// storage code, no new UI for secrets.

pub mod agent;
pub mod keychain;
pub mod secret;
pub mod vault;

use crate::model::connection::{Auth, Connection, Protocol};

pub use secret::Secret;
pub use vault::{SecretRef, Slot, Vault};

/// Everything a provider needs to authenticate, with the secret already
/// fetched. Deliberately not tied to any one SSH or AWS library.
#[derive(Debug)]
pub enum Credentials {
    /// Let ssh-agent do the signing.
    SshAgent {
        username: String,
    },
    SshPassword {
        username: String,
        password: Secret,
    },
    SshKeyFile {
        username: String,
        path: String,
        /// Absent when the key is not encrypted.
        passphrase: Option<Secret>,
    },
    /// Let the AWS SDK's own chain resolve it: environment, `~/.aws`, or SSO.
    AwsProfile {
        profile: String,
    },
    AwsKeys {
        access_key_id: String,
        secret_access_key: Secret,
    },
}

/// Why a connection cannot be opened yet. The caller turns this into a prompt
/// rather than an error dialog: a missing password is a normal state, not a
/// failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Needs {
    /// The keychain holds nothing for this connection. `prompt` is what to ask.
    Secret { prompt: &'static str },
}

/// Combine a saved connection with the secret fetched for it.
///
/// `secret` is whatever `Vault::load` produced for `SecretRef::for_connection`.
/// Auth methods that delegate elsewhere ignore it entirely.
pub fn assemble(connection: &Connection, secret: Option<Secret>) -> Result<Credentials, Needs> {
    let username = connection.username.clone();
    match &connection.auth {
        Auth::Agent => Ok(Credentials::SshAgent { username }),

        Auth::Password => match secret {
            Some(password) if !password.is_empty() => {
                Ok(Credentials::SshPassword { username, password })
            }
            _ => Err(Needs::Secret { prompt: "Password" }),
        },

        // An empty passphrase is meaningful here: it means the key is not
        // encrypted, which is the common case. Absent and empty are the same
        // thing, and neither should block the connection.
        Auth::KeyFile { path } => Ok(Credentials::SshKeyFile {
            username,
            path: path.clone(),
            passphrase: secret.filter(|s| !s.is_empty()),
        }),

        Auth::AwsProfile { profile } => Ok(Credentials::AwsProfile {
            profile: if profile.is_empty() {
                "default".to_owned()
            } else {
                profile.clone()
            },
        }),

        Auth::AwsKeys { access_key_id } => match secret {
            Some(secret_access_key) if !secret_access_key.is_empty() => Ok(Credentials::AwsKeys {
                access_key_id: access_key_id.clone(),
                secret_access_key,
            }),
            _ => Err(Needs::Secret {
                prompt: "Secret access key",
            }),
        },
    }
}

/// Whether this auth method has a secret worth asking the user for. Drives
/// whether the editor shows a password field.
pub fn takes_secret(auth: &Auth) -> bool {
    matches!(
        auth,
        Auth::Password | Auth::KeyFile { .. } | Auth::AwsKeys { .. }
    )
}

/// Whether the SSH agent is relevant to this connection at all.
pub fn uses_agent(connection: &Connection) -> bool {
    connection.protocol == Protocol::Sftp && matches!(connection.auth, Auth::Agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::connection::ConnectionId;

    fn sftp(auth: Auth) -> Connection {
        let mut connection = Connection::new(ConnectionId(1), Protocol::Sftp);
        connection.username = "ferry".to_owned();
        connection.auth = auth;
        connection
    }

    #[test]
    fn agent_auth_needs_nothing_from_the_keychain() {
        let credentials = assemble(&sftp(Auth::Agent), None).unwrap();
        assert!(matches!(credentials, Credentials::SshAgent { username } if username == "ferry"));
    }

    #[test]
    fn password_auth_without_a_stored_secret_asks_for_one() {
        assert_eq!(
            assemble(&sftp(Auth::Password), None).unwrap_err(),
            Needs::Secret { prompt: "Password" }
        );
        // An empty stored value is the same as none — a blank password should
        // not be silently attempted.
        assert!(assemble(&sftp(Auth::Password), Some(Secret::default())).is_err());
    }

    #[test]
    fn password_auth_with_a_stored_secret_is_ready() {
        let credentials = assemble(&sftp(Auth::Password), Some(Secret::new("hunter2"))).unwrap();
        match credentials {
            Credentials::SshPassword { password, .. } => assert_eq!(password.expose(), "hunter2"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unencrypted_key_file_does_not_need_a_passphrase() {
        let connection = sftp(Auth::KeyFile {
            path: "/home/ferry/.ssh/id_ed25519".to_owned(),
        });

        let credentials = assemble(&connection, None).unwrap();
        assert!(matches!(
            credentials,
            Credentials::SshKeyFile {
                passphrase: None,
                ..
            }
        ));

        // Stored-but-empty must also mean "no passphrase", not "prompt".
        let credentials = assemble(&connection, Some(Secret::default())).unwrap();
        assert!(matches!(
            credentials,
            Credentials::SshKeyFile {
                passphrase: None,
                ..
            }
        ));
    }

    #[test]
    fn an_encrypted_key_file_carries_its_passphrase() {
        let connection = sftp(Auth::KeyFile {
            path: "/k".to_owned(),
        });
        match assemble(&connection, Some(Secret::new("open sesame"))).unwrap() {
            Credentials::SshKeyFile {
                passphrase, path, ..
            } => {
                assert_eq!(passphrase.unwrap().expose(), "open sesame");
                assert_eq!(path, "/k");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_empty_aws_profile_falls_back_to_default() {
        let mut connection = Connection::new(ConnectionId(2), Protocol::S3);
        connection.auth = Auth::AwsProfile {
            profile: String::new(),
        };
        match assemble(&connection, None).unwrap() {
            Credentials::AwsProfile { profile } => assert_eq!(profile, "default"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn aws_keys_need_the_secret_half() {
        let mut connection = Connection::new(ConnectionId(2), Protocol::S3);
        connection.auth = Auth::AwsKeys {
            access_key_id: "AKIA".to_owned(),
        };
        assert_eq!(
            assemble(&connection, None).unwrap_err(),
            Needs::Secret {
                prompt: "Secret access key"
            }
        );

        match assemble(&connection, Some(Secret::new("shhh"))).unwrap() {
            Credentials::AwsKeys {
                access_key_id,
                secret_access_key,
            } => {
                assert_eq!(access_key_id, "AKIA");
                assert_eq!(secret_access_key.expose(), "shhh");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn only_secret_bearing_methods_report_taking_one() {
        assert!(!takes_secret(&Auth::Agent));
        assert!(takes_secret(&Auth::Password));
        assert!(takes_secret(&Auth::KeyFile {
            path: String::new()
        }));
        assert!(!takes_secret(&Auth::AwsProfile {
            profile: String::new()
        }));
        assert!(takes_secret(&Auth::AwsKeys {
            access_key_id: String::new()
        }));
    }

    #[test]
    fn the_agent_is_only_relevant_to_sftp() {
        assert!(uses_agent(&sftp(Auth::Agent)));
        assert!(!uses_agent(&sftp(Auth::Password)));

        let mut s3 = Connection::new(ConnectionId(3), Protocol::S3);
        s3.auth = Auth::Agent;
        assert!(!uses_agent(&s3));
    }
}

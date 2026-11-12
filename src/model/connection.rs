// A saved connection.
//
// Deliberately one flat struct rather than a per-protocol enum: the fields
// overlap heavily, the UI wants a single form, and serialising a flat struct
// keeps the config file readable by a human. Fields that do not apply to a
// protocol are simply empty.
//
// No secret is ever stored here. `Auth` names *which* secret to look up; the
// value lives in the OS keychain under the connection's id (milestone 3).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Sftp,
    S3,
}

impl Protocol {
    pub const ALL: [Self; 2] = [Self::Sftp, Self::S3];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sftp => "SFTP",
            Self::S3 => "Amazon S3",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Sftp => 22,
            Self::S3 => 443,
        }
    }
}

/// How to authenticate. Which variants are valid depends on the protocol; the
/// editor only offers the applicable ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Auth {
    /// SFTP via ssh-agent. No secret of ours to store.
    Agent,
    /// SFTP password, held in the keychain.
    Password,
    /// SFTP private key file; its passphrase, if any, is in the keychain.
    KeyFile { path: String },
    /// S3 credentials resolved by the AWS SDK's own chain (env, ~/.aws, SSO).
    AwsProfile { profile: String },
    /// S3 access key id + secret key, both in the keychain.
    AwsKeys { access_key_id: String },
}

impl Auth {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Agent => "SSH agent",
            Self::Password => "Password",
            Self::KeyFile { .. } => "Key file",
            Self::AwsProfile { .. } => "AWS profile",
            Self::AwsKeys { .. } => "Access keys",
        }
    }

    pub fn options_for(protocol: Protocol) -> &'static [fn() -> Auth] {
        match protocol {
            Protocol::Sftp => &[
                || Auth::Agent,
                || Auth::Password,
                || Auth::KeyFile {
                    path: String::new(),
                },
            ],
            Protocol::S3 => &[
                || Auth::AwsProfile {
                    profile: "default".to_owned(),
                },
                || Auth::AwsKeys {
                    access_key_id: String::new(),
                },
            ],
        }
    }
}

/// Stable across restarts and used as the keychain lookup key, so it must not
/// be a positional index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ConnectionId(pub u64);

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub id: ConnectionId,
    pub name: String,
    pub protocol: Protocol,
    /// Hostname for SFTP. For S3 this is an optional custom endpoint; empty
    /// means real AWS.
    #[serde(default)]
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: String,
    pub auth: Auth,
    /// S3 only.
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub region: String,
    /// Directory to open on connect. Empty means the protocol's default.
    #[serde(default)]
    pub initial_path: String,
}

impl Connection {
    pub fn new(id: ConnectionId, protocol: Protocol) -> Self {
        Self {
            id,
            name: String::new(),
            protocol,
            host: String::new(),
            port: protocol.default_port(),
            username: String::new(),
            auth: match protocol {
                Protocol::Sftp => Auth::Agent,
                Protocol::S3 => Auth::AwsProfile {
                    profile: "default".to_owned(),
                },
            },
            bucket: String::new(),
            region: if protocol == Protocol::S3 {
                "us-east-1".to_owned()
            } else {
                String::new()
            },
            initial_path: String::new(),
        }
    }

    /// What the sidebar shows under the name.
    pub fn summary(&self) -> String {
        match self.protocol {
            Protocol::Sftp => {
                if self.username.is_empty() {
                    format!("{}:{}", self.host, self.port)
                } else {
                    format!("{}@{}:{}", self.username, self.host, self.port)
                }
            }
            Protocol::S3 => {
                if self.host.is_empty() {
                    format!("s3://{} · {}", self.bucket, self.region)
                } else {
                    format!("{}/{}", self.host.trim_end_matches('/'), self.bucket)
                }
            }
        }
    }

    /// Reasons this connection cannot be used yet, for inline editor feedback.
    /// Returns an empty slice when the connection is usable.
    pub fn problems(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.name.trim().is_empty() {
            out.push("Name is required");
        }
        match self.protocol {
            Protocol::Sftp => {
                if self.host.trim().is_empty() {
                    out.push("Hostname is required");
                }
                if self.username.trim().is_empty() {
                    out.push("Username is required");
                }
                if let Auth::KeyFile { path } = &self.auth
                    && path.trim().is_empty()
                {
                    out.push("Key file path is required");
                }
            }
            Protocol::S3 => {
                if self.bucket.trim().is_empty() {
                    out.push("Bucket is required");
                }
                if self.region.trim().is_empty() {
                    out.push("Region is required");
                }
                if let Auth::AwsKeys { access_key_id } = &self.auth
                    && access_key_id.trim().is_empty()
                {
                    out.push("Access key id is required");
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sftp() -> Connection {
        let mut c = Connection::new(ConnectionId(1), Protocol::Sftp);
        c.name = "Box".into();
        c.host = "example.com".into();
        c.username = "ferry".into();
        c
    }

    #[test]
    fn new_connection_takes_protocol_defaults() {
        let c = Connection::new(ConnectionId(7), Protocol::Sftp);
        assert_eq!(c.port, 22);
        assert_eq!(c.auth, Auth::Agent);

        let s3 = Connection::new(ConnectionId(8), Protocol::S3);
        assert_eq!(s3.region, "us-east-1");
    }

    #[test]
    fn validation_reports_every_missing_field() {
        let empty = Connection::new(ConnectionId(1), Protocol::Sftp);
        let problems = empty.problems();
        assert!(problems.contains(&"Name is required"));
        assert!(problems.contains(&"Hostname is required"));
        assert!(problems.contains(&"Username is required"));

        assert!(sftp().problems().is_empty());
    }

    #[test]
    fn key_file_auth_requires_a_path() {
        let mut c = sftp();
        c.auth = Auth::KeyFile {
            path: String::new(),
        };
        assert_eq!(c.problems(), ["Key file path is required"]);

        c.auth = Auth::KeyFile {
            path: "~/.ssh/id_ed25519".into(),
        };
        assert!(c.problems().is_empty());
    }

    #[test]
    fn summary_omits_empty_username() {
        let mut c = sftp();
        assert_eq!(c.summary(), "ferry@example.com:22");
        c.username.clear();
        assert_eq!(c.summary(), "example.com:22");
    }
}

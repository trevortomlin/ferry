// Where saved connections live on disk.
//
// A TOML file the user can read, diff, and hand-edit. No secrets in it — see
// `connection::Auth`. The path is derived by hand rather than pulling in a
// crate, because the rule is four lines per platform.

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::connection::Connection;

const APP_DIR: &str = "ferry";
const FILE_NAME: &str = "connections.toml";

/// `~/Library/Application Support/ferry` on macOS,
/// `$XDG_CONFIG_HOME/ferry` (or `~/.config/...`) elsewhere.
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support"))
            .map(|base| base.join(APP_DIR))
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|base| base.join(APP_DIR))
    }
}

pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(FILE_NAME))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Document {
    #[serde(default, rename = "connection")]
    connections: Vec<Connection>,
}

/// Never fails: a missing file is an empty list, and an unreadable or malformed
/// one is logged and treated as empty rather than blocking startup. The file is
/// left untouched in that case so the user can repair it.
pub fn load() -> Vec<Connection> {
    let Some(path) = config_path() else {
        tracing::warn!("no home directory; saved connections are disabled");
        return Vec::new();
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not read connections");
            return Vec::new();
        }
    };

    match toml::from_str::<Document>(&text) {
        Ok(document) => document.connections,
        Err(error) => {
            tracing::error!(path = %path.display(), %error, "connections file is malformed; leaving it alone");
            Vec::new()
        }
    }
}

/// Written to a sibling temporary file and renamed, so an interrupted save
/// cannot leave a half-written config behind.
pub fn save(connections: &[Connection]) -> io::Result<()> {
    let path = config_path()
        .ok_or_else(|| io::Error::other("no home directory; cannot save connections"))?;
    let dir = path.parent().expect("config path always has a parent");
    std::fs::create_dir_all(dir)?;

    let document = Document {
        connections: connections.to_vec(),
    };
    let text = toml::to_string_pretty(&document).map_err(io::Error::other)?;

    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::connection::{Auth, ConnectionId, Protocol};

    #[test]
    fn connections_survive_a_round_trip_through_toml() {
        let mut sftp = Connection::new(ConnectionId(0x1234), Protocol::Sftp);
        sftp.name = "Test box".into();
        sftp.host = "127.0.0.1".into();
        sftp.port = 2222;
        sftp.username = "ferry".into();
        sftp.auth = Auth::KeyFile {
            path: "/home/ferry/.ssh/id_ed25519".into(),
        };

        let mut s3 = Connection::new(ConnectionId(0x5678), Protocol::S3);
        s3.name = "MinIO".into();
        s3.host = "http://127.0.0.1:9000".into();
        s3.bucket = "ferry-test".into();
        s3.auth = Auth::AwsKeys {
            access_key_id: "ferryadmin".into(),
        };

        let document = Document {
            connections: vec![sftp.clone(), s3.clone()],
        };
        let text = toml::to_string_pretty(&document).unwrap();
        let parsed: Document = toml::from_str(&text).unwrap();

        assert_eq!(parsed.connections, [sftp, s3]);
    }

    #[test]
    fn an_empty_document_parses() {
        let parsed: Document = toml::from_str("").unwrap();
        assert!(parsed.connections.is_empty());
    }

    #[test]
    fn config_path_ends_where_we_expect() {
        let path = config_path().expect("test environment has a home directory");
        assert!(path.ends_with("ferry/connections.toml"));
        assert!(path.is_absolute());
    }
}

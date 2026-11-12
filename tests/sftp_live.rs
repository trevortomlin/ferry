// Talks to the SFTP server in `testing/`.
//
//   testing/up.sh && testing/seed.sh 1000
//   cargo test -p ferry-vfs -- --nocapture
//
// Skipped when nothing is listening on 127.0.0.1:2222, so a machine without the
// containers running still gets a green build.

use ferry::model::connection::{Connection, ConnectionId, Protocol};
use ferry::model::entries::{Entries, Kind};
use ferry::secrets::{Credentials, Secret};
use ferry::remote::{Chunks, Collect, Provider, VfsError};

const HOST: &str = "127.0.0.1";
const PORT: u16 = 2222;

fn server_is_up() -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("{HOST}:{PORT}").parse().unwrap(),
        std::time::Duration::from_millis(500),
    )
    .is_ok()
}

fn connection() -> Connection {
    let mut connection = Connection::new(ConnectionId(1), Protocol::Sftp);
    connection.host = HOST.to_owned();
    connection.port = PORT;
    connection.username = "ferry".to_owned();
    connection
}

fn key_path() -> String {
    // The workspace root is two levels up from this crate.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/vfs sits two levels below the workspace root");
    root.join("testing/keys/id_ed25519")
        .to_string_lossy()
        .into_owned()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn with_key() -> Credentials {
    Credentials::SshKeyFile {
        username: "ferry".to_owned(),
        path: key_path(),
        passphrase: None,
    }
}

macro_rules! skip_unless_up {
    () => {
        if !server_is_up() {
            eprintln!("no SFTP server on {HOST}:{PORT}; run testing/up.sh — skipping");
            return;
        }
    };
}

#[test]
fn key_authentication_and_a_directory_listing() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), with_key())
            .await
            .expect("key auth should succeed");

        let home = provider.home().await.expect("realpath . should work");
        eprintln!("home = {home}");

        let mut collected = Collect::default();
        provider.list("/upload", &mut collected).await.unwrap();
        assert!(collected.finished, "the listing should have completed");

        let names: Vec<_> = (0..collected.entries.len())
            .map(|i| collected.entries.name(i).to_owned())
            .collect();
        eprintln!("/upload = {names:?}");
        assert!(names.contains(&"big".to_owned()));
        assert!(names.contains(&"mixed".to_owned()));

        // Seeded directories must be reported as directories.
        let big = (0..collected.entries.len())
            .find(|&i| collected.entries.name(i) == "big")
            .unwrap();
        assert_eq!(collected.entries.kind(big), Kind::Directory);

        // `.` and `..` are navigation, not content.
        assert!(!names.iter().any(|name| name == "." || name == ".."));
    });
}

#[test]
fn password_authentication_works_too() {
    skip_unless_up!();
    runtime().block_on(async {
        let credentials = Credentials::SshPassword {
            username: "ferry".to_owned(),
            password: Secret::new("ferry"),
        };
        Provider::connect(&connection(), credentials)
            .await
            .expect("password auth should succeed");
    });
}

#[test]
fn a_wrong_password_is_reported_as_an_auth_failure() {
    skip_unless_up!();
    runtime().block_on(async {
        let credentials = Credentials::SshPassword {
            username: "ferry".to_owned(),
            password: Secret::new("definitely-not-the-password"),
        };
        match Provider::connect(&connection(), credentials).await {
            Err(VfsError::Auth(message)) => {
                eprintln!("auth error: {message}");
                assert!(message.contains("ferry"), "the message should name the user");
            }
            Err(other) => panic!("expected an auth failure, got {other}"),
            Ok(_) => panic!("a wrong password should not authenticate"),
        }
    });
}

#[test]
fn a_missing_directory_is_reported_as_not_found() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), with_key()).await.unwrap();
        let mut collected = Collect::default();
        match provider.list("/upload/no-such-place", &mut collected).await {
            Err(VfsError::NotFound(path)) => assert!(path.contains("no-such-place")),
            Err(other) => panic!("expected NotFound, got {other}"),
            Ok(()) => panic!("listing a missing directory should fail"),
        }
    });
}

#[test]
fn a_large_directory_arrives_in_chunks_and_can_be_stopped_early() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), with_key()).await.unwrap();

        // First: how many entries are actually there?
        let mut everything = Collect::default();
        provider.list("/upload/big", &mut everything).await.unwrap();
        let total = everything.entries.len();
        if total < 600 {
            eprintln!("only {total} seeded entries; run testing/seed.sh 1000 — skipping");
            return;
        }
        eprintln!(
            "/upload/big has {total} entries in {} chunk(s)",
            everything.chunks
        );
        assert!(
            everything.chunks > 1,
            "a directory this size should arrive in more than one chunk"
        );

        // Now stop after the first chunk, the way navigating away does.
        struct StopEarly {
            entries: Entries,
            chunks: usize,
        }
        impl Chunks for StopEarly {
            fn push(&mut self, entries: Entries, _last: bool) -> bool {
                self.entries.append(&entries);
                self.chunks += 1;
                false
            }
        }

        let mut stopped = StopEarly {
            entries: Entries::new(),
            chunks: 0,
        };
        provider.list("/upload/big", &mut stopped).await.unwrap();
        assert_eq!(stopped.chunks, 1, "the provider should have stopped asking");
        assert!(
            stopped.entries.len() < total,
            "stopping early should have read less than the whole directory"
        );

        // The session must still be usable after an abandoned listing.
        let mut after = Collect::default();
        provider.list("/upload", &mut after).await.unwrap();
        assert!(after.finished);
    });
}

#[test]
fn a_hundred_thousand_entries_stay_small_in_memory() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), with_key()).await.unwrap();
        let mut collected = Collect::default();
        provider.list("/upload/big", &mut collected).await.unwrap();

        let entries = &collected.entries;
        let bytes = entries.heap_bytes();
        eprintln!(
            "{} entries in {} KiB ({:.1} bytes/entry)",
            entries.len(),
            bytes / 1024,
            bytes as f64 / entries.len().max(1) as f64
        );
        // Names are ~16 bytes plus 33 bytes of columns; anything near 200
        // bytes an entry means we have started allocating per row.
        assert!(bytes / entries.len().max(1) < 200);
    });
}

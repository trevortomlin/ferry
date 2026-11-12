// Transfers and file operations against the SFTP server in `testing/`.
//
//   testing/up.sh
//   cargo test -p ferry-vfs --test sftp_transfer -- --nocapture
//
// Skipped when nothing is listening on 127.0.0.1:2222. Every test works inside
// its own directory under /upload and removes it afterwards, so runs do not
// interfere with each other or with the seeded data.

use std::path::Path;

use ferry::model::connection::{Connection, ConnectionId, Protocol};
use ferry::secrets::Credentials;
use ferry::remote::{Collect, Finished, Ignore, Progress, Provider, VfsError};

const HOST: &str = "127.0.0.1";
const PORT: u16 = 2222;

fn server_is_up() -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("{HOST}:{PORT}").parse().unwrap(),
        std::time::Duration::from_millis(500),
    )
    .is_ok()
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/vfs sits two levels below the workspace root")
}

fn connection() -> Connection {
    let mut connection = Connection::new(ConnectionId(1), Protocol::Sftp);
    connection.host = HOST.to_owned();
    connection.port = PORT;
    connection.username = "ferry".to_owned();
    connection
}

fn credentials() -> Credentials {
    Credentials::SshKeyFile {
        username: "ferry".to_owned(),
        path: workspace_root()
            .join("testing/keys/id_ed25519")
            .to_string_lossy()
            .into_owned(),
        passphrase: None,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Counts bytes, and optionally stops after a threshold.
struct Counter {
    seen: u64,
    stop_after: Option<u64>,
}

impl Counter {
    fn new() -> Self {
        Self {
            seen: 0,
            stop_after: None,
        }
    }
    fn stopping_after(bytes: u64) -> Self {
        Self {
            seen: 0,
            stop_after: Some(bytes),
        }
    }
}

impl Progress for Counter {
    fn advance(&mut self, bytes: u64) -> bool {
        self.seen += bytes;
        self.stop_after.is_none_or(|limit| self.seen < limit)
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

/// A scratch directory on the server, removed however the test ends.
async fn scratch(provider: &mut Provider, name: &str) -> String {
    let remote = format!("/upload/test-{name}");
    let _ = provider.remove(&remote, true).await;
    provider
        .create_dir(&remote)
        .await
        .expect("should be able to create a scratch directory");
    remote
}

#[test]
fn a_file_uploads_and_comes_back_byte_for_byte() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let remote_dir = scratch(&mut provider, "roundtrip").await;

        // Larger than one 32 KiB block, and not a multiple of it.
        let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let local = std::env::temp_dir().join("ferry-upload.bin");
        std::fs::write(&local, &payload).unwrap();

        let remote_file = format!("{remote_dir}/payload.bin");
        let mut up = Counter::new();
        assert_eq!(
            provider
                .upload(&local, &remote_file, 0, &mut up)
                .await
                .unwrap(),
            Finished::Complete
        );
        assert_eq!(up.seen, payload.len() as u64);
        assert_eq!(provider.size_of(&remote_file).await.unwrap(), 100_000);

        let back = std::env::temp_dir().join("ferry-download.bin");
        let _ = std::fs::remove_file(&back);
        let mut down = Counter::new();
        assert_eq!(
            provider
                .download(&remote_file, &back, 0, &mut down)
                .await
                .unwrap(),
            Finished::Complete
        );
        assert_eq!(down.seen, payload.len() as u64);
        assert_eq!(std::fs::read(&back).unwrap(), payload);

        let _ = std::fs::remove_file(&local);
        let _ = std::fs::remove_file(&back);
        provider.remove(&remote_dir, true).await.unwrap();
    });
}

#[test]
fn an_empty_file_round_trips() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let remote_dir = scratch(&mut provider, "empty").await;

        let local = std::env::temp_dir().join("ferry-empty.bin");
        std::fs::write(&local, b"").unwrap();
        let remote_file = format!("{remote_dir}/empty.bin");

        provider
            .upload(&local, &remote_file, 0, &mut Ignore)
            .await
            .unwrap();
        assert_eq!(provider.size_of(&remote_file).await.unwrap(), 0);

        let back = std::env::temp_dir().join("ferry-empty-back.bin");
        let _ = std::fs::remove_file(&back);
        provider
            .download(&remote_file, &back, 0, &mut Ignore)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&back).unwrap(), Vec::<u8>::new());

        let _ = std::fs::remove_file(&local);
        let _ = std::fs::remove_file(&back);
        provider.remove(&remote_dir, true).await.unwrap();
    });
}

#[test]
fn a_cancelled_download_can_be_resumed_to_completion() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let remote_dir = scratch(&mut provider, "cancel-down").await;

        let payload = vec![7u8; 400_000];
        let local = std::env::temp_dir().join("ferry-cancel-src.bin");
        std::fs::write(&local, &payload).unwrap();
        let remote_file = format!("{remote_dir}/big.bin");
        provider
            .upload(&local, &remote_file, 0, &mut Ignore)
            .await
            .unwrap();

        let target = std::env::temp_dir().join("ferry-cancelled.bin");
        let _ = std::fs::remove_file(&target);
        let mut stopping = Counter::stopping_after(64 * 1024);
        assert_eq!(
            provider
                .download(&remote_file, &target, 0, &mut stopping)
                .await
                .unwrap(),
            Finished::Cancelled
        );

        // The destination must not exist — nothing may look complete. The
        // partial must, because that is what a resume continues from.
        assert!(
            !target.exists(),
            "a cancelled download left a finished-looking file"
        );
        let partial = target.with_file_name("ferry-cancelled.bin.ferry-part");
        let kept = std::fs::metadata(&partial).expect("the partial should be kept for resuming");
        assert!(kept.len() > 0 && kept.len() < payload.len() as u64);

        // Resuming finishes the job and produces the original bytes.
        let mut rest = Counter::new();
        assert_eq!(
            provider
                .download(&remote_file, &target, kept.len(), &mut rest)
                .await
                .unwrap(),
            Finished::Complete
        );
        assert_eq!(std::fs::read(&target).unwrap(), payload);
        assert!(!partial.exists(), "the partial should be gone once renamed");

        let _ = std::fs::remove_file(&local);
        let _ = std::fs::remove_file(&target);
        provider.remove(&remote_dir, true).await.unwrap();
    });
}

#[test]
fn a_cancelled_upload_keeps_a_partial_that_can_be_resumed() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let remote_dir = scratch(&mut provider, "cancel-up").await;

        let local = std::env::temp_dir().join("ferry-cancel-up.bin");
        std::fs::write(&local, vec![3u8; 400_000]).unwrap();
        let remote_file = format!("{remote_dir}/partial.bin");

        let mut stopping = Counter::stopping_after(64 * 1024);
        assert_eq!(
            provider
                .upload(&local, &remote_file, 0, &mut stopping)
                .await
                .unwrap(),
            Finished::Cancelled
        );

        // Nothing may sit under the real name looking complete, but the
        // partial stays so the upload can be resumed.
        let mut listing = Collect::default();
        provider.list(&remote_dir, &mut listing).await.unwrap();
        let names: Vec<_> = (0..listing.entries.len())
            .map(|i| listing.entries.name(i).to_owned())
            .collect();
        assert!(
            !names.contains(&"partial.bin".to_owned()),
            "a truncated upload was left under the real name: {names:?}"
        );
        let at = provider
            .partial_size(&remote_file)
            .await
            .expect("the partial should be kept for resuming");
        assert!(at > 0 && at < 400_000);

        // Resuming completes it, and the finished file appears under the real
        // name with the right size.
        let mut rest = Counter::new();
        assert_eq!(
            provider
                .upload(&local, &remote_file, at, &mut rest)
                .await
                .unwrap(),
            Finished::Complete
        );
        assert_eq!(provider.size_of(&remote_file).await.unwrap(), 400_000);
        assert_eq!(provider.partial_size(&remote_file).await, None);

        let _ = std::fs::remove_file(&local);
        provider.remove(&remote_dir, true).await.unwrap();
    });
}

#[test]
fn create_rename_and_delete() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let remote_dir = scratch(&mut provider, "ops").await;

        provider
            .create_dir(&format!("{remote_dir}/before"))
            .await
            .unwrap();
        provider
            .rename(
                &format!("{remote_dir}/before"),
                &format!("{remote_dir}/after"),
            )
            .await
            .unwrap();

        let mut listing = Collect::default();
        provider.list(&remote_dir, &mut listing).await.unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries.name(0), "after");

        provider
            .remove(&format!("{remote_dir}/after"), true)
            .await
            .unwrap();
        let mut empty = Collect::default();
        provider.list(&remote_dir, &mut empty).await.unwrap();
        assert_eq!(empty.entries.len(), 0);

        provider.remove(&remote_dir, true).await.unwrap();
    });
}

#[test]
fn deleting_a_directory_removes_everything_under_it() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let remote_dir = scratch(&mut provider, "recursive").await;

        // A tree three deep, with files at every level.
        let local = std::env::temp_dir().join("ferry-leaf.txt");
        std::fs::write(&local, b"leaf").unwrap();
        let mut path = remote_dir.clone();
        for level in 0..3 {
            provider
                .upload(&local, &format!("{path}/file-{level}.txt"), 0, &mut Ignore)
                .await
                .unwrap();
            path = format!("{path}/level-{level}");
            provider.create_dir(&path).await.unwrap();
        }
        provider
            .upload(&local, &format!("{path}/deepest.txt"), 0, &mut Ignore)
            .await
            .unwrap();

        provider.remove(&remote_dir, true).await.unwrap();

        // The whole tree must be gone, not merely emptied.
        let mut parent = Collect::default();
        provider.list("/upload", &mut parent).await.unwrap();
        let names: Vec<_> = (0..parent.entries.len())
            .map(|i| parent.entries.name(i))
            .collect();
        assert!(
            !names.contains(&"test-recursive"),
            "the tree survived: {names:?}"
        );

        let _ = std::fs::remove_file(&local);
    });
}

#[test]
fn downloading_something_that_is_not_there_reports_not_found() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let target = std::env::temp_dir().join("ferry-never.bin");
        let _ = std::fs::remove_file(&target);

        match provider
            .download("/upload/no-such-file.bin", &target, 0, &mut Ignore)
            .await
        {
            Err(VfsError::NotFound(path)) => assert!(path.contains("no-such-file")),
            Err(other) => panic!("expected NotFound, got {other}"),
            Ok(_) => panic!("downloading a missing file should fail"),
        }
        assert!(!target.exists());
    });
}

#[test]
fn an_overwrite_after_a_cancelled_upload_starts_clean() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let remote_dir = scratch(&mut provider, "discard").await;

        let local = std::env::temp_dir().join("ferry-discard.bin");
        std::fs::write(&local, vec![9u8; 300_000]).unwrap();
        let remote_file = format!("{remote_dir}/thing.bin");

        let mut stopping = Counter::stopping_after(64 * 1024);
        provider
            .upload(&local, &remote_file, 0, &mut stopping)
            .await
            .unwrap();
        assert!(provider.partial_size(&remote_file).await.is_some());

        // Choosing overwrite must clear the partial, not silently resume it.
        provider.discard_partial(&remote_file).await;
        assert_eq!(provider.partial_size(&remote_file).await, None);

        provider
            .upload(&local, &remote_file, 0, &mut Ignore)
            .await
            .unwrap();
        assert_eq!(provider.size_of(&remote_file).await.unwrap(), 300_000);

        let _ = std::fs::remove_file(&local);
        provider.remove(&remote_dir, true).await.unwrap();
    });
}

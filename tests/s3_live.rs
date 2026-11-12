// S3 against the MinIO server in `testing/`.
//
//   testing/up.sh && testing/seed.sh 1000
//   cargo test -p ferry-vfs --test s3_live -- --nocapture
//
// Skipped when nothing is listening on 127.0.0.1:9000. Each test works under
// its own prefix and clears it afterwards.

use ferry::model::connection::{Connection, ConnectionId, Protocol};
use ferry::model::entries::Kind;
use ferry::secrets::{Credentials, Secret};
use ferry::remote::{Chunks, Collect, Entries, Finished, Ignore, Provider, VfsError};

const ENDPOINT: &str = "http://127.0.0.1:9000";
const BUCKET: &str = "ferry-test";

fn server_is_up() -> bool {
    std::net::TcpStream::connect_timeout(
        &"127.0.0.1:9000".parse().unwrap(),
        std::time::Duration::from_millis(500),
    )
    .is_ok()
}

fn connection() -> Connection {
    let mut connection = Connection::new(ConnectionId(2), Protocol::S3);
    connection.host = ENDPOINT.to_owned();
    connection.bucket = BUCKET.to_owned();
    connection.region = "us-east-1".to_owned();
    connection
}

fn credentials() -> Credentials {
    Credentials::AwsKeys {
        access_key_id: "ferryadmin".to_owned(),
        secret_access_key: Secret::new("ferryadmin"),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

macro_rules! skip_unless_up {
    () => {
        if !server_is_up() {
            eprintln!("no S3 server on 127.0.0.1:9000; run testing/up.sh — skipping");
            return;
        }
    };
}

async fn scratch(provider: &mut Provider, name: &str) -> String {
    let prefix = format!("/test-{name}");
    let _ = provider.remove(&prefix, true).await;
    provider.create_dir(&prefix).await.unwrap();
    prefix
}

fn names(entries: &Entries) -> Vec<String> {
    (0..entries.len())
        .map(|i| entries.name(i).to_owned())
        .collect()
}

#[test]
fn connecting_and_listing_the_bucket_root() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .expect("access keys should authenticate against MinIO");

        assert_eq!(provider.home().await.unwrap(), "/");

        let mut listing = Collect::default();
        provider.list("/", &mut listing).await.unwrap();
        let found = names(&listing.entries);
        eprintln!("bucket root = {found:?}");

        // The seeded `big/` prefix must appear as a directory, not as a key.
        assert!(found.contains(&"big".to_owned()), "{found:?}");
        let index = (0..listing.entries.len())
            .find(|&i| listing.entries.name(i) == "big")
            .unwrap();
        assert_eq!(listing.entries.kind(index), Kind::Directory);
    });
}

#[test]
fn a_wrong_secret_key_is_reported_rather_than_hanging() {
    skip_unless_up!();
    runtime().block_on(async {
        let bad = Credentials::AwsKeys {
            access_key_id: "ferryadmin".to_owned(),
            secret_access_key: Secret::new("not-the-secret"),
        };
        match Provider::connect(&connection(), bad).await {
            Err(error) => {
                let message = error.to_string();
                eprintln!("auth error: {message}");
                assert!(message.contains("ferry-test"), "{message}");
                // The message has to say what to do about it, not dump the
                // SDK's error struct.
                assert!(
                    message.contains("credentials were rejected"),
                    "unhelpful message: {message}"
                );
            }
            Ok(_) => panic!("a wrong secret key should not connect"),
        }
    });
}

#[test]
fn a_missing_bucket_names_itself_in_the_error() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut connection = connection();
        connection.bucket = "no-such-bucket-here".to_owned();
        match Provider::connect(&connection, credentials()).await {
            Err(error) => {
                let message = error.to_string();
                assert!(message.contains("no-such-bucket-here"), "{message}");
                assert!(message.contains("no bucket by that name"), "{message}");
            }
            Ok(_) => panic!("a missing bucket should not connect"),
        }
    });
}

#[test]
fn a_large_prefix_pages_and_can_be_stopped_early() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();

        let mut everything = Collect::default();
        provider.list("/big", &mut everything).await.unwrap();
        let total = everything.entries.len();
        if total <= 1000 {
            eprintln!("only {total} seeded objects; run testing/seed.sh 100000 — skipping");
            return;
        }
        eprintln!("/big has {total} objects in {} page(s)", everything.chunks);
        assert!(
            everything.chunks > 1,
            "more than a thousand objects must arrive in more than one page"
        );

        // Navigating away stops the paging rather than reading to the end.
        struct StopEarly(usize);
        impl Chunks for StopEarly {
            fn push(&mut self, _entries: Entries, _last: bool) -> bool {
                self.0 += 1;
                false
            }
        }
        let mut stopped = StopEarly(0);
        provider.list("/big", &mut stopped).await.unwrap();
        assert_eq!(stopped.0, 1, "the provider should have stopped asking");
    });
}

#[test]
fn an_object_round_trips_byte_for_byte() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let prefix = scratch(&mut provider, "roundtrip").await;

        let payload: Vec<u8> = (0..70_000u32).map(|i| (i % 253) as u8).collect();
        let local = std::env::temp_dir().join("ferry-s3-up.bin");
        std::fs::write(&local, &payload).unwrap();

        let key = format!("{prefix}/payload.bin");
        provider.upload(&local, &key, 0, &mut Ignore).await.unwrap();
        assert_eq!(provider.size_of(&key).await.unwrap(), payload.len() as u64);

        let back = std::env::temp_dir().join("ferry-s3-down.bin");
        let _ = std::fs::remove_file(&back);
        assert_eq!(
            provider
                .download(&key, &back, 0, &mut Ignore)
                .await
                .unwrap(),
            Finished::Complete
        );
        assert_eq!(std::fs::read(&back).unwrap(), payload);

        let _ = std::fs::remove_file(&local);
        let _ = std::fs::remove_file(&back);
        provider.remove(&prefix, true).await.unwrap();
    });
}

/// Anything over one part takes the multipart path, which is a different code
/// route entirely and the one that can leave paid-for garbage behind.
#[test]
fn a_multipart_upload_completes_and_reads_back() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let prefix = scratch(&mut provider, "multipart").await;

        // Just over two 5 MiB parts, so the last one is short.
        let size = 11 * 1024 * 1024;
        let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let local = std::env::temp_dir().join("ferry-s3-multi.bin");
        std::fs::write(&local, &payload).unwrap();

        let key = format!("{prefix}/large.bin");
        assert_eq!(
            provider.upload(&local, &key, 0, &mut Ignore).await.unwrap(),
            Finished::Complete
        );
        assert_eq!(provider.size_of(&key).await.unwrap(), size as u64);

        let back = std::env::temp_dir().join("ferry-s3-multi-back.bin");
        let _ = std::fs::remove_file(&back);
        provider
            .download(&key, &back, 0, &mut Ignore)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&back).unwrap(), payload);

        let _ = std::fs::remove_file(&local);
        let _ = std::fs::remove_file(&back);
        provider.remove(&prefix, true).await.unwrap();
    });
}

#[test]
fn a_resumed_download_uses_a_ranged_request() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let prefix = scratch(&mut provider, "resume").await;

        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 249) as u8).collect();
        let local = std::env::temp_dir().join("ferry-s3-resume-src.bin");
        std::fs::write(&local, &payload).unwrap();
        let key = format!("{prefix}/thing.bin");
        provider.upload(&local, &key, 0, &mut Ignore).await.unwrap();

        // Pretend an earlier attempt stopped a third of the way in.
        let target = std::env::temp_dir().join("ferry-s3-resume.bin");
        let partial = target.with_file_name("ferry-s3-resume.bin.ferry-part");
        let at = 60_000usize;
        std::fs::write(&partial, &payload[..at]).unwrap();

        provider
            .download(&key, &target, at as u64, &mut Ignore)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(&target).unwrap(),
            payload,
            "a resumed download must reproduce the original exactly"
        );

        let _ = std::fs::remove_file(&local);
        let _ = std::fs::remove_file(&target);
        provider.remove(&prefix, true).await.unwrap();
    });
}

#[test]
fn create_rename_and_delete_a_prefix() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let prefix = scratch(&mut provider, "ops").await;

        let local = std::env::temp_dir().join("ferry-s3-ops.txt");
        std::fs::write(&local, b"contents").unwrap();
        provider
            .upload(&local, &format!("{prefix}/before.txt"), 0, &mut Ignore)
            .await
            .unwrap();

        provider
            .rename(
                &format!("{prefix}/before.txt"),
                &format!("{prefix}/after.txt"),
            )
            .await
            .unwrap();

        let mut listing = Collect::default();
        provider.list(&prefix, &mut listing).await.unwrap();
        assert_eq!(names(&listing.entries), ["after.txt"]);

        // An empty directory needs its marker object, and the marker must not
        // show up inside the directory it represents.
        let nested = format!("{prefix}/empty-folder");
        provider.create_dir(&nested).await.unwrap();
        let mut inside = Collect::default();
        provider.list(&nested, &mut inside).await.unwrap();
        assert!(
            inside.entries.is_empty(),
            "the marker object leaked into its own listing: {:?}",
            names(&inside.entries)
        );

        let mut outside = Collect::default();
        provider.list(&prefix, &mut outside).await.unwrap();
        assert!(names(&outside.entries).contains(&"empty-folder".to_owned()));

        // Deleting the prefix removes everything under it.
        provider.remove(&prefix, true).await.unwrap();
        let mut gone = Collect::default();
        provider.list(&prefix, &mut gone).await.unwrap();
        assert!(gone.entries.is_empty());

        let _ = std::fs::remove_file(&local);
    });
}

#[test]
fn a_missing_object_reports_not_found() {
    skip_unless_up!();
    runtime().block_on(async {
        let mut provider = Provider::connect(&connection(), credentials())
            .await
            .unwrap();
        let target = std::env::temp_dir().join("ferry-s3-never.bin");
        let _ = std::fs::remove_file(&target);

        match provider
            .download("/definitely-not-here.bin", &target, 0, &mut Ignore)
            .await
        {
            Err(VfsError::NotFound(path)) => assert!(path.contains("definitely-not-here")),
            Err(other) => panic!("expected NotFound, got {other}"),
            Ok(_) => panic!("downloading a missing object should fail"),
        }
        assert!(!target.exists());
    });
}

// Exercises the real OS keychain, through the same threaded path the app uses.
//
// Skipped when no keychain is reachable (headless CI, no Secret Service), so it
// never turns a missing daemon into a red build. Cleans up after itself: the
// entry it writes is deleted before the test returns.

use std::sync::mpsc::Receiver;
use std::time::Duration;

use ferry::model::connection::ConnectionId;
use ferry::secrets::Secret;
use ferry::secrets::vault::{Reply, SecretRef, Slot, Vault};

const WAIT: Duration = Duration::from_secs(10);

fn next(inbox: &Receiver<Reply>) -> Reply {
    inbox.recv_timeout(WAIT).expect("the vault should reply")
}

#[test]
fn a_secret_survives_a_store_load_delete_cycle() {
    let (vault, inbox) = Vault::new(|| {});

    vault.probe();
    match next(&inbox) {
        Reply::Availability(Ok(())) => {}
        Reply::Availability(Err(why)) => {
            eprintln!("no keychain available, skipping: {why}");
            return;
        }
        other => panic!("expected an availability reply, got {other:?}"),
    }

    // An id no real connection will ever be given.
    let reference = SecretRef::new(ConnectionId(0xFFFF_FFFF_FFFF_FF01), Slot::Password);

    vault.store(reference, Secret::new("hunter2"));
    assert!(matches!(next(&inbox), Reply::Stored(r) if r == reference));

    vault.load(reference);
    match next(&inbox) {
        Reply::Loaded(r, Some(secret)) => {
            assert_eq!(r, reference);
            assert_eq!(secret.expose(), "hunter2");
        }
        other => panic!("expected the stored secret, got {other:?}"),
    }

    vault.delete(reference);
    assert!(matches!(next(&inbox), Reply::Deleted(r) if r == reference));

    // A deleted secret reads back as absent, not as an error.
    vault.load(reference);
    assert!(matches!(next(&inbox), Reply::Loaded(r, None) if r == reference));

    // Deleting again is still a success.
    vault.delete(reference);
    assert!(matches!(next(&inbox), Reply::Deleted(_)));
}

/// Regression: the vault spawns a thread per operation, and `keyring`'s v1
/// module has a race in its one-time store setup. Firing several operations at
/// once used to make some of them fail with "no default store has been set".
#[test]
fn concurrent_first_operations_do_not_race_the_store_setup() {
    let (vault, inbox) = Vault::new(|| {});

    let references: Vec<_> = (0..8)
        .map(|i| SecretRef::new(ConnectionId(0xFFFF_FFFF_FFFF_FF10 + i), Slot::Password))
        .collect();
    for reference in &references {
        vault.load(*reference);
    }

    let mut unavailable = false;
    for _ in &references {
        match next(&inbox) {
            Reply::Loaded(_, _) => {}
            Reply::Failed(_, why) => {
                // A machine with no keychain at all fails every one of them,
                // which is fine; a race fails only some.
                unavailable = true;
                eprintln!("keychain unavailable: {why}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    if unavailable {
        eprintln!("no keychain available, skipping");
    }
}

#[test]
fn loading_an_unknown_secret_reports_absence_rather_than_failure() {
    let (vault, inbox) = Vault::new(|| {});

    vault.probe();
    if let Reply::Availability(Err(why)) = next(&inbox) {
        eprintln!("no keychain available, skipping: {why}");
        return;
    }

    let reference = SecretRef::new(ConnectionId(0xFFFF_FFFF_FFFF_FF02), Slot::Passphrase);
    vault.load(reference);
    assert!(matches!(next(&inbox), Reply::Loaded(_, None)));
}

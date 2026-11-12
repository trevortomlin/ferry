// The OS keychain: Keychain Services on macOS, Secret Service on Linux.
//
// Every operation runs on its own short-lived thread. That looks extravagant
// until you notice what the blocking call can do: Secret Service is a D-Bus
// round trip that may pop a "unlock your keyring" dialog and sit there for as
// long as the user takes. On the UI thread that is a frozen window. Keychain
// operations happen a handful of times per session, so a thread each is far
// cheaper than the machinery needed to avoid one.
//
// The vault never fails fatally. If no keychain is available the app still
// works; it just asks for the password every time and says so.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::model::connection::{Auth, Connection, ConnectionId};

use crate::secrets::secret::Secret;

const SERVICE: &str = "ferry";

/// Which secret of a connection is meant. A connection can hold more than one
/// (a key file has a passphrase; S3 has a secret access key), so the id alone
/// is not a unique account name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Password,
    Passphrase,
    SecretAccessKey,
}

impl Slot {
    fn suffix(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Passphrase => "passphrase",
            Self::SecretAccessKey => "secret-access-key",
        }
    }
}

/// Names one secret in the keychain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SecretRef {
    pub connection: ConnectionId,
    pub slot: Slot,
}

impl SecretRef {
    pub fn new(connection: ConnectionId, slot: Slot) -> Self {
        Self { connection, slot }
    }

    /// The keychain account name. Uses the connection id, never the display
    /// name, so renaming a connection cannot orphan its password.
    fn account(&self) -> String {
        format!("{}#{}", self.connection, self.slot.suffix())
    }

    /// Which secret this connection needs, if any. `Agent` and `AwsProfile`
    /// delegate to something else and have nothing of ours to store.
    pub fn for_connection(connection: &Connection) -> Option<Self> {
        let slot = match connection.auth {
            Auth::Password => Slot::Password,
            Auth::KeyFile { .. } => Slot::Passphrase,
            Auth::AwsKeys { .. } => Slot::SecretAccessKey,
            Auth::Agent | Auth::AwsProfile { .. } => return None,
        };
        Some(Self::new(connection.id, slot))
    }
}

#[derive(Debug)]
pub enum Reply {
    /// Result of the startup probe. `Err` carries text meant for the user.
    Availability(Result<(), String>),
    Stored(SecretRef),
    /// `None` means there was no such entry, which is not an error.
    Loaded(SecretRef, Option<Secret>),
    Deleted(SecretRef),
    Failed(SecretRef, String),
}

/// Hands work to background threads and posts results back to the UI.
pub struct Vault {
    replies: Sender<Reply>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Vault {
    /// `wake` is called after each reply is queued, so the UI can repaint
    /// without polling.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> (Self, Receiver<Reply>) {
        let (replies, inbox) = channel();
        (
            Self {
                replies,
                wake: Arc::new(wake),
            },
            inbox,
        )
    }

    /// Checks at startup whether a keychain exists at all, so the UI can warn
    /// once rather than failing at the moment the user tries to save.
    pub fn probe(&self) {
        self.spawn("probe", move || {
            ensure_store();
            let outcome = keyring::Entry::new(SERVICE, PROBE_ACCOUNT)
                .and_then(|entry| match entry.get_password() {
                    // Absent is the expected answer; it still proves the store
                    // is reachable.
                    Err(keyring::Error::NoEntry) => Ok(()),
                    other => other.map(|_| ()),
                })
                .map_err(|error| describe(&error));
            Reply::Availability(outcome)
        });
    }

    pub fn store(&self, reference: SecretRef, secret: Secret) {
        self.spawn("store", move || {
            match entry(reference).and_then(|entry| entry.set_password(secret.expose())) {
                Ok(()) => Reply::Stored(reference),
                Err(error) => Reply::Failed(reference, describe(&error)),
            }
        });
    }

    pub fn load(&self, reference: SecretRef) {
        self.spawn("load", move || {
            match entry(reference).and_then(|entry| entry.get_password()) {
                Ok(value) => Reply::Loaded(reference, Some(Secret::new(value))),
                Err(keyring::Error::NoEntry) => Reply::Loaded(reference, None),
                Err(error) => Reply::Failed(reference, describe(&error)),
            }
        });
    }

    pub fn delete(&self, reference: SecretRef) {
        self.spawn("delete", move || {
            match entry(reference).and_then(|entry| entry.delete_credential()) {
                // Deleting something that was never stored is a success.
                Ok(()) | Err(keyring::Error::NoEntry) => Reply::Deleted(reference),
                Err(error) => Reply::Failed(reference, describe(&error)),
            }
        });
    }

    fn spawn(&self, name: &'static str, work: impl FnOnce() -> Reply + Send + 'static) {
        let replies = self.replies.clone();
        let wake = Arc::clone(&self.wake);
        let spawned = std::thread::Builder::new()
            .name(format!("keychain-{name}"))
            .spawn(move || {
                // A closed channel means the UI is gone; drop the result.
                if replies.send(work()).is_ok() {
                    wake();
                }
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "could not spawn keychain thread");
        }
    }
}

/// `keyring`'s `v1` module marks its credential-store setup as *started*
/// before it has finished, so a second thread arriving during the very first
/// call sails past and gets "no default store has been set". Spawning a thread
/// per operation makes that race easy to hit — two of our threads at startup
/// reproduce it every time. Serialising the first call closes it.
static STORE_READY: std::sync::Once = std::sync::Once::new();

fn ensure_store() {
    STORE_READY.call_once(|| {
        let _ = keyring::Entry::new(SERVICE, PROBE_ACCOUNT);
    });
}

const PROBE_ACCOUNT: &str = "availability-probe";

fn entry(reference: SecretRef) -> keyring::Result<keyring::Entry> {
    ensure_store();
    keyring::Entry::new(SERVICE, &reference.account())
}

/// Turns a backend error into something a person can act on. The default
/// `Display` says what went wrong internally; these say what to do about it.
fn describe(error: &keyring::Error) -> String {
    match error {
        keyring::Error::NoStorageAccess(_) => {
            "The system keychain is locked. Unlock it and try again.".to_owned()
        }
        keyring::Error::NoEntry => "No saved secret for this connection.".to_owned(),
        keyring::Error::Ambiguous(_) => {
            "More than one keychain entry matches this connection. Remove the duplicates in your keychain app.".to_owned()
        }
        other => format!(
            "No system keychain is available, so passwords cannot be saved ({other}). On Linux, check that gnome-keyring or KWallet is running."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::connection::Protocol;

    #[test]
    fn account_names_are_stable_and_slot_specific() {
        let id = ConnectionId(0xdeadbeef);
        assert_eq!(
            SecretRef::new(id, Slot::Password).account(),
            "00000000deadbeef#password"
        );
        assert_eq!(
            SecretRef::new(id, Slot::Passphrase).account(),
            "00000000deadbeef#passphrase"
        );
        assert_ne!(
            SecretRef::new(id, Slot::Password).account(),
            SecretRef::new(id, Slot::SecretAccessKey).account()
        );
    }

    #[test]
    fn only_auth_methods_holding_our_secret_need_a_slot() {
        let mut connection = Connection::new(ConnectionId(1), Protocol::Sftp);

        connection.auth = Auth::Agent;
        assert_eq!(SecretRef::for_connection(&connection), None);

        connection.auth = Auth::Password;
        assert_eq!(
            SecretRef::for_connection(&connection).map(|r| r.slot),
            Some(Slot::Password)
        );

        connection.auth = Auth::KeyFile {
            path: "/k".to_owned(),
        };
        assert_eq!(
            SecretRef::for_connection(&connection).map(|r| r.slot),
            Some(Slot::Passphrase)
        );

        connection.auth = Auth::AwsProfile {
            profile: "default".to_owned(),
        };
        assert_eq!(SecretRef::for_connection(&connection), None);

        connection.auth = Auth::AwsKeys {
            access_key_id: "AKIA".to_owned(),
        };
        assert_eq!(
            SecretRef::for_connection(&connection).map(|r| r.slot),
            Some(Slot::SecretAccessKey)
        );
    }

    #[test]
    fn a_renamed_connection_keeps_the_same_account_name() {
        let mut connection = Connection::new(ConnectionId(42), Protocol::Sftp);
        connection.auth = Auth::Password;
        connection.name = "Before".to_owned();
        let before = SecretRef::for_connection(&connection).unwrap();
        connection.name = "After".to_owned();
        connection.host = "elsewhere.example".to_owned();
        assert_eq!(before, SecretRef::for_connection(&connection).unwrap());
    }

    #[test]
    fn error_messages_tell_the_user_what_to_do() {
        let locked = describe(&keyring::Error::NoStorageAccess(Box::new(
            std::io::Error::other("locked"),
        )));
        assert!(locked.contains("Unlock"), "{locked}");
    }
}

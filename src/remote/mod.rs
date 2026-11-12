// Remote filesystem providers.
//
// Dispatch is an enum rather than a trait object. Providers are async, and
// `dyn` plus async means boxing a future per call for an interface with two
// implementations and no third-party ones. A new provider is a variant plus a
// match arm — explicit, allocation-free, and the compiler points at every place
// that needs updating.
//
// Nothing here knows about the UI. Listings are handed out through `Chunks` as
// they arrive, and the caller decides where they go and when to stop.

pub mod s3;
pub mod sftp;

use crate::model::connection::Connection;
pub use crate::model::entries::Entries;
use crate::secrets::Credentials;

/// Failures worth telling a person about. Each `Display` says what went wrong
/// and, where there is one, what to do about it.
#[derive(Debug)]
pub enum VfsError {
    /// Could not reach the server at all.
    Unreachable(String),
    /// Reached it, but it would not let us in.
    Auth(String),
    /// The server's host key is not the one we recorded.
    HostKeyChanged {
        fingerprint: String,
        line: usize,
    },
    NotFound(String),
    PermissionDenied(String),
    /// The connection was fine but the operation failed.
    Failed(String),
}

impl std::fmt::Display for VfsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(what) => write!(f, "Could not connect: {what}"),
            Self::Auth(what) => write!(f, "Authentication failed: {what}"),
            Self::HostKeyChanged { fingerprint, line } => write!(
                f,
                "The server's host key has changed (now {fingerprint}). \
                 This could be a different machine, or someone intercepting the connection. \
                 If you trust the change, remove line {line} from ~/.ssh/known_hosts."
            ),
            Self::NotFound(path) => write!(f, "Not found: {path}"),
            Self::PermissionDenied(path) => write!(f, "Permission denied: {path}"),
            Self::Failed(what) => f.write_str(what),
        }
    }
}

/// Progress on a running transfer, and the way to stop one.
///
/// The provider reports bytes as they move and asks whether to keep going. A
/// transfer therefore stops at the next block boundary rather than whenever a
/// cancel happens to be noticed, and never leaves a half-written request on
/// the wire.
pub trait Progress {
    /// Called as bytes move. Return `false` to abandon the transfer.
    fn advance(&mut self, bytes: u64) -> bool;
}

/// Ignores progress and never cancels, for tests and one-shot work.
pub struct Ignore;

impl Progress for Ignore {
    fn advance(&mut self, _bytes: u64) -> bool {
        true
    }
}

/// What a transfer did, since stopping early is not a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finished {
    Complete,
    Cancelled,
}

/// Where a listing goes as it arrives.
///
/// A trait rather than a channel so the provider does not depend on the app's
/// plumbing, and so listing can be tested by collecting into a `Vec`.
pub trait Chunks {
    /// Hand over one batch. `last` marks the end of the listing.
    ///
    /// Return `false` to stop: the caller has navigated away and the rest of
    /// this directory is no longer wanted. This is the cancellation point, and
    /// it is why a 100k-entry listing over a slow link can be abandoned
    /// promptly instead of running to completion.
    fn push(&mut self, entries: Entries, last: bool) -> bool;
}

/// Collects everything, for tests.
#[derive(Debug, Default)]
pub struct Collect {
    pub entries: Entries,
    pub chunks: usize,
    pub finished: bool,
}

impl Chunks for Collect {
    fn push(&mut self, entries: Entries, last: bool) -> bool {
        self.entries.append(&entries);
        self.chunks += 1;
        self.finished = last;
        true
    }
}

pub enum Provider {
    Sftp(sftp::Sftp),
    S3(s3::S3),
}

impl Provider {
    pub async fn connect(
        connection: &Connection,
        credentials: Credentials,
    ) -> Result<Self, VfsError> {
        match credentials {
            Credentials::SshAgent { .. }
            | Credentials::SshPassword { .. }
            | Credentials::SshKeyFile { .. } => Ok(Self::Sftp(
                sftp::Sftp::connect(&connection.host, connection.port, credentials).await?,
            )),
            Credentials::AwsProfile { .. } | Credentials::AwsKeys { .. } => {
                let s3 = s3::S3::connect(
                    &connection.host,
                    &connection.bucket,
                    &connection.region,
                    credentials,
                )
                .await?;
                // Fail now, with a message about the bucket, rather than
                // letting the first listing produce something cryptic.
                s3.check().await?;
                Ok(Self::S3(s3))
            }
        }
    }

    /// Where to open when no initial path is configured.
    pub async fn home(&mut self) -> Result<String, VfsError> {
        match self {
            Self::Sftp(sftp) => sftp.home().await,
            Self::S3(s3) => s3.home().await,
        }
    }

    pub async fn list(&mut self, path: &str, sink: &mut impl Chunks) -> Result<(), VfsError> {
        match self {
            Self::Sftp(sftp) => sftp.list(path, sink).await,
            Self::S3(s3) => s3.list(path, sink).await,
        }
    }

    /// Copy a remote file to `local`. The file is written to a temporary
    /// sibling and renamed on success, so an interrupted download never leaves
    /// something that looks like a complete file.
    pub async fn download(
        &mut self,
        remote: &str,
        local: &std::path::Path,
        resume_from: u64,
        progress: &mut impl Progress,
    ) -> Result<Finished, VfsError> {
        match self {
            Self::Sftp(sftp) => sftp.download(remote, local, resume_from, progress).await,
            Self::S3(s3) => s3.download(remote, local, resume_from, progress).await,
        }
    }

    pub async fn upload(
        &mut self,
        local: &std::path::Path,
        remote: &str,
        resume_from: u64,
        progress: &mut impl Progress,
    ) -> Result<Finished, VfsError> {
        match self {
            Self::Sftp(sftp) => sftp.upload(local, remote, resume_from, progress).await,
            Self::S3(s3) => s3.upload(local, remote, resume_from, progress).await,
        }
    }

    /// Size of an interrupted transfer of `remote`, if one is there.
    pub async fn partial_size(&mut self, remote: &str) -> Option<u64> {
        match self {
            Self::Sftp(sftp) => sftp.partial_size(remote).await,
            Self::S3(s3) => s3.partial_size(remote).await,
        }
    }

    pub async fn discard_partial(&mut self, remote: &str) {
        match self {
            Self::Sftp(sftp) => sftp.discard_partial(remote).await,
            Self::S3(s3) => s3.discard_partial(remote).await,
        }
    }

    /// Size in bytes, for sizing a progress bar before the transfer starts.
    pub async fn size_of(&mut self, remote: &str) -> Result<u64, VfsError> {
        match self {
            Self::Sftp(sftp) => sftp.size_of(remote).await,
            Self::S3(s3) => s3.size_of(remote).await,
        }
    }

    pub async fn create_dir(&mut self, remote: &str) -> Result<(), VfsError> {
        match self {
            Self::Sftp(sftp) => sftp.create_dir(remote).await,
            Self::S3(s3) => s3.create_dir(remote).await,
        }
    }

    pub async fn rename(&mut self, from: &str, to: &str) -> Result<(), VfsError> {
        match self {
            Self::Sftp(sftp) => sftp.rename(from, to).await,
            Self::S3(s3) => s3.rename(from, to).await,
        }
    }

    /// Delete a file, or a directory and everything in it.
    pub async fn remove(&mut self, remote: &str, is_dir: bool) -> Result<(), VfsError> {
        match self {
            Self::Sftp(sftp) => sftp.remove(remote, is_dir).await,
            Self::S3(s3) => s3.remove(remote, is_dir).await,
        }
    }
}

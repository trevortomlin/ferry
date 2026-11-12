// SFTP over SSH.
//
// Listing uses the raw SFTP session rather than the convenience wrapper,
// because the wrapper's `read_dir` reads the entire directory into a `Vec`
// before returning. The wire protocol already hands back a batch at a time, and
// forwarding each batch as it lands is what makes a huge directory start
// drawing immediately instead of after the whole thing has transferred.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::model::entries::{Entries, Entry, Kind};
use crate::secrets::Credentials;
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, known_hosts};
use russh_sftp::client::RawSftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use futures::StreamExt as _;
use futures::stream::FuturesOrdered;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::remote::{Chunks, Finished, Progress, VfsError};

/// SFTP servers hand back batches of a few hundred names. One `Entries` per
/// batch would mean a lot of tiny messages, so batches are accumulated until
/// this many entries and then forwarded. Large enough to keep channel traffic
/// low, small enough that the first rows appear promptly and cancellation stays
/// responsive.
const CHUNK: usize = 512;

/// Bytes per SFTP read or write request.
///
/// 32 KiB is the largest every server accepts without negotiation. Speed
/// comes from keeping many requests in flight (below), not from big blocks.
const BLOCK: usize = 32 * 1024;

/// READDIRs in flight while listing.
const DIR_WINDOW: usize = 8;

/// Reads and writes in flight during a transfer: 2 MiB of 32 KiB blocks,
/// enough to fill a 300 Mbit/s link at 50 ms of latency without holding much.
const READ_WINDOW: usize = 64;
const WRITE_WINDOW: usize = 64;

pub struct Sftp {
    /// Dropping this closes the SSH connection, so it is held even though the
    /// SFTP session is what we actually talk to.
    _ssh: client::Handle<HostKeys>,
    sftp: RawSftpSession,
}

impl Sftp {
    pub async fn connect(
        host: &str,
        port: u16,
        credentials: Credentials,
    ) -> Result<Self, VfsError> {
        let config = Arc::new(client::Config {
            // Long enough that browsing does not drop the connection, short
            // enough that a dead link is noticed.
            inactivity_timeout: Some(std::time::Duration::from_secs(300)),
            keepalive_interval: Some(std::time::Duration::from_secs(30)),
            ..Default::default()
        });

        let verdict = Arc::new(AtomicU8::new(HostKeys::PENDING));
        let handler = HostKeys {
            host: host.to_owned(),
            port,
            verdict: Arc::clone(&verdict),
        };

        let mut ssh = client::connect(config, (host, port), handler)
            .await
            .map_err(|error| match verdict.load(Ordering::Acquire) {
                HostKeys::CHANGED => VfsError::HostKeyChanged {
                    fingerprint: String::from("see ~/.ssh/known_hosts"),
                    line: 0,
                },
                _ => VfsError::Unreachable(describe_connect(host, port, &error)),
            })?;

        authenticate(&mut ssh, credentials).await?;

        let channel = ssh
            .channel_open_session()
            .await
            .map_err(|error| VfsError::Failed(format!("Could not open a channel: {error}")))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|error| {
                VfsError::Failed(format!(
                    "The server accepted the login but refused the SFTP subsystem ({error}). \
                     Check that SFTP is enabled for this account."
                ))
            })?;

        let sftp = RawSftpSession::new(channel.into_stream());
        sftp.init()
            .await
            .map_err(|error| VfsError::Failed(format!("SFTP handshake failed: {error}")))?;

        Ok(Self { _ssh: ssh, sftp })
    }

    pub async fn home(&mut self) -> Result<String, VfsError> {
        let name = self
            .sftp
            .realpath(".")
            .await
            .map_err(|error| status_error(".", &error))?;
        Ok(name
            .files
            .first()
            .map(|file| file.filename.clone())
            .unwrap_or_else(|| "/".to_owned()))
    }

    pub async fn list(&mut self, path: &str, sink: &mut impl Chunks) -> Result<(), VfsError> {
        let handle = self
            .sftp
            .opendir(path)
            .await
            .map_err(|error| status_error(path, &error))?
            .handle;

        let result = self.drain_dir(&handle, sink).await;

        // Close the handle whatever happened; leaking one on the server costs a
        // file descriptor there until the session ends.
        let _ = self.sftp.close(handle).await;
        result
    }

    /// Reads the directory with several READDIRs in flight at once. Each one
    /// returns the next batch of names, so a 100k-entry listing is a thousand
    /// round trips — waiting on them one at a time made the link's latency the
    /// whole cost. Batches can come back in any order without harm: the list
    /// is sorted on arrival anyway.
    async fn drain_dir(&self, handle: &str, sink: &mut impl Chunks) -> Result<(), VfsError> {
        let mut batch = Entries::with_capacity(CHUNK + 128, (CHUNK + 128) * 24);
        let mut pending = FuturesOrdered::new();
        for _ in 0..DIR_WINDOW {
            pending.push_back(self.sftp.readdir(handle.to_owned()));
        }
        let mut done = false;

        while let Some(result) = pending.next().await {
            let names = match result {
                Ok(names) => names,
                Err(russh_sftp::client::error::Error::Status(status))
                    if status.status_code == StatusCode::Eof =>
                {
                    // Normal end of directory. Requests still in flight
                    // answer EOF too; they are drained, not reissued.
                    done = true;
                    continue;
                }
                Err(error) => return Err(status_error("directory", &error)),
            };
            if !done {
                pending.push_back(self.sftp.readdir(handle.to_owned()));
            }

            for file in names.files {
                // `.` and `..` are navigation, not content; the UI has its own
                // affordances for going up.
                if file.filename == "." || file.filename == ".." {
                    continue;
                }
                batch.push(Entry {
                    name: &file.filename,
                    size: file.attrs.size.unwrap_or(0),
                    modified: file.attrs.mtime.unwrap_or(0) as i64,
                    mode: file.attrs.permissions.unwrap_or(0),
                    kind: kind_of(&file.attrs),
                });
            }

            if batch.len() >= CHUNK {
                let full = std::mem::replace(
                    &mut batch,
                    Entries::with_capacity(CHUNK + 128, (CHUNK + 128) * 24),
                );
                if !sink.push(full, false) {
                    // The caller navigated away.
                    return Ok(());
                }
            }
        }
        sink.push(batch, true);
        Ok(())
    }
}

impl Sftp {
    pub async fn size_of(&mut self, remote: &str) -> Result<u64, VfsError> {
        let attrs = self
            .sftp
            .stat(remote)
            .await
            .map_err(|error| status_error(remote, &error))?;
        Ok(attrs.attrs.size.unwrap_or(0))
    }

    /// Size of an interrupted download or upload of `remote`, if one is there.
    pub async fn partial_size(&mut self, remote: &str) -> Option<u64> {
        self.sftp
            .stat(&partial_remote(remote))
            .await
            .ok()
            .and_then(|attrs| attrs.attrs.size)
    }

    /// Remove an interrupted upload, so an overwrite really does start clean.
    pub async fn discard_partial(&mut self, remote: &str) {
        let _ = self.sftp.remove(partial_remote(remote)).await;
    }

    pub async fn download(
        &mut self,
        remote: &str,
        local: &std::path::Path,
        resume_from: u64,
        progress: &mut impl Progress,
    ) -> Result<Finished, VfsError> {
        let handle = self
            .sftp
            .open(remote, OpenFlags::READ, FileAttributes::default())
            .await
            .map_err(|error| status_error(remote, &error))?
            .handle;

        // Written beside the destination and renamed at the end, so an
        // interrupted download never leaves something that looks complete —
        // and so the next attempt has something to resume from.
        let partial = partial_path(local);
        let result = self
            .stream_down(&handle, &partial, resume_from, progress)
            .await;
        let _ = self.sftp.close(handle).await;

        match result? {
            Finished::Complete => {
                std::fs::rename(&partial, local).map_err(|error| {
                    VfsError::Failed(format!(
                        "Downloaded, but could not move it into place: {error}"
                    ))
                })?;
                Ok(Finished::Complete)
            }
            // The partial stays put: it is exactly what a resume needs.
            Finished::Cancelled => Ok(Finished::Cancelled),
        }
    }

    async fn stream_down(
        &self,
        handle: &str,
        partial: &std::path::Path,
        resume_from: u64,
        progress: &mut impl Progress,
    ) -> Result<Finished, VfsError> {
        let mut file = if resume_from > 0 {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(partial)
                .await
        } else {
            tokio::fs::File::create(partial).await
        }
        .map_err(|error| {
            VfsError::Failed(format!("Could not write {}: {error}", partial.display()))
        })?;

        // Several reads in flight, written in order as they complete. One at
        // a time, throughput was one block per round trip: 21 MB/s even over
        // loopback.
        let read = |offset: u64, len: usize| {
            let request = self.sftp.read(handle.to_owned(), offset, len as u32);
            async move { (offset, len, request.await) }
        };
        let mut pending = FuturesOrdered::new();
        let mut next = resume_from;
        for _ in 0..READ_WINDOW {
            pending.push_back(read(next, BLOCK));
            next += BLOCK as u64;
        }
        let mut eof = false;
        while let Some((offset, len, result)) = pending.next().await {
            let mut data = match result {
                Ok(data) => data.data,
                // End of file arrives as a status, not an empty read.
                Err(russh_sftp::client::error::Error::Status(status))
                    if status.status_code == StatusCode::Eof =>
                {
                    Vec::new()
                }
                Err(error) => return Err(status_error("download", &error)),
            };
            if data.is_empty() {
                eof = true;
                break;
            }
            // A server may answer with less than asked for without being at
            // the end; fetch the rest before writing, so blocks stay in place.
            while data.len() < len {
                match self
                    .sftp
                    .read(handle.to_owned(), offset + data.len() as u64, (len - data.len()) as u32)
                    .await
                {
                    Ok(more) if !more.data.is_empty() => data.extend_from_slice(&more.data),
                    Ok(_) => {
                        eof = true;
                        break;
                    }
                    Err(russh_sftp::client::error::Error::Status(status))
                        if status.status_code == StatusCode::Eof =>
                    {
                        eof = true;
                        break;
                    }
                    Err(error) => return Err(status_error("download", &error)),
                }
            }
            file.write_all(&data).await.map_err(|error| {
                VfsError::Failed(format!("Could not write {}: {error}", partial.display()))
            })?;
            if !progress.advance(data.len() as u64) {
                return Ok(Finished::Cancelled);
            }
            if eof {
                break;
            }
            pending.push_back(read(next, BLOCK));
            next += BLOCK as u64;
        }
        let _ = eof;

        file.flush()
            .await
            .map_err(|error| VfsError::Failed(format!("Could not finish writing: {error}")))?;
        Ok(Finished::Complete)
    }

    pub async fn upload(
        &mut self,
        local: &std::path::Path,
        remote: &str,
        resume_from: u64,
        progress: &mut impl Progress,
    ) -> Result<Finished, VfsError> {
        let mut file = tokio::fs::File::open(local).await.map_err(|error| {
            VfsError::Failed(format!("Could not read {}: {error}", local.display()))
        })?;
        if resume_from > 0 {
            use tokio::io::AsyncSeekExt as _;
            file.seek(std::io::SeekFrom::Start(resume_from))
                .await
                .map_err(|error| {
                    VfsError::Failed(format!("Could not resume {}: {error}", local.display()))
                })?;
        }

        // Uploads go to a partial name and are renamed on success, for the same
        // reason downloads do: an interrupted upload must not leave a truncated
        // file sitting under the real name looking complete.
        let staging = partial_remote(remote);
        let flags = if resume_from > 0 {
            OpenFlags::WRITE
        } else {
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE
        };
        let handle = self
            .sftp
            .open(staging.as_str(), flags, FileAttributes::default())
            .await
            .map_err(|error| status_error(&staging, &error))?
            .handle;

        // Writes are kept in flight the same way reads are.
        let mut pending = FuturesOrdered::new();
        let mut offset = resume_from;
        let mut finished_reading = false;
        let outcome = loop {
            while !finished_reading && pending.len() < WRITE_WINDOW {
                let mut buffer = vec![0u8; BLOCK];
                let mut filled = 0;
                // Fill a whole block, so writes stay large and aligned.
                while filled < BLOCK {
                    match file.read(&mut buffer[filled..]).await {
                        Ok(0) => {
                            finished_reading = true;
                            break;
                        }
                        Ok(read) => filled += read,
                        Err(error) => {
                            return Err(VfsError::Failed(format!(
                                "Could not read {}: {error}",
                                local.display()
                            )));
                        }
                    }
                }
                if filled == 0 {
                    break;
                }
                buffer.truncate(filled);
                let request = self.sftp.write(handle.as_str(), offset, buffer);
                pending.push_back(async move { (filled, request.await) });
                offset += filled as u64;
            }
            let Some((written, result)) = pending.next().await else {
                break Ok(Finished::Complete);
            };
            if let Err(error) = result {
                break Err(status_error(&staging, &error));
            }
            if !progress.advance(written as u64) {
                break Ok(Finished::Cancelled);
            }
        };
        // Anything still in flight is dropped; the server finishes or
        // discards it, and the handle is closed next either way.
        drop(pending);

        let _ = self.sftp.close(handle).await;

        if matches!(outcome, Ok(Finished::Complete)) {
            // SFTP rename fails when the target exists, so an overwrite has to
            // clear the way first.
            let _ = self.sftp.remove(remote).await;
            self.sftp
                .rename(staging.as_str(), remote)
                .await
                .map_err(|error| status_error(remote, &error))?;
        }
        // A cancelled or failed upload leaves the partial where it is, under a
        // name that cannot be mistaken for the real file, ready to resume.
        outcome
    }

    pub async fn create_dir(&mut self, remote: &str) -> Result<(), VfsError> {
        self.sftp
            .mkdir(remote, FileAttributes::default())
            .await
            .map_err(|error| status_error(remote, &error))?;
        Ok(())
    }

    pub async fn rename(&mut self, from: &str, to: &str) -> Result<(), VfsError> {
        self.sftp
            .rename(from, to)
            .await
            .map_err(|error| status_error(from, &error))?;
        Ok(())
    }

    pub async fn remove(&mut self, remote: &str, is_dir: bool) -> Result<(), VfsError> {
        if !is_dir {
            self.sftp
                .remove(remote)
                .await
                .map_err(|error| status_error(remote, &error))?;
            return Ok(());
        }

        // SFTP has no recursive delete, so the tree is walked depth-first.
        // Iterative rather than recursive because an async recursive function
        // needs boxing, and because a deep tree should not risk the stack.
        let mut pending = vec![remote.to_owned()];
        let mut directories = Vec::new();

        while let Some(directory) = pending.pop() {
            let mut children = Vec::new();
            let handle = self
                .sftp
                .opendir(directory.as_str())
                .await
                .map_err(|error| status_error(&directory, &error))?
                .handle;

            loop {
                match self.sftp.readdir(handle.as_str()).await {
                    Ok(names) => {
                        for file in names.files {
                            if file.filename == "." || file.filename == ".." {
                                continue;
                            }
                            let child =
                                format!("{}/{}", directory.trim_end_matches('/'), file.filename);
                            if kind_of(&file.attrs).is_dir() {
                                pending.push(child);
                            } else {
                                children.push(child);
                            }
                        }
                    }
                    Err(russh_sftp::client::error::Error::Status(status))
                        if status.status_code == StatusCode::Eof =>
                    {
                        break;
                    }
                    Err(error) => return Err(status_error(&directory, &error)),
                }
            }
            let _ = self.sftp.close(handle).await;

            for child in children {
                self.sftp
                    .remove(child.as_str())
                    .await
                    .map_err(|error| status_error(&child, &error))?;
            }
            directories.push(directory);
        }

        // Deepest first, since a directory must be empty to be removed.
        for directory in directories.into_iter().rev() {
            self.sftp
                .rmdir(directory.as_str())
                .await
                .map_err(|error| status_error(&directory, &error))?;
        }
        Ok(())
    }
}

/// Suffix marking an incomplete transfer, on either side.
const PARTIAL: &str = ".ferry-part";

/// Where a download is written before it is complete.
fn partial_path(local: &std::path::Path) -> std::path::PathBuf {
    let mut name = local.file_name().unwrap_or_default().to_os_string();
    name.push(PARTIAL);
    local.with_file_name(name)
}

/// Where an upload is written before it is complete.
fn partial_remote(remote: &str) -> String {
    format!("{remote}{PARTIAL}")
}

fn kind_of(attrs: &russh_sftp::protocol::FileAttributes) -> Kind {
    if attrs.is_symlink() {
        Kind::Symlink
    } else if attrs.is_dir() {
        Kind::Directory
    } else {
        Kind::File
    }
}

async fn authenticate(
    ssh: &mut client::Handle<HostKeys>,
    credentials: Credentials,
) -> Result<(), VfsError> {
    match credentials {
        Credentials::SshAgent { username } => {
            let mut agent = russh::keys::agent::client::AgentClient::connect_env()
                .await
                .map_err(|error| {
                    VfsError::Auth(format!(
                        "Could not reach the SSH agent ({error}). Start one, or pick another \
                         authentication method."
                    ))
                })?;

            let identities = agent.request_identities().await.map_err(|error| {
                VfsError::Auth(format!("The SSH agent would not list its keys: {error}"))
            })?;
            if identities.is_empty() {
                return Err(VfsError::Auth(
                    "Your SSH agent holds no keys. Add one with ssh-add.".to_owned(),
                ));
            }

            // Offer each key in turn, exactly as OpenSSH does.
            let count = identities.len();
            for identity in identities {
                let key = identity.public_key().into_owned();
                let hash = key.algorithm().is_rsa().then_some(HashAlg::Sha256);
                match ssh
                    .authenticate_publickey_with(&username, key, hash, &mut agent)
                    .await
                {
                    Ok(result) if result.success() => return Ok(()),
                    Ok(_) => continue,
                    Err(error) => {
                        return Err(VfsError::Auth(format!(
                            "Agent authentication failed: {error}"
                        )));
                    }
                }
            }
            Err(VfsError::Auth(format!(
                "The server rejected all {count} key(s) in your SSH agent."
            )))
        }

        Credentials::SshPassword { username, password } => {
            let result = ssh
                .authenticate_password(&username, password.expose())
                .await
                .map_err(|error| VfsError::Auth(error.to_string()))?;
            if result.success() {
                Ok(())
            } else {
                Err(VfsError::Auth(format!(
                    "The server rejected the password for {username}."
                )))
            }
        }

        Credentials::SshKeyFile {
            username,
            path,
            passphrase,
        } => {
            let expanded = expand_home(&path);
            let key = russh::keys::load_secret_key(
                &expanded,
                passphrase.as_ref().map(|secret| secret.expose()),
            )
            .map_err(|error| {
                VfsError::Auth(format!("Could not read the key at {expanded}: {error}"))
            })?;

            let hash = key.algorithm().is_rsa().then_some(HashAlg::Sha256);
            let result = ssh
                .authenticate_publickey(&username, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|error| VfsError::Auth(error.to_string()))?;
            if result.success() {
                Ok(())
            } else {
                Err(VfsError::Auth(format!(
                    "The server rejected the key at {expanded} for {username}."
                )))
            }
        }

        other => Err(VfsError::Auth(format!(
            "{other:?} is not an SSH authentication method."
        ))),
    }
}

/// `~` only, which is the form people actually type into a key-file field.
fn expand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => format!("{}/{rest}", home.to_string_lossy()),
            None => path.to_owned(),
        },
        None => path.to_owned(),
    }
}

fn describe_connect(host: &str, port: u16, error: &russh::Error) -> String {
    format!("{host}:{port} — {error}")
}

fn status_error(path: &str, error: &russh_sftp::client::error::Error) -> VfsError {
    use russh_sftp::client::error::Error;
    match error {
        Error::Status(status) => match status.status_code {
            StatusCode::NoSuchFile => VfsError::NotFound(path.to_owned()),
            StatusCode::PermissionDenied => VfsError::PermissionDenied(path.to_owned()),
            _ => VfsError::Failed(format!("{path}: {}", status.error_message)),
        },
        other => VfsError::Failed(format!("{path}: {other}")),
    }
}

/// Host key verification.
///
/// Known host: accept. Unknown host: accept and record it, the same
/// trust-on-first-use OpenSSH offers when you answer "yes". *Changed* host key:
/// refuse, because that is the case the whole mechanism exists for.
///
/// A prompt showing the fingerprint before trusting a new host would be better
/// than silent TOFU, but it needs a dialog the connect path can await; that is
/// noted for the polish milestone.
struct HostKeys {
    host: String,
    port: u16,
    verdict: Arc<AtomicU8>,
}

impl HostKeys {
    const PENDING: u8 = 0;
    const KNOWN: u8 = 1;
    const LEARNED: u8 = 2;
    const CHANGED: u8 = 3;
}

impl client::Handler for HostKeys {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        match known_hosts::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => {
                self.verdict.store(Self::KNOWN, Ordering::Release);
                Ok(true)
            }
            Ok(false) => {
                if let Err(error) = known_hosts::learn_known_hosts(&self.host, self.port, key) {
                    tracing::warn!(%error, "could not record the host key");
                }
                tracing::info!(host = %self.host, "trusting a new host key on first use");
                self.verdict.store(Self::LEARNED, Ordering::Release);
                Ok(true)
            }
            Err(russh::keys::Error::KeyChanged { line }) => {
                tracing::error!(host = %self.host, line, "host key changed");
                self.verdict.store(Self::CHANGED, Ordering::Release);
                Ok(false)
            }
            Err(error) => {
                tracing::warn!(%error, "could not check known_hosts");
                self.verdict.store(Self::LEARNED, Ordering::Release);
                Ok(true)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_only_at_the_start() {
        // SAFETY: single-threaded test setup.
        unsafe { std::env::set_var("HOME", "/home/ferry") };
        assert_eq!(
            expand_home("~/.ssh/id_ed25519"),
            "/home/ferry/.ssh/id_ed25519"
        );
        assert_eq!(expand_home("/absolute/key"), "/absolute/key");
        assert_eq!(expand_home("relative/key"), "relative/key");
        // A file genuinely named "~" is left alone.
        assert_eq!(expand_home("~"), "~");
        assert_eq!(expand_home("/a/~/b"), "/a/~/b");
    }

    #[test]
    fn not_found_and_permission_denied_are_distinguished() {
        let status = |code: StatusCode| {
            russh_sftp::client::error::Error::Status(russh_sftp::protocol::Status {
                id: 0,
                status_code: code,
                error_message: "message".to_owned(),
                language_tag: String::new(),
            })
        };
        assert!(matches!(
            status_error("/gone", &status(StatusCode::NoSuchFile)),
            VfsError::NotFound(path) if path == "/gone"
        ));
        assert!(matches!(
            status_error("/locked", &status(StatusCode::PermissionDenied)),
            VfsError::PermissionDenied(path) if path == "/locked"
        ));
    }
}

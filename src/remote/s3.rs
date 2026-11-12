// Amazon S3, and anything that speaks its API.
//
// S3 has no directories. It has a flat keyspace and a convention: keys contain
// slashes, and asking for a delimiter makes the service report the distinct
// prefixes up to the next one. That is what `CommonPrefixes` is, and it is what
// this maps to directories, so the browser does not have to learn a second
// shape of filesystem.
//
// The mapping in both directions:
//
//   path  /reports/2026/q1.csv   key  reports/2026/q1.csv
//   path  /reports/2026          key  reports/2026/     (as a prefix)
//   path  /                      key  ""                (bucket root)
//
// Consequences worth knowing, since they are S3's and not ours:
//
//   * A directory only exists if something is under it, so an empty one is
//     represented by a zero-byte object whose key ends in a slash. Those marker
//     objects are hidden from listings, since showing a folder inside itself
//     would be nonsense.
//   * There is no rename. Copy then delete is the only option, and it is not
//     atomic.
//   * Objects have no permissions to show, so the mode column stays empty.

use std::path::Path;

use aws_sdk_s3::Client;
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use crate::model::entries::{Entries, Entry, Kind};
use crate::secrets::Credentials as Auth;
use tokio::io::AsyncWriteExt;

use crate::remote::{Chunks, Finished, Progress, VfsError};

/// Objects requested per listing page. The service caps this at 1000, which is
/// also a sensible chunk to hand the UI.
const PAGE: i32 = 1000;

/// Bytes per upload part. S3 requires at least 5 MiB for every part but the
/// last, so this is the floor rather than a tuned figure.
const PART: usize = 5 * 1024 * 1024;

pub struct S3 {
    client: Client,
    bucket: String,
}

impl S3 {
    pub async fn connect(
        endpoint: &str,
        bucket: &str,
        region: &str,
        credentials: Auth,
    ) -> Result<Self, VfsError> {
        let region = Region::new(region.to_owned());

        let mut loader = aws_config::defaults(BehaviorVersion::latest()).region(region);
        match &credentials {
            Auth::AwsProfile { profile } => {
                loader = loader.profile_name(profile);
            }
            Auth::AwsKeys {
                access_key_id,
                secret_access_key,
            } => {
                loader = loader.credentials_provider(Credentials::new(
                    access_key_id,
                    secret_access_key.expose(),
                    None,
                    None,
                    "ferry",
                ));
            }
            other => {
                return Err(VfsError::Auth(format!(
                    "{other:?} is not an S3 authentication method."
                )));
            }
        }

        let shared = loader.load().await;
        let mut config = aws_sdk_s3::config::Builder::from(&shared);
        if !endpoint.trim().is_empty() {
            // Anything that is not AWS itself — MinIO, Ceph, R2 — is almost
            // always addressed by path rather than by virtual host, and will
            // not have a wildcard certificate for bucket subdomains.
            config = config.endpoint_url(endpoint.trim()).force_path_style(true);
        }

        Ok(Self {
            client: Client::from_conf(config.build()),
            bucket: bucket.to_owned(),
        })
    }

    /// Fails early with a message about the bucket rather than letting the
    /// first listing produce something cryptic.
    ///
    /// `HeadBucket` answers with a bare status and no body, so the status is
    /// the only thing to go on — and the three that matter each mean something
    /// quite different to whoever is reading the message.
    pub async fn check(&self) -> Result<(), VfsError> {
        let Err(error) = self.client.head_bucket().bucket(&self.bucket).send().await else {
            return Ok(());
        };

        let bucket = &self.bucket;
        Err(match status_of(&error) {
            Some(403) => VfsError::Auth(format!(
                "the credentials were rejected for bucket “{bucket}”. Check the access key \
                 and secret, and that they are allowed to use this bucket."
            )),
            Some(404) => VfsError::NotFound(format!(
                "bucket “{bucket}” — no bucket by that name in this region."
            )),
            Some(301) | Some(307) => VfsError::Unreachable(format!(
                "bucket “{bucket}” is in a different region. Correct the region and try again."
            )),
            _ => VfsError::Unreachable(format!(
                "bucket “{bucket}” — {}. Check the endpoint, the bucket name, and the region.",
                describe(&error)
            )),
        })
    }

    pub async fn list(&mut self, path: &str, sink: &mut impl Chunks) -> Result<(), VfsError> {
        let prefix = prefix_of(path);
        let mut token: Option<String> = None;

        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                // The delimiter is what turns a flat keyspace into one level of
                // a directory tree.
                .delimiter("/")
                .max_keys(PAGE);
            if let Some(token) = &token {
                request = request.continuation_token(token);
            }

            let page = request
                .send()
                .await
                .map_err(|error| listing_error(path, &error))?;

            let mut batch = Entries::new();

            for common in page.common_prefixes() {
                let Some(full) = common.prefix() else {
                    continue;
                };
                let name = full
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                batch.push(Entry {
                    name,
                    size: 0,
                    modified: 0,
                    // S3 has no permission bits to report.
                    mode: 0,
                    kind: Kind::Directory,
                });
            }

            for object in page.contents() {
                let Some(key) = object.key() else { continue };
                // The prefix itself comes back as an object when a marker was
                // created for an empty directory. Showing it would put the
                // folder inside itself.
                if key == prefix {
                    continue;
                }
                let name = key.rsplit('/').next().unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                batch.push(Entry {
                    name,
                    size: object.size().unwrap_or(0).max(0) as u64,
                    modified: object.last_modified().map(|at| at.secs()).unwrap_or(0),
                    mode: 0,
                    kind: Kind::File,
                });
            }

            token = page.next_continuation_token().map(str::to_owned);
            let last = token.is_none();
            if !sink.push(batch, last) {
                // Navigated away.
                return Ok(());
            }
            if last {
                return Ok(());
            }
        }
    }

    /// S3 has no working directory, so browsing starts at the bucket root.
    pub async fn home(&mut self) -> Result<String, VfsError> {
        Ok("/".to_owned())
    }

    pub async fn size_of(&mut self, path: &str) -> Result<u64, VfsError> {
        let key = key_of(path);
        let head = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|error| object_error(path, &error))?;
        Ok(head.content_length().unwrap_or(0).max(0) as u64)
    }

    pub async fn partial_size(&mut self, path: &str) -> Option<u64> {
        // Uploads are staged under a partial key for the same reason SFTP
        // stages under a partial name.
        self.size_of(&partial(path)).await.ok()
    }

    pub async fn discard_partial(&mut self, path: &str) {
        let _ = self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(key_of(&partial(path)))
            .send()
            .await;
    }

    pub async fn download(
        &mut self,
        path: &str,
        local: &Path,
        resume_from: u64,
        progress: &mut impl Progress,
    ) -> Result<Finished, VfsError> {
        let key = key_of(path);
        let mut request = self.client.get_object().bucket(&self.bucket).key(&key);
        if resume_from > 0 {
            // A ranged GET is how S3 resumes; there is no seek.
            request = request.range(format!("bytes={resume_from}-"));
        }

        let mut response = request
            .send()
            .await
            .map_err(|error| object_error(path, &error))?;

        let staging = partial_path(local);
        let mut file = if resume_from > 0 {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&staging)
                .await
        } else {
            tokio::fs::File::create(&staging).await
        }
        .map_err(|error| {
            VfsError::Failed(format!("Could not write {}: {error}", staging.display()))
        })?;

        while let Some(chunk) = response
            .body
            .next()
            .await
            .transpose()
            .map_err(|error| VfsError::Failed(format!("{path}: {error}")))?
        {
            file.write_all(&chunk).await.map_err(|error| {
                VfsError::Failed(format!("Could not write {}: {error}", staging.display()))
            })?;
            if !progress.advance(chunk.len() as u64) {
                return Ok(Finished::Cancelled);
            }
        }

        file.flush()
            .await
            .map_err(|error| VfsError::Failed(format!("Could not finish writing: {error}")))?;
        drop(file);

        std::fs::rename(&staging, local).map_err(|error| {
            VfsError::Failed(format!(
                "Downloaded, but could not move it into place: {error}"
            ))
        })?;
        Ok(Finished::Complete)
    }

    pub async fn upload(
        &mut self,
        local: &Path,
        path: &str,
        resume_from: u64,
        progress: &mut impl Progress,
    ) -> Result<Finished, VfsError> {
        // S3 has no append. A resumed upload has to start over, which is why
        // the conflict dialog only offers to continue when there is something
        // to continue — and for S3 there never is.
        let _ = resume_from;

        let size = std::fs::metadata(local).map(|m| m.len()).map_err(|error| {
            VfsError::Failed(format!("Could not read {}: {error}", local.display()))
        })?;
        let key = key_of(path);

        if size <= PART as u64 {
            let body = ByteStream::from_path(local).await.map_err(|error| {
                VfsError::Failed(format!("Could not read {}: {error}", local.display()))
            })?;
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .body(body)
                .send()
                .await
                .map_err(|error| object_error(path, &error))?;
            progress.advance(size);
            return Ok(Finished::Complete);
        }

        self.multipart_upload(local, &key, path, size, progress)
            .await
    }

    /// Anything over one part goes up piece by piece, so progress means
    /// something and a cancel does not have to wait for the whole file.
    async fn multipart_upload(
        &mut self,
        local: &Path,
        key: &str,
        path: &str,
        size: u64,
        progress: &mut impl Progress,
    ) -> Result<Finished, VfsError> {
        use aws_sdk_s3::types::CompletedMultipartUpload;

        let started = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| object_error(path, &error))?;
        let Some(upload_id) = started.upload_id().map(str::to_owned) else {
            return Err(VfsError::Failed(
                "The server accepted a multipart upload but gave no id.".to_owned(),
            ));
        };

        let outcome = self
            .send_parts(local, key, &upload_id, size, progress)
            .await;

        match outcome {
            Ok(Some(parts)) => {
                self.client
                    .complete_multipart_upload()
                    .bucket(&self.bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .multipart_upload(
                        CompletedMultipartUpload::builder()
                            .set_parts(Some(parts))
                            .build(),
                    )
                    .send()
                    .await
                    .map_err(|error| object_error(path, &error))?;
                Ok(Finished::Complete)
            }
            // Abandoned or failed: abort so the parts are not billed forever.
            other => {
                let _ = self
                    .client
                    .abort_multipart_upload()
                    .bucket(&self.bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .send()
                    .await;
                match other {
                    Ok(_) => Ok(Finished::Cancelled),
                    Err(error) => Err(error),
                }
            }
        }
    }

    async fn send_parts(
        &self,
        local: &Path,
        key: &str,
        upload_id: &str,
        size: u64,
        progress: &mut impl Progress,
    ) -> Result<Option<Vec<aws_sdk_s3::types::CompletedPart>>, VfsError> {
        use aws_sdk_s3::types::CompletedPart;
        use tokio::io::AsyncReadExt;

        let mut file = tokio::fs::File::open(local).await.map_err(|error| {
            VfsError::Failed(format!("Could not read {}: {error}", local.display()))
        })?;
        let mut parts = Vec::with_capacity((size as usize).div_ceil(PART));
        let mut buffer = vec![0u8; PART];
        let mut number = 1i32;

        loop {
            let mut filled = 0;
            while filled < PART {
                let read = file.read(&mut buffer[filled..]).await.map_err(|error| {
                    VfsError::Failed(format!("Could not read {}: {error}", local.display()))
                })?;
                if read == 0 {
                    break;
                }
                filled += read;
            }
            if filled == 0 {
                break;
            }

            let uploaded = self
                .client
                .upload_part()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(upload_id)
                .part_number(number)
                .body(ByteStream::from(buffer[..filled].to_vec()))
                .send()
                .await
                .map_err(|error| {
                    VfsError::Failed(format!("Part {number}: {}", describe(&error)))
                })?;

            parts.push(
                CompletedPart::builder()
                    .part_number(number)
                    .set_e_tag(uploaded.e_tag().map(str::to_owned))
                    .build(),
            );
            number += 1;

            if !progress.advance(filled as u64) {
                return Ok(None);
            }
        }

        Ok(Some(parts))
    }

    /// Creates the zero-byte marker object that makes an empty directory
    /// visible. Once something real is under the prefix the marker is
    /// redundant, but harmless.
    pub async fn create_dir(&mut self, path: &str) -> Result<(), VfsError> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(prefix_of(path))
            .body(ByteStream::from_static(b""))
            .send()
            .await
            .map_err(|error| object_error(path, &error))?;
        Ok(())
    }

    /// Copy then delete: S3 has no rename, and this is not atomic.
    pub async fn rename(&mut self, from: &str, to: &str) -> Result<(), VfsError> {
        let (source, target) = (key_of(from), key_of(to));
        self.client
            .copy_object()
            .bucket(&self.bucket)
            .copy_source(format!("{}/{source}", self.bucket))
            .key(&target)
            .send()
            .await
            .map_err(|error| object_error(from, &error))?;
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&source)
            .send()
            .await
            .map_err(|error| object_error(from, &error))?;
        Ok(())
    }

    pub async fn remove(&mut self, path: &str, is_dir: bool) -> Result<(), VfsError> {
        if !is_dir {
            self.client
                .delete_object()
                .bucket(&self.bucket)
                .key(key_of(path))
                .send()
                .await
                .map_err(|error| object_error(path, &error))?;
            return Ok(());
        }

        // A directory is every key under its prefix. Deletes go in batches of
        // a thousand, which is the API's limit and also why this is not a loop
        // of individual requests.
        let prefix = prefix_of(path);
        let mut token: Option<String> = None;
        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .max_keys(PAGE);
            if let Some(token) = &token {
                request = request.continuation_token(token);
            }
            let page = request
                .send()
                .await
                .map_err(|error| listing_error(path, &error))?;

            let keys: Vec<_> = page
                .contents()
                .iter()
                .filter_map(|object| object.key())
                .filter_map(|key| ObjectIdentifier::builder().key(key).build().ok())
                .collect();

            if !keys.is_empty() {
                let deleted = self
                    .client
                    .delete_objects()
                    .bucket(&self.bucket)
                    .delete(
                        Delete::builder()
                            .set_objects(Some(keys))
                            .build()
                            .map_err(|error| VfsError::Failed(error.to_string()))?,
                    )
                    .send()
                    .await
                    .map_err(|error| object_error(path, &error))?;

                // Partial failure is reported per key rather than as an error.
                if let Some(problem) = deleted.errors().first() {
                    return Err(VfsError::Failed(format!(
                        "Could not delete {}: {}",
                        problem.key().unwrap_or("an object"),
                        problem.message().unwrap_or("no reason given")
                    )));
                }
            }

            token = page.next_continuation_token().map(str::to_owned);
            if token.is_none() {
                return Ok(());
            }
        }
    }
}

/// `/a/b.txt` becomes `a/b.txt`. The bucket root is the empty key.
fn key_of(path: &str) -> String {
    path.trim_start_matches('/').to_owned()
}

/// `/a/b` becomes `a/b/`, which is how a directory is asked about. The root is
/// the empty prefix, not `/`.
fn prefix_of(path: &str) -> String {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

fn partial(path: &str) -> String {
    format!("{path}.ferry-part")
}

fn partial_path(local: &Path) -> std::path::PathBuf {
    let mut name = local.file_name().unwrap_or_default().to_os_string();
    name.push(".ferry-part");
    local.with_file_name(name)
}

/// The HTTP status behind an SDK error, when there was a response at all.
fn status_of<E, R: HttpStatus>(error: &aws_sdk_s3::error::SdkError<E, R>) -> Option<u16> {
    error.raw_response().map(HttpStatus::code)
}

/// Lets `status_of` work for any response type the SDK hands back.
trait HttpStatus {
    fn code(&self) -> u16;
}

impl HttpStatus for aws_smithy_runtime_api::http::Response {
    fn code(&self) -> u16 {
        self.status().as_u16()
    }
}

/// The SDK's errors are deeply nested; this digs out the part worth showing.
fn describe<E: std::fmt::Debug, R>(error: &aws_sdk_s3::error::SdkError<E, R>) -> String {
    use aws_sdk_s3::error::SdkError;
    match error {
        SdkError::ServiceError(inner) => format!("{:?}", inner.err()),
        SdkError::DispatchFailure(_) => "could not reach the service".to_owned(),
        SdkError::TimeoutError(_) => "the request timed out".to_owned(),
        SdkError::ConstructionFailure(_) => "the request could not be built".to_owned(),
        SdkError::ResponseError(_) => "the service sent something unexpected".to_owned(),
        _ => "the request failed".to_owned(),
    }
}

fn listing_error<E: std::fmt::Debug, R: HttpStatus>(
    path: &str,
    error: &aws_sdk_s3::error::SdkError<E, R>,
) -> VfsError {
    let rendered = describe(error);
    if rendered.contains("NoSuchBucket") || status_of(error) == Some(404) {
        return VfsError::NotFound(format!("{path} — no such bucket"));
    }
    if rendered.contains("AccessDenied") || status_of(error) == Some(403) {
        return VfsError::PermissionDenied(path.to_owned());
    }
    VfsError::Failed(format!("{path}: {rendered}"))
}

fn object_error<E: std::fmt::Debug, R: HttpStatus>(
    path: &str,
    error: &aws_sdk_s3::error::SdkError<E, R>,
) -> VfsError {
    // Some operations model 404 as a typed variant and some do not, so the
    // status is the reliable signal.
    match status_of(error) {
        Some(404) => return VfsError::NotFound(path.to_owned()),
        Some(403) => return VfsError::PermissionDenied(path.to_owned()),
        _ => {}
    }
    let rendered = describe(error);
    if rendered.contains("NoSuchKey") || rendered.contains("NotFound") {
        return VfsError::NotFound(path.to_owned());
    }
    VfsError::Failed(format!("{path}: {rendered}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_map_onto_keys_and_prefixes() {
        assert_eq!(key_of("/reports/2026/q1.csv"), "reports/2026/q1.csv");
        assert_eq!(key_of("/"), "");
        assert_eq!(key_of(""), "");

        assert_eq!(prefix_of("/reports/2026"), "reports/2026/");
        assert_eq!(prefix_of("/reports/2026/"), "reports/2026/");
        // The bucket root is an empty prefix, not "/", or every key would be
        // filtered out.
        assert_eq!(prefix_of("/"), "");
        assert_eq!(prefix_of(""), "");
    }

    #[test]
    fn partial_names_match_the_sftp_convention() {
        assert_eq!(partial("/a/b.bin"), "/a/b.bin.ferry-part");
        assert_eq!(
            partial_path(Path::new("/tmp/b.bin")),
            Path::new("/tmp/b.bin.ferry-part")
        );
    }
}

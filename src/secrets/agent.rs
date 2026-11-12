// Talking to ssh-agent.
//
// The agent protocol is a length-prefixed binary exchange over a Unix socket
// and the part we need is small enough to write out: send a one-byte request,
// read back a list of (key blob, comment) pairs. A dependency would cost more
// in transitive crates than this costs in lines, and keeping it here means
// `creds` does not depend on whichever SSH library `vfs` ends up using.
//
// Signing stays with the SSH client in `vfs`, which has to drive the
// authentication handshake anyway. This module exists so the connection editor
// can show which keys are actually available before you try to connect.

use std::io::{Read, Write};

const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;

/// Agents hold a handful of keys. Anything near this is a malformed or hostile
/// response, and we would rather fail than allocate on its say-so.
const MAX_RESPONSE: u32 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentKey {
    /// e.g. `ssh-ed25519`.
    pub algorithm: String,
    /// Whatever `ssh-add` recorded, usually `user@host` or the key's path.
    pub comment: String,
}

impl AgentKey {
    /// One line for the connection editor.
    pub fn describe(&self) -> String {
        let algorithm = self
            .algorithm
            .strip_prefix("ssh-")
            .unwrap_or(&self.algorithm);
        if self.comment.is_empty() {
            algorithm.to_owned()
        } else {
            format!("{algorithm} · {}", self.comment)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgentError {
    /// No `SSH_AUTH_SOCK`, so no agent is running for this session.
    NotRunning,
    Unreachable(String),
    Malformed(&'static str),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning => f.write_str("No SSH agent is running (SSH_AUTH_SOCK is not set)"),
            Self::Unreachable(why) => write!(f, "Could not reach the SSH agent: {why}"),
            Self::Malformed(what) => write!(f, "The SSH agent sent something unexpected: {what}"),
        }
    }
}

/// Ask the agent what keys it is holding.
#[cfg(unix)]
pub fn identities() -> Result<Vec<AgentKey>, AgentError> {
    use std::os::unix::net::UnixStream;

    let path = std::env::var_os("SSH_AUTH_SOCK").ok_or(AgentError::NotRunning)?;
    let mut socket =
        UnixStream::connect(&path).map_err(|error| AgentError::Unreachable(error.to_string()))?;

    // Read and write timeouts, so a wedged agent cannot hang the caller.
    let timeout = Some(std::time::Duration::from_secs(2));
    let _ = socket.set_read_timeout(timeout);
    let _ = socket.set_write_timeout(timeout);

    socket
        .write_all(&[0, 0, 0, 1, REQUEST_IDENTITIES])
        .map_err(|error| AgentError::Unreachable(error.to_string()))?;

    let mut length = [0u8; 4];
    socket
        .read_exact(&mut length)
        .map_err(|error| AgentError::Unreachable(error.to_string()))?;
    let length = u32::from_be_bytes(length);
    if length == 0 || length > MAX_RESPONSE {
        return Err(AgentError::Malformed("implausible response length"));
    }

    let mut payload = vec![0u8; length as usize];
    socket
        .read_exact(&mut payload)
        .map_err(|error| AgentError::Unreachable(error.to_string()))?;

    parse_identities(&payload)
}

#[cfg(not(unix))]
pub fn identities() -> Result<Vec<AgentKey>, AgentError> {
    Err(AgentError::NotRunning)
}

/// Split out from the socket handling so the wire format can be tested
/// directly. Expects the payload with its outer length already stripped.
fn parse_identities(payload: &[u8]) -> Result<Vec<AgentKey>, AgentError> {
    let mut reader = Reader::new(payload);

    if reader.u8().ok_or(AgentError::Malformed("empty response"))? != IDENTITIES_ANSWER {
        return Err(AgentError::Malformed("not an identities answer"));
    }

    let count = reader.u32().ok_or(AgentError::Malformed("missing count"))?;
    if count > 1024 {
        return Err(AgentError::Malformed("implausible key count"));
    }

    let mut keys = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let blob = reader.bytes().ok_or(AgentError::Malformed("short key"))?;
        let comment = reader
            .bytes()
            .ok_or(AgentError::Malformed("short comment"))?;

        // The algorithm name is itself the first length-prefixed string inside
        // the key blob.
        let algorithm = Reader::new(blob)
            .bytes()
            .ok_or(AgentError::Malformed("key blob has no algorithm"))?;

        keys.push(AgentKey {
            algorithm: String::from_utf8_lossy(algorithm).into_owned(),
            comment: String::from_utf8_lossy(comment).into_owned(),
        });
    }
    Ok(keys)
}

/// A cursor that returns `None` rather than panicking on a truncated message.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let slice = self.data.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|slice| slice[0])
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|slice| u32::from_be_bytes(slice.try_into().expect("took exactly four bytes")))
    }

    /// A length-prefixed byte string.
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let length = self.u32()? as usize;
        self.take(length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(out: &mut Vec<u8>, value: &[u8]) {
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(value);
    }

    fn blob(algorithm: &str) -> Vec<u8> {
        let mut out = Vec::new();
        string(&mut out, algorithm.as_bytes());
        string(&mut out, b"key material");
        out
    }

    fn answer(keys: &[(&str, &str)]) -> Vec<u8> {
        let mut out = vec![IDENTITIES_ANSWER];
        out.extend_from_slice(&(keys.len() as u32).to_be_bytes());
        for (algorithm, comment) in keys {
            string(&mut out, &blob(algorithm));
            string(&mut out, comment.as_bytes());
        }
        out
    }

    #[test]
    fn parses_a_normal_answer() {
        let payload = answer(&[("ssh-ed25519", "trevor@desktop"), ("ssh-rsa", "backup key")]);
        let keys = parse_identities(&payload).unwrap();
        assert_eq!(
            keys,
            [
                AgentKey {
                    algorithm: "ssh-ed25519".into(),
                    comment: "trevor@desktop".into()
                },
                AgentKey {
                    algorithm: "ssh-rsa".into(),
                    comment: "backup key".into()
                },
            ]
        );
    }

    #[test]
    fn an_agent_holding_nothing_is_not_an_error() {
        assert_eq!(parse_identities(&answer(&[])).unwrap(), []);
    }

    #[test]
    fn rejects_a_response_that_is_not_an_identities_answer() {
        assert_eq!(
            parse_identities(&[5, 0, 0, 0, 0]),
            Err(AgentError::Malformed("not an identities answer"))
        );
        assert_eq!(
            parse_identities(&[]),
            Err(AgentError::Malformed("empty response"))
        );
    }

    #[test]
    fn truncation_anywhere_is_reported_rather_than_panicking() {
        let full = answer(&[("ssh-ed25519", "trevor@desktop")]);
        for cut in 1..full.len() {
            assert!(
                parse_identities(&full[..cut]).is_err(),
                "truncating to {cut} bytes should fail"
            );
        }
    }

    #[test]
    fn a_lying_length_prefix_cannot_over_read() {
        // Claims one key, then ends.
        let mut payload = vec![IDENTITIES_ANSWER];
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            parse_identities(&payload),
            Err(AgentError::Malformed("short key"))
        );
    }

    #[test]
    fn an_implausible_key_count_is_refused_before_allocating() {
        let mut payload = vec![IDENTITIES_ANSWER];
        payload.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            parse_identities(&payload),
            Err(AgentError::Malformed("implausible key count"))
        );
    }

    #[test]
    fn non_utf8_comments_do_not_fail_the_whole_listing() {
        let mut payload = vec![IDENTITIES_ANSWER];
        payload.extend_from_slice(&1u32.to_be_bytes());
        string(&mut payload, &blob("ssh-ed25519"));
        string(&mut payload, &[0xff, 0xfe]);
        let keys = parse_identities(&payload).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].algorithm, "ssh-ed25519");
    }

    #[test]
    fn describe_is_readable() {
        assert_eq!(
            AgentKey {
                algorithm: "ssh-ed25519".into(),
                comment: "trevor@desktop".into()
            }
            .describe(),
            "ed25519 · trevor@desktop"
        );
        assert_eq!(
            AgentKey {
                algorithm: "ssh-ed25519".into(),
                comment: String::new()
            }
            .describe(),
            "ed25519"
        );
    }
}

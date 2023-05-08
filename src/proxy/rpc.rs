//! Defines the communication protocol between the proxy subprocesses and the parent process.

use anyhow::Context;
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use std::io::Read;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::link_info::LinkInfo;

use super::errors;

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
pub(crate) enum CanContinueResponse {
    /// Used by the controller to indicate that the build process should continue. In the case that
    /// something failed, proceed means that whatever failed should be retried. In the case that
    /// nothing failed, proceed means we can move onto whatever is next. Conceptually, we could
    /// model retry as separate from proceed, however this just adds complexity to the code.
    Proceed,
    Deny,
}

/// A communication channel to the main Cackle process.
pub(crate) struct RpcClient {
    socket_path: PathBuf,
}

impl RpcClient {
    pub(crate) fn new(socket_path: PathBuf) -> Self {
        RpcClient { socket_path }
    }

    /// Advises the parent process that the specified crate uses unsafe.
    pub(crate) fn crate_uses_unsafe(
        &self,
        crate_name: &str,
        error_info: errors::UnsafeUsage,
    ) -> Result<CanContinueResponse> {
        let mut ipc = self.connect()?;
        let request = Request::CrateUsesUnsafe(UnsafeUsage {
            crate_name: crate_name.to_owned(),
            error_info,
        });
        write_to_stream(&request, &mut ipc)?;
        read_from_stream(&mut ipc)
    }

    pub(crate) fn linker_invoked(&self, info: LinkInfo) -> Result<CanContinueResponse> {
        let mut ipc = self.connect()?;
        write_to_stream(&Request::LinkerInvoked(info), &mut ipc)?;
        read_from_stream(&mut ipc)
    }

    pub(crate) fn buid_script_complete(
        &self,
        info: BuildScriptOutput,
    ) -> Result<CanContinueResponse> {
        let mut ipc = self.connect()?;
        write_to_stream(&Request::BuildScriptComplete(info), &mut ipc)?;
        read_from_stream(&mut ipc)
    }

    /// Creates a new connection to the socket. We only send a single request/response on each
    /// connection because it makes things simpler. In general a single request/response is all we
    /// need anyway.
    fn connect(&self) -> Result<UnixStream> {
        UnixStream::connect(&self.socket_path).with_context(|| {
            format!(
                "Failed to connect to socket `{}`",
                self.socket_path.display()
            )
        })
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
pub(crate) enum Request {
    /// Advises that the specified crate failed to compile because it uses unsafe.
    CrateUsesUnsafe(UnsafeUsage),
    LinkerInvoked(LinkInfo),
    BuildScriptComplete(BuildScriptOutput),
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub(crate) struct BuildScriptOutput {
    pub(crate) exit_code: i32,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) package_name: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
pub(crate) struct UnsafeUsage {
    pub(crate) crate_name: String,
    pub(crate) error_info: errors::UnsafeUsage,
}

/// Writes `value` to `stream`. The format used is the length followed by `value` serialised as
/// JSON.
pub(crate) fn write_to_stream<T: Serialize>(value: &T, stream: &mut impl Write) -> Result<()> {
    let serialized = serde_json::to_string(value)?;
    stream.write_all(&serialized.len().to_le_bytes())?;
    stream.write_all(serialized.as_bytes())?;
    Ok(())
}

/// Reads a value of type `T` from `stream`. Format is the same as for `write_to_stream`.
pub(crate) fn read_from_stream<T: DeserializeOwned>(stream: &mut impl Read) -> Result<T> {
    let mut len_bytes = [0u8; std::mem::size_of::<usize>()];
    stream.read_exact(&mut len_bytes)?;
    let len = usize::from_le_bytes(len_bytes);
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    let serialized = std::str::from_utf8(&buf)?;
    serde_json::from_str(serialized).with_context(|| format!("Invalid message `{serialized}`"))
}

impl BuildScriptOutput {
    pub(crate) fn new(
        value: &std::process::Output,
        package_name: String,
        exit_status: &std::process::ExitStatus,
    ) -> Self {
        Self {
            exit_code: exit_status.code().unwrap_or(-1),
            stdout: value.stdout.clone(),
            stderr: value.stderr.clone(),
            package_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_deserialize() {
        let req = Request::CrateUsesUnsafe(UnsafeUsage {
            crate_name: "foo".to_owned(),
            error_info: errors::UnsafeUsage {
                file_name: "src/main.rs".to_owned(),
                start_line: 42,
            },
        });
        let mut buf = Vec::new();
        write_to_stream(&req, &mut buf).unwrap();

        let req2 = read_from_stream(&mut buf.as_slice()).unwrap();

        assert_eq!(req, req2);
    }
}

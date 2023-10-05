//! Defines the communication protocol between the proxy subprocesses and the parent process.

use crate::config::SandboxConfig;
use crate::crate_index::CrateSel;
use crate::link_info::LinkInfo;
use crate::location::SourceLocation;
use crate::outcome::Outcome;
use anyhow::Context;
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use std::io::Read;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

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
        crate_sel: &CrateSel,
        locations: Vec<SourceLocation>,
    ) -> Result<Outcome> {
        let mut ipc = self.connect()?;
        let request = Request::CrateUsesUnsafe(UnsafeUsage {
            crate_sel: crate_sel.clone(),
            locations,
        });
        write_to_stream(&request, &mut ipc)?;
        read_from_stream(&mut ipc)
    }

    pub(crate) fn rustc_started(&self, crate_sel: &CrateSel) -> Result<Outcome> {
        let mut ipc = self.connect()?;
        let request = Request::RustcStarted(crate_sel.clone());
        write_to_stream(&request, &mut ipc)?;
        read_from_stream(&mut ipc)
    }

    pub(crate) fn linker_invoked(&self, info: LinkInfo) -> Result<Outcome> {
        let mut ipc = self.connect()?;
        write_to_stream(&Request::LinkerInvoked(info), &mut ipc)?;
        read_from_stream(&mut ipc)
    }

    pub(crate) fn bin_execution_complete(&self, info: BinExecutionOutput) -> Result<Outcome> {
        let mut ipc = self.connect()?;
        write_to_stream(&Request::BinExecutionComplete(info), &mut ipc)?;
        read_from_stream(&mut ipc)
    }

    pub(crate) fn rustc_complete(&self, info: RustcOutput) -> Result<Outcome> {
        let mut ipc = self.connect()?;
        write_to_stream(&Request::RustcComplete(info), &mut ipc)?;
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
    BinExecutionComplete(BinExecutionOutput),
    RustcStarted(CrateSel),
    RustcComplete(RustcOutput),
}

/// The output from running a binary such as a build script or a test.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub(crate) struct BinExecutionOutput {
    pub(crate) exit_code: i32,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) crate_sel: CrateSel,
    pub(crate) sandbox_config: SandboxConfig,
    pub(crate) binary_path: PathBuf,
    /// A display string for how the sandbox was configured (e.g. the command line). Only present if
    /// the exit code is non-zero.
    pub(crate) sandbox_config_display: Option<String>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub(crate) struct RustcOutput {
    pub(crate) crate_sel: CrateSel,
    pub(crate) source_paths: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub(crate) struct UnsafeUsage {
    pub(crate) crate_sel: CrateSel,
    pub(crate) locations: Vec<SourceLocation>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn serialize_deserialize() {
        let req = Request::CrateUsesUnsafe(UnsafeUsage {
            crate_sel: CrateSel::primary(crate::crate_index::testing::pkg_id("foo")),
            locations: vec![SourceLocation::new(Path::new("src/main.rs"), 42, None)],
        });
        let mut buf = Vec::new();
        write_to_stream(&req, &mut buf).unwrap();

        let req2 = read_from_stream(&mut buf.as_slice()).unwrap();

        assert_eq!(req, req2);
    }
}

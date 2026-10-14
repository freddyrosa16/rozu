//! Private files and approval channel owned by one subscription-provider session.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rozu_core::{ApprovalGate, StreamEvent};
use rozu_mcp::ApprovalBridge;
use tokio::sync::mpsc;
use tracing::warn;

pub(crate) struct ApprovalRuntime {
    // Stop accepting connections before removing the socket directory.
    bridge: Option<ApprovalBridge>,
    directory: tempfile::TempDir,
}

impl ApprovalRuntime {
    pub(crate) fn new(
        prefix: &str,
        provider: &str,
        gate: Arc<dyn ApprovalGate>,
    ) -> io::Result<(Self, mpsc::UnboundedReceiver<StreamEvent>)> {
        // Unix socket paths are limited to about 100 bytes, shorter than macOS's TMPDIR.
        let base = if cfg!(unix) {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let mut builder = tempfile::Builder::new();
        builder.prefix(prefix);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let directory = builder.tempdir_in(base)?;
        let (events, records) = mpsc::unbounded_channel();
        let socket = directory.path().join("approval.sock");
        let bridge = match ApprovalBridge::bind(&socket, provider, gate, events) {
            Ok(bridge) => Some(bridge),
            Err(err) => {
                warn!(provider, error = %err,
                    "no approval bridge; prompts from rozu's MCP tools will be denied");
                None
            }
        };
        Ok((Self { bridge, directory }, records))
    }

    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }

    pub(crate) fn socket_path(&self) -> Option<&Path> {
        self.bridge.as_ref().map(ApprovalBridge::path)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use rozu_core::ReadOnlyGate;

    use super::*;

    #[tokio::test]
    async fn approval_files_are_private_and_removed_with_the_runtime() {
        let (runtime, _records) =
            ApprovalRuntime::new("rozu-runtime-test-", "test", Arc::new(ReadOnlyGate)).unwrap();
        let directory = runtime.path().to_owned();
        let socket = runtime.socket_path().unwrap().to_owned();
        assert_eq!(socket.parent(), Some(directory.as_path()));
        assert!(socket.exists());
        let mode = std::fs::metadata(&directory).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);

        drop(runtime);
        assert!(!socket.exists());
        assert!(!directory.exists());
    }
}

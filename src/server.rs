// SPDX-License-Identifier: GPL-3.0-or-later
//
// Serves the DeviceService on a Unix domain socket.
// Based on the CoolerControl device-service template (GPL-3.0-or-later).

use crate::device_service::v1::device_service_server::DeviceServiceServer;
use crate::service::GigabyteGpuService;
use anyhow::{Context, Result, bail};
use log::{info, warn};
use std::fs;
use std::future::Future;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use tokio::net::UnixListener;
use tonic::codegen::tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

/// Removes a socket left behind by an unclean exit. Refuses to remove anything
/// that is not a socket.
pub fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            warn!("Removing stale socket {}", path.display());
            fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))
        }
        Ok(_) => bail!(
            "{} exists and is not a socket; refusing to replace it",
            path.display()
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("cannot stat {}", path.display())),
    }
}

/// Binds `socket`, serves until `shutdown` resolves, then removes the socket.
pub async fn serve_uds(
    service: GigabyteGpuService,
    socket: &Path,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    remove_stale_socket(socket)?;
    let listener = UnixListener::bind(socket).with_context(|| {
        format!(
            "cannot bind {}. Does the directory exist and is it writable?",
            socket.display()
        )
    })?;
    // Only root (the CoolerControl daemon) may connect.
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot chmod {}", socket.display()))?;
    info!("Listening on {}", socket.display());

    let result = Server::builder()
        .add_service(DeviceServiceServer::new(service))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown)
        .await;
    let _ = fs::remove_file(socket);
    info!("Stopped");
    result.context("gRPC server failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_socket_handling() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        // Missing: fine.
        remove_stale_socket(&sock).unwrap();
        // A real (stale) socket: removed.
        let l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        drop(l);
        assert!(sock.exists());
        remove_stale_socket(&sock).unwrap();
        assert!(!sock.exists());
        // A regular file: refused and kept.
        fs::write(&sock, b"x").unwrap();
        assert!(remove_stale_socket(&sock).is_err());
        assert!(sock.exists());
    }
}

//! The local Unix socket: a private directory, a 0600 socket (0660 when another uid may connect), the peer's uid
//! checked with SO_PEERCRED, one JSON line per request with a size limit and an idle timeout.

use std::fs;
use std::future::Future;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;

use crate::chain::Chain;
use crate::policy::Role;
use crate::vault::{Vault, error};

/// Longest request line accepted, in bytes. A rebalance is about 230 bytes (460 in hex); a 64-note batch about 15 KiB.
pub const MAX_LINE: usize = 256 * 1024;
/// A connection that sends nothing for this long is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Connections served at once; further ones are closed as they arrive.
const MAX_CONNECTIONS: usize = 8;

/// The uid this process runs as. /proc/self is owned by it (Linux; no libc call, so no unsafe code).
pub fn own_uid() -> io::Result<u32> {
    Ok(fs::metadata("/proc/self")?.uid())
}

pub fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// `$XDG_RUNTIME_DIR/strawberryswap-vault/<role>.sock`, or `<tmp>/strawberryswap-vault-<uid>/<role>.sock`.
pub fn default_socket(uid: u32, role: Role) -> PathBuf {
    let file = format!("{}.sock", role.name());
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("strawberryswap-vault").join(file),
        _ => std::env::temp_dir()
            .join(format!("strawberryswap-vault-{uid}"))
            .join(file),
    }
}

/// Binds the socket in a private directory: the directory must belong to this uid and grant nothing to other users
/// (it is created 0700 if missing). The socket is 0600, or 0660 when another uid may connect (that uid then reaches
/// it through the directory's group, set up by the deployment). A stale socket from an earlier run is replaced; a
/// live one, or any other file, is not.
pub fn bind_socket(path: &Path, own_uid: u32, group_access: bool) -> io::Result<UnixListener> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| {
            invalid(format!(
                "{}: the socket needs a parent directory",
                path.display()
            ))
        })?;
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(invalid(format!("{} is not a directory", dir.display())));
            }
            if meta.uid() != own_uid {
                return Err(invalid(format!(
                    "{} belongs to uid {}, not to this process (uid {own_uid})",
                    dir.display(),
                    meta.uid()
                )));
            }
            if meta.mode() & 0o007 != 0 {
                return Err(invalid(format!(
                    "{} is open to other users (mode {:o}): use 0700, or 0750 with a group",
                    dir.display(),
                    meta.mode() & 0o777
                )));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?,
        Err(e) => return Err(e),
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(invalid(format!(
                    "another process is listening on {}",
                    path.display()
                )));
            }
            fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(invalid(format!(
                "{} exists and is not a socket",
                path.display()
            )));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if group_access { 0o660 } else { 0o600 }),
    )?;
    Ok(listener)
}

async fn write_line(write: &mut OwnedWriteHalf, value: &Value) -> io::Result<()> {
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    write.write_all(&bytes).await
}

async fn read_line(
    mut reader: impl tokio::io::AsyncBufRead + Unpin,
    line: &mut Vec<u8>,
) -> io::Result<usize> {
    reader.read_until(b'\n', line).await
}

/// Serves one client: the kernel-reported uid must be the allowed one, then one JSON line per request, each at most
/// MAX_LINE bytes. A line that is too long, or cut short, ends the connection after an error reply.
async fn connection<C: Chain>(stream: UnixStream, vault: &Vault<C>) -> io::Result<()> {
    let uid = stream.peer_cred()?.uid();
    if uid != vault.allowed_uid() {
        eprintln!(
            "audit: refused a connection from uid {uid} (only uid {} may connect)",
            vault.allowed_uid()
        );
        return Ok(());
    }
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
    loop {
        line.clear();
        let limited = (&mut reader).take(MAX_LINE as u64 + 1);
        let n = match timeout(IDLE_TIMEOUT, read_line(limited, &mut line)).await {
            Ok(read) => read?,
            Err(_) => return Ok(()),
        };
        if n == 0 {
            return Ok(());
        }
        if line.last() != Some(&b'\n') {
            let message = if line.len() > MAX_LINE {
                format!("request longer than {MAX_LINE} bytes")
            } else {
                "request not terminated by a newline".to_string()
            };
            return write_line(&mut write, &error(None, "parse", message)).await;
        }
        let response = vault.handle(&line).await;
        write_line(&mut write, &response).await?;
    }
}

/// Accepts connections until `shutdown` completes, at most MAX_CONNECTIONS at a time.
pub async fn serve<C: Chain>(
    listener: UnixListener,
    vault: Arc<Vault<C>>,
    shutdown: impl Future<Output = ()>,
) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        eprintln!("vault: {MAX_CONNECTIONS} connections already open; closing a new one");
                        continue;
                    };
                    let vault = Arc::clone(&vault);
                    tokio::spawn(async move {
                        if let Err(e) = connection(stream, &vault).await {
                            eprintln!("vault: connection ended: {e}");
                        }
                        drop(permit);
                    });
                }
                Err(e) => {
                    eprintln!("vault: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

/// Ctrl-C or SIGTERM (systemd's stop). A handler that cannot be installed never fires.
pub async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::sweepCall;
    use crate::policy::tests::{operator, rebalance_tx};
    use crate::vault::tests::{MockChain, caps, sign_request, vault_with};
    use alloy_consensus::TxEip1559;
    use alloy_primitives::U256;
    use alloy_sol_types::SolCall;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tsig-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Sends one line and reads one reply line ("" when the vault closes without replying). A refused peer may see
    /// its write fail with a broken pipe, which is part of being refused, so write errors are not fatal here.
    async fn ask(path: &Path, line: &[u8]) -> String {
        let mut stream = UnixStream::connect(path).await.unwrap();
        let _ = stream.write_all(&[line, b"\n"].concat()).await;
        let mut reply = String::new();
        let _ = BufReader::new(stream).read_line(&mut reply).await;
        reply
    }

    fn vault_for(uid: u32) -> Arc<Vault<Arc<MockChain>>> {
        let mut vault = vault_with(operator(), 20.0, caps(), Arc::new(MockChain::default()));
        vault.set_allowed_uid(uid);
        Arc::new(vault)
    }

    #[tokio::test]
    async fn the_socket_is_private_and_serves_the_protocol() {
        let dir = TempDir::new("serve");
        let path = dir.0.join("operator.sock");
        let uid = own_uid().unwrap();
        let listener = bind_socket(&path, uid, false).unwrap();
        assert_eq!(fs::metadata(&dir.0).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::symlink_metadata(&path).unwrap().mode() & 0o777, 0o600);

        let vault = vault_for(uid);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(listener, Arc::clone(&vault), async {
            let _ = stopped.await;
        }));

        let reply: Value =
            serde_json::from_str(&ask(&path, br#"{"id":1,"method":"address"}"#).await).unwrap();
        assert_eq!(reply["result"]["address"], vault.address().to_string());

        let tx = TxEip1559 {
            nonce: 0,
            ..rebalance_tx()
        };
        let reply: Value = serde_json::from_str(&ask(&path, &sign_request(2, &tx)).await).unwrap();
        assert!(reply["result"]["signed_tx"].is_string(), "{reply}");
        let sweep = TxEip1559 {
            input: sweepCall {
                token: Default::default(),
                to: Default::default(),
                amount: U256::ZERO,
            }
            .abi_encode()
            .into(),
            ..tx
        };
        let reply: Value =
            serde_json::from_str(&ask(&path, &sign_request(3, &sweep)).await).unwrap();
        assert_eq!(reply["error"]["code"], "policy");

        // A line longer than MAX_LINE gets one error and the connection is closed.
        let long = vec![b'x'; MAX_LINE + 10];
        let reply: Value = serde_json::from_str(&ask(&path, &long).await).unwrap();
        assert_eq!(reply["error"]["code"], "parse");

        // A second vault refuses to take over a live socket; files that are not sockets are left alone.
        assert!(
            bind_socket(&path, uid, false)
                .unwrap_err()
                .to_string()
                .contains("another process is listening")
        );
        let file = dir.0.join("not-a-socket");
        fs::write(&file, b"").unwrap();
        assert!(
            bind_socket(&file, uid, false)
                .unwrap_err()
                .to_string()
                .contains("not a socket")
        );

        stop.send(()).unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_peer_with_another_uid_is_refused() {
        let dir = TempDir::new("uid");
        let path = dir.0.join("operator.sock");
        let uid = own_uid().unwrap();
        let listener = bind_socket(&path, uid, false).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(listener, vault_for(uid.wrapping_add(1)), async {
            let _ = stopped.await;
        }));
        // The connection is closed without a reply.
        assert_eq!(ask(&path, br#"{"id":1,"method":"address"}"#).await, "");
        stop.send(()).unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_directory_open_to_other_users_is_refused() {
        let dir = TempDir::new("open");
        fs::create_dir_all(&dir.0).unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o777)).unwrap();
        let err = bind_socket(&dir.0.join("operator.sock"), own_uid().unwrap(), false).unwrap_err();
        assert!(err.to_string().contains("open to other users"), "{err}");
    }

    #[test]
    fn each_role_has_its_own_default_socket() {
        let paths: Vec<PathBuf> = Role::ALL.iter().map(|&r| default_socket(1000, r)).collect();
        assert!(paths[0].ends_with("operator.sock") && paths[2].ends_with("automator.sock"));
        assert_ne!(paths[0], paths[1]);
    }
}

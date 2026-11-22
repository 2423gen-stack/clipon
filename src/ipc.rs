//! Unix ドメインソケット (UDS) を用いた単一インスタンス制御および IPC 通信モジュール。

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::Duration;

/// クライアント接続の読み書きタイムアウト。
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(2);
/// 1行あたりに許容する最大バイト長。
const MAX_COMMAND_LINE_BYTES: usize = 1024;

/// IPC 経由でデーモンへ送信するコマンド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcCommand {
    /// ポップアップウィンドウを表示する。
    ShowPopup,
    /// 設定ウィンドウを表示する。
    ShowSettings,
    /// デーモンを終了する。
    Quit,
}

impl IpcCommand {
    /// ワイヤプロトコル用の文字列表現を返す。
    pub fn to_wire(&self) -> &'static str {
        match self {
            Self::ShowPopup => "SHOW_POPUP",
            Self::ShowSettings => "SHOW_SETTINGS",
            Self::Quit => "QUIT",
        }
    }

    /// ワイヤプロトコル文字列から [`IpcCommand`] を復元する。
    pub fn from_wire(s: &str) -> Option<Self> {
        match s.trim() {
            "SHOW_POPUP" => Some(Self::ShowPopup),
            "SHOW_SETTINGS" => Some(Self::ShowSettings),
            "QUIT" => Some(Self::Quit),
            _ => None,
        }
    }
}

/// 既定の IPC ソケットパス（`$XDG_RUNTIME_DIR/clipon.sock` または `/tmp/clipon-<uid>.sock`）を返す。
pub fn default_socket_path() -> PathBuf {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !runtime_dir.is_empty() {
            return PathBuf::from(runtime_dir).join("clipon.sock");
        }
    }
    let uid = std::fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/clipon-{uid}.sock"))
}

/// 指定したソケットパスへ IPC コマンドを送信する。
pub fn send_command_to(path: &Path, cmd: IpcCommand) -> Result<(), String> {
    let mut stream = UnixStream::connect(path)
        .map_err(|e| format!("IPC ソケット ({}) への接続に失敗しました: {e}", path.display()))?;
    let _ = stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT));
    writeln!(stream, "{}", cmd.to_wire())
        .map_err(|e| format!("IPC コマンドの送信に失敗しました: {e}"))?;
    stream
        .flush()
        .map_err(|e| format!("IPC ストリームのフラッシュに失敗しました: {e}"))?;
    Ok(())
}

/// UDS 上でコマンドを待ち受ける IPC サーバー。
pub struct IpcServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl IpcServer {
    /// 指定パスへソケットをバインドする。稼働中のインスタンスが存在する場合は `Err` を返す。
    pub fn bind(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    format!(
                        "ソケット親ディレクトリ ({}) の作成に失敗しました: {e}",
                        parent.display()
                    )
                })?;
            }
        }

        // 既存ファイルが残っている場合、接続を試みて生存中か残留ソケットかを判定する。
        if std::fs::symlink_metadata(path).is_ok() {
            if UnixStream::connect(path).is_ok() {
                return Err("already running".to_string());
            }
            std::fs::remove_file(path).map_err(|e| {
                format!(
                    "残留ソケットファイル ({}) の削除に失敗しました: {e}",
                    path.display()
                )
            })?;
        }

        let listener = UnixListener::bind(path).map_err(|e| {
            format!(
                "IPC ソケット ({}) のバインドに失敗しました: {e}",
                path.display()
            )
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            let _ = std::fs::remove_file(path);
            format!(
                "IPC ソケット ({}) のパーミッション設定 (0600) に失敗しました: {e}",
                path.display()
            )
        })?;

        Ok(Self {
            listener,
            socket_path: path.to_path_buf(),
        })
    }

    /// バックグラウンドスレッドでコマンド受信ループを開始する。`Quit` 受信で終了する。
    pub fn spawn_listener(self, on_cmd: impl Fn(IpcCommand) + Send + 'static) -> JoinHandle<()> {
        std::thread::spawn(move || {
            for stream_res in self.listener.incoming() {
                let Ok(stream) = stream_res else {
                    break;
                };
                if let Some(cmd) = read_command_from_stream(stream) {
                    on_cmd(cmd);
                    if cmd == IpcCommand::Quit {
                        break;
                    }
                }
            }
        })
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// 接続されたストリームからタイムアウトおよび最大長制限付きで1コマンドを読み取る。
fn read_command_from_stream(stream: UnixStream) -> Option<IpcCommand> {
    let _ = stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT));
    let mut reader = BufReader::new(stream.take(MAX_COMMAND_LINE_BYTES as u64));
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => IpcCommand::from_wire(&line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tempfile::TempDir;

    #[test]
    fn wire_protocol_roundtrip() {
        for cmd in [
            IpcCommand::ShowPopup,
            IpcCommand::ShowSettings,
            IpcCommand::Quit,
        ] {
            let wire = cmd.to_wire();
            assert_eq!(IpcCommand::from_wire(wire), Some(cmd));
            assert_eq!(IpcCommand::from_wire(&format!("{wire}\n")), Some(cmd));
        }
        assert_eq!(IpcCommand::from_wire("UNKNOWN_CMD"), None);
        assert_eq!(IpcCommand::from_wire(""), None);
    }

    #[test]
    fn ipc_server_receives_commands_and_detects_running_instance() {
        let dir = TempDir::new().expect("TempDir の作成に失敗");
        let sock_path = dir.path().join("clipon.sock");

        let server = IpcServer::bind(&sock_path).expect("初回バインドに失敗");
        let dup_err = IpcServer::bind(&sock_path)
            .err()
            .expect("稼働中ソケットへの2重バインドはエラーになるべき");
        assert!(dup_err.contains("already running"), "unexpected error: {dup_err}");

        let (tx, rx) = mpsc::channel();
        let handle = server.spawn_listener(move |cmd| {
            let _ = tx.send(cmd);
        });

        send_command_to(&sock_path, IpcCommand::ShowPopup).expect("ShowPopup 送信に失敗");
        send_command_to(&sock_path, IpcCommand::ShowSettings).expect("ShowSettings 送信に失敗");
        send_command_to(&sock_path, IpcCommand::Quit).expect("Quit 送信に失敗");

        handle.join().expect("リスナースレッドの join に失敗");

        let received: Vec<IpcCommand> = rx.try_iter().collect();
        assert_eq!(
            received,
            vec![
                IpcCommand::ShowPopup,
                IpcCommand::ShowSettings,
                IpcCommand::Quit
            ]
        );
        assert!(
            !sock_path.exists(),
            "Quit 終了後の IpcServer ドロップでソケットファイルが削除されるべき"
        );
    }

    #[test]
    fn ipc_server_cleans_stale_socket_file() {
        let dir = TempDir::new().expect("TempDir の作成に失敗");
        let sock_path = dir.path().join("stale.sock");

        // 誰も listen していない通常ファイルを残留ソケットとして配置する。
        std::fs::write(&sock_path, b"stale").expect("ダミーファイルの書き込みに失敗");
        assert!(sock_path.exists());

        let server = IpcServer::bind(&sock_path).expect("残留ソケットの掃除後にバインドできるべき");
        drop(server);
        assert!(
            !sock_path.exists(),
            "IpcServer ドロップ時にソケットファイルが削除されるべき"
        );
    }

    #[test]
    fn ipc_socket_permissions_are_strictly_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().expect("TempDir の作成に失敗");
        let sock_path = dir.path().join("secure.sock");
        let _server = IpcServer::bind(&sock_path).expect("バインドに失敗");

        let mode = std::fs::metadata(&sock_path)
            .expect("ソケットメタデータの取得に失敗")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "IPC ソケットのパーミッションは所有者限定の 0600 であるべき (actual: {mode:04o})"
        );
    }
}

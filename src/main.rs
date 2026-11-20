//! ClipOn デーモンおよび CLI エントリポイント。

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui;

use clipon::clipboard::ClipboardHandle;
use clipon::config::AppConfig;
use clipon::gui::{run_gui, spawn_paste_worker, AppState, UiCommand};
use clipon::history::HistoryStore;
use clipon::ipc::{default_socket_path, send_command_to, IpcCommand, IpcServer};
use clipon::keyhook::KeyHookHandle;
use clipon::tray::{TrayAction, TrayHandle};

/// CLI 引数から解釈された実行アクション。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliAction {
    /// デーモンを起動する（既に稼働中ならポップアップ表示を指示して終了）。
    Daemon,
    /// ポップアップを表示する（未起動ならデーモンを起動して即表示）。
    ShowPopup,
    /// 設定画面を表示する（未起動ならデーモンを起動して設定表示）。
    ShowSettings,
    /// 稼働中のデーモンへ終了を指示する。
    Quit,
    /// ヘルプメッセージを表示する。
    Help,
}

/// GUI スレッドへコマンド送信と即時再描画要求を同時に届ける軽量通知ハンドル。
#[derive(Clone)]
struct UiNotifier {
    ui_tx: mpsc::Sender<UiCommand>,
    ctx_slot: Arc<Mutex<Option<egui::Context>>>,
}

impl UiNotifier {
    /// GUI スレッドへコマンドを積み、`egui::Context` が確立済みであれば即時再描画を要求する。
    fn send(&self, cmd: UiCommand) {
        let _ = self.ui_tx.send(cmd);
        if let Ok(guard) = self.ctx_slot.lock() {
            if let Some(ctx) = guard.as_ref() {
                ctx.request_repaint();
            }
        }
    }
}

/// CLI 引数イテレータ（先頭プログラム名を含む）を解析する。
fn parse_cli_args<I, S>(args: I) -> Result<CliAction, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter();
    // 先頭の実行ファイル名をスキップする。
    let _program = iter.next();

    let Some(first) = iter.next() else {
        return Ok(CliAction::Daemon);
    };

    if iter.next().is_some() {
        return Err("同時に指定できるオプションは 1 つだけです".to_string());
    }

    match first.as_ref() {
        "--toggle" | "--show" => Ok(CliAction::ShowPopup),
        "--settings" => Ok(CliAction::ShowSettings),
        "--quit" => Ok(CliAction::Quit),
        "--help" | "-h" => Ok(CliAction::Help),
        other => Err(format!("不明なオプションです: {other}")),
    }
}

/// 標準出力へ簡潔な CLI ヘルプを表示する。
fn print_help() {
    println!(
        "\
ClipOn - 超軽量クリップボードランチャー

使い方:
  clipon              デーモンを起動（起動済みならポップアップを表示）
  clipon --toggle     ポップアップを表示（未起動ならデーモンを起動して表示）
  clipon --show       --toggle と同じ
  clipon --settings   設定画面を表示（未起動ならデーモンを起動して設定表示）
  clipon --quit       稼働中の ClipOn デーモンを終了
  clipon --help       このヘルプを表示"
    );
}

/// IPC サーバーの獲得を試み、既に稼働中であれば該当コマンドを送信して `Ok(None)` を返す。
fn acquire_or_signal_daemon(
    socket_path: &std::path::Path,
    action: CliAction,
    ipc_cmd: IpcCommand,
) -> Result<Option<IpcServer>, String> {
    if action != CliAction::Daemon && send_command_to(socket_path, ipc_cmd).is_ok() {
        return Ok(None);
    }

    match IpcServer::bind(socket_path) {
        Ok(server) => Ok(Some(server)),
        Err(err) if err.contains("already running") => {
            send_command_to(socket_path, ipc_cmd)?;
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// バックグラウンドワーカー群と常駐 GUI を配線してデーモンを実行する。
fn run_daemon(
    ipc_server: IpcServer,
    socket_path: PathBuf,
    initial_ui_cmd: Option<UiCommand>,
) -> Result<(), String> {
    let config_path = AppConfig::default_config_path();
    let cache_dir = AppConfig::default_cache_dir();
    let config = match AppConfig::load_from_path(&config_path) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("ClipOn 警告: 設定読込に失敗したため既定値を使用します: {err}");
            AppConfig::default()
        }
    };

    let history = Arc::new(Mutex::new(HistoryStore::new(config.history.max_items)));
    let (ui_tx, ui_rx) = mpsc::channel::<UiCommand>();
    let (ctx_tx, ctx_rx) = mpsc::channel::<egui::Context>();
    let ctx_slot = Arc::new(Mutex::new(None));

    // GUI 初期化時に届いた egui::Context を共有スロットへ格納し、先行コマンドがあれば即描画させる。
    let bridge_slot = Arc::clone(&ctx_slot);
    let ctx_bridge_handle = std::thread::Builder::new()
        .name("clipon-ctx-bridge".to_string())
        .spawn(move || {
            if let Ok(ctx) = ctx_rx.recv() {
                if let Ok(mut guard) = bridge_slot.lock() {
                    *guard = Some(ctx.clone());
                }
                ctx.request_repaint();
            }
        })
        .map_err(|e| format!("コンテキスト連携スレッドの起動に失敗しました: {e}"))?;

    let notifier = UiNotifier {
        ui_tx,
        ctx_slot: Arc::clone(&ctx_slot),
    };

    let history_for_cb = Arc::clone(&history);
    let notifier_for_cb = notifier.clone();
    let clipboard = ClipboardHandle::spawn(move |content| {
        let updated = if let Ok(mut guard) = history_for_cb.lock() {
            guard.push(content)
        } else {
            false
        };
        if updated {
            notifier_for_cb.send(UiCommand::RefreshHistory);
        }
    })?;

    let (paste_tx, paste_handle) = spawn_paste_worker();

    let notifier_for_key = notifier.clone();
    let keyhook = KeyHookHandle::spawn(config.trigger.clone(), move || {
        notifier_for_key.send(UiCommand::ShowPopup { anchor: None });
    });

    let notifier_for_tray = notifier.clone();
    let tray_handle = match TrayHandle::spawn(move |action| match action {
        TrayAction::ShowPopup => notifier_for_tray.send(UiCommand::ShowPopup { anchor: None }),
        TrayAction::ShowSettings => notifier_for_tray.send(UiCommand::ShowSettings),
        TrayAction::Quit => notifier_for_tray.send(UiCommand::Quit),
    }) {
        Ok(handle) => Some(handle),
        Err(err) => {
            eprintln!("ClipOn 警告: システムトレイを初期化できませんでした（継続します）: {err}");
            None
        }
    };

    let notifier_for_ipc = notifier.clone();
    let ipc_handle = ipc_server.spawn_listener(move |cmd| match cmd {
        IpcCommand::ShowPopup => notifier_for_ipc.send(UiCommand::ShowPopup { anchor: None }),
        IpcCommand::ShowSettings => notifier_for_ipc.send(UiCommand::ShowSettings),
        IpcCommand::Quit => notifier_for_ipc.send(UiCommand::Quit),
    });

    if let Some(cmd) = initial_ui_cmd {
        notifier.send(cmd);
    }

    let shared = Arc::new(AppState {
        config: Mutex::new(config),
        config_path,
        cache_dir,
        history,
        clipboard,
        keyhook,
        paste_tx: Mutex::new(Some(paste_tx)),
    });

    let gui_result = run_gui(Arc::clone(&shared), ui_rx, ctx_tx);

    // GUI 終了後は全バックグラウンドリソースを順序よくクリーンに停止する。
    if let Some(tray) = tray_handle {
        tray.shutdown();
    }
    shared.keyhook.shutdown();
    shared.clipboard.shutdown();

    // トレイ等からの終了時にも IPC リスナーを確実に抜けてソケットファイルを削除させる。
    let _ = send_command_to(&socket_path, IpcCommand::Quit);
    let _ = ipc_handle.join();

    if let Ok(mut guard) = shared.paste_tx.lock() {
        if let Some(tx) = guard.take() {
            let _ = tx.send(None);
        }
    }
    let _ = paste_handle.join();
    let _ = ctx_bridge_handle.join();

    gui_result
}

fn main() -> ExitCode {

    let action = match parse_cli_args(std::env::args()) {
        Ok(act) => act,
        Err(err) => {
            eprintln!("ClipOn エラー: {err}");
            print_help();
            return ExitCode::FAILURE;
        }
    };

    if action == CliAction::Help {
        print_help();
        return ExitCode::SUCCESS;
    }

    let socket_path = default_socket_path();

    if action == CliAction::Quit {
        match send_command_to(&socket_path, IpcCommand::Quit) {
            Ok(()) => return ExitCode::SUCCESS,
            Err(_) => {
                println!("ClipOn は現在起動していません。");
                return ExitCode::SUCCESS;
            }
        }
    }

    let (ipc_cmd, initial_ui_cmd) = match action {
        CliAction::Daemon => (IpcCommand::ShowPopup, None),
        CliAction::ShowPopup => (
            IpcCommand::ShowPopup,
            Some(UiCommand::ShowPopup { anchor: None }),
        ),
        CliAction::ShowSettings => (IpcCommand::ShowSettings, Some(UiCommand::ShowSettings)),
        CliAction::Quit | CliAction::Help => unreachable!(),
    };

    let server = match acquire_or_signal_daemon(&socket_path, action, ipc_cmd) {
        Ok(Some(srv)) => srv,
        Ok(None) => return ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ClipOn エラー: IPC 初期化に失敗しました: {err}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(err) = run_daemon(server, socket_path, initial_ui_cmd) {
        eprintln!("ClipOn エラー: {err}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cli_args_handles_all_supported_flags() {
        assert_eq!(parse_cli_args(["clipon"]), Ok(CliAction::Daemon));
        assert_eq!(
            parse_cli_args(["clipon", "--toggle"]),
            Ok(CliAction::ShowPopup)
        );
        assert_eq!(
            parse_cli_args(["clipon", "--show"]),
            Ok(CliAction::ShowPopup)
        );
        assert_eq!(
            parse_cli_args(["clipon", "--settings"]),
            Ok(CliAction::ShowSettings)
        );
        assert_eq!(parse_cli_args(["clipon", "--quit"]), Ok(CliAction::Quit));
        assert_eq!(parse_cli_args(["clipon", "--help"]), Ok(CliAction::Help));
        assert_eq!(parse_cli_args(["clipon", "-h"]), Ok(CliAction::Help));

        assert!(parse_cli_args(["clipon", "--unknown"]).is_err());
        assert!(parse_cli_args(["clipon", "--show", "--quit"]).is_err());
    }
}

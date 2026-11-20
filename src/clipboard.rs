//! X11 (XFixes) イベント駆動クリップボード監視および所有権エンジンモジュール。

use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use image::{ImageFormat, ImageReader};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xfixes::{self, ConnectionExt as XFixesConnectionExt};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent,
    ConnectionExt as XProtoConnectionExt, CreateWindowAux, EventMask, GetPropertyReply,
    GetPropertyType, PropMode, Property, SelectionNotifyEvent, SelectionRequestEvent, Timestamp,
    Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;

use crate::config::{FileMode, ImageMode};
use crate::history::{ensure_cached_image_path, encode_uri_list, parse_uri_list, ClipContent};

/// 通常の `SelectionNotify` 応答を待機する最大時間。
const SELECTION_READ_TIMEOUT: Duration = Duration::from_millis(1500);
/// `INCR` 受信中に無通信が続いた場合のアイドルタイムアウト。
const INCR_IDLE_TIMEOUT: Duration = Duration::from_millis(1500);
/// `INCR` 受信全体の最大許容時間。
const INCR_TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
/// 停滞した `INCR` 送信状態を破棄するまでのタイムアウト。
const INCR_SEND_STALE_TIMEOUT: Duration = Duration::from_secs(15);
/// `query_pointer` コマンド応答の最大待機時間。
const POINTER_QUERY_TIMEOUT: Duration = Duration::from_millis(500);
/// 待機ループ内でイベントが空のときに挟む短時間スリープ。
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// クリップボード受信データの最大許容サイズ（32 MiB）。
const MAX_SELECTION_BYTES: usize = 32 * 1024 * 1024;
/// `TARGETS` 取得時の最大要素数（32-bit アトム単位）。
const MAX_TARGETS_WORDS: u32 = 1024;
/// `ChangeProperty` リクエストヘッダおよびアラインメント用の安全マージン。
const X11_REQUEST_HEADER_MARGIN: usize = 256;
/// 分割送信時の最小チャンクサイズ。
const MIN_INCR_CHUNK_BYTES: usize = 4096;

/// 標準および主要なクリップボード MIME ターゲット名。
const MIME_UTF8_STRING: &str = "UTF8_STRING";
const MIME_TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";
const MIME_STRING: &str = "STRING";
const MIME_TEXT_URI_LIST: &str = "text/uri-list";
const MIME_GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";
const MIME_IMAGE_PNG: &str = "image/png";

x11rb::atom_manager! {
    /// クリップボード監視および提供に使用する X11 アトム群。
    Atoms: AtomsCookie {
        CLIPBOARD,
        TARGETS,
        TIMESTAMP,
        INCR,
        UTF8_STRING,
        TEXT_PLAIN_UTF8: b"text/plain;charset=utf-8",
        STRING,
        TEXT_URI_LIST: b"text/uri-list",
        GNOME_COPIED_FILES: b"x-special/gnome-copied-files",
        IMAGE_PNG: b"image/png",
        CLIPON_SELECTION_PROP: b"CLIPON_SELECTION_DATA",
        CLIPON_WAKE: b"CLIPON_WAKE",
    }
}

impl Atoms {
    /// MIME ターゲット名文字列に対応する X11 アトムを返す。
    fn atom_for_mime(&self, mime: &str) -> Option<Atom> {
        match mime {
            MIME_UTF8_STRING => Some(self.UTF8_STRING),
            MIME_TEXT_PLAIN_UTF8 => Some(self.TEXT_PLAIN_UTF8),
            MIME_STRING => Some(self.STRING),
            MIME_TEXT_URI_LIST => Some(self.TEXT_URI_LIST),
            MIME_GNOME_COPIED_FILES => Some(self.GNOME_COPIED_FILES),
            MIME_IMAGE_PNG => Some(self.IMAGE_PNG),
            _ => None,
        }
    }
}

/// クリップボードへ設定する送信ペイロード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutgoingPayload {
    /// プレーンテキスト。
    Text(String),
    /// ローカルファイルパス一覧と転送モード。
    Files {
        /// コピー対象のパス一覧。
        paths: Vec<PathBuf>,
        /// パス文字列または URI リストとしての転送モード。
        mode: FileMode,
    },
    /// PNG 画像データと転送モード。
    Image {
        /// PNG バイナリデータ。
        png_bytes: Vec<u8>,
        /// 画像コンテンツのハッシュ値。
        hash: u64,
        /// 画像バイナリまたは一時パスとしての転送モード。
        mode: ImageMode,
        /// 一時パス保存先のキャッシュディレクトリ。
        cache_dir: PathBuf,
    },
}

/// X11 セレクションとして提供するために構築されたターゲットとデータ列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSelection {
    /// `(MIMEターゲット名, バイト列)` のペア一覧。
    pub targets: Vec<(&'static str, Vec<u8>)>,
}

/// [`OutgoingPayload`] から X11 で提供する MIME ターゲット一覧を構築する。
pub fn build_selection_payload(payload: &OutgoingPayload) -> Result<PreparedSelection, String> {
    let targets = match payload {
        OutgoingPayload::Text(text) => build_text_targets(text.as_bytes()),
        OutgoingPayload::Files { paths, mode } => {
            if paths.is_empty() {
                return Err("コピー対象のファイルパス一覧が空です".to_string());
            }
            match mode {
                FileMode::Path => {
                    let joined = paths
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("\n");
                    build_text_targets(joined.as_bytes())
                }
                FileMode::File => {
                    let uri_list = encode_uri_list(paths);
                    let gnome_uris = uri_list.lines().collect::<Vec<_>>().join("\n");
                    let gnome_copied = format!("copy\n{gnome_uris}");
                    vec![
                        (MIME_TEXT_URI_LIST, uri_list.into_bytes()),
                        (MIME_GNOME_COPIED_FILES, gnome_copied.into_bytes()),
                    ]
                }
            }
        }
        OutgoingPayload::Image {
            png_bytes,
            hash,
            mode,
            cache_dir,
        } => {
            if png_bytes.is_empty() {
                return Err("コピー対象の画像データが空です".to_string());
            }
            match mode {
                ImageMode::Image => vec![(MIME_IMAGE_PNG, png_bytes.clone())],
                ImageMode::Path => {
                    let cached_path = ensure_cached_image_path(cache_dir, *hash, png_bytes)?;
                    let path_text = cached_path.to_string_lossy().into_owned();
                    build_text_targets(path_text.as_bytes())
                }
            }
        }
    };

    Ok(PreparedSelection { targets })
}

/// プレーンテキスト向けの標準 MIME ターゲット一覧を生成する。
fn build_text_targets(bytes: &[u8]) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (MIME_UTF8_STRING, bytes.to_vec()),
        (MIME_TEXT_PLAIN_UTF8, bytes.to_vec()),
        (MIME_STRING, bytes.to_vec()),
    ]
}

/// PNG バイト列のヘッダを解析して `(width, height)` を高速取得する。
pub fn parse_png_dimensions(png_bytes: &[u8]) -> Option<(u32, u32)> {
    let reader = ImageReader::with_format(Cursor::new(png_bytes), ImageFormat::Png);
    reader
        .into_dimensions()
        .ok()
        .filter(|&(w, h)| w > 0 && h > 0)
}

/// バックグラウンドスレッドへ送る操作コマンド。
enum ClipboardCommand {
    /// 指定されたセレクションで `CLIPBOARD` 所有権を取得する。
    SetContent(PreparedSelection),
    /// 現在のマウスカーソル座標を取得する。
    QueryPointer(mpsc::Sender<Option<(i32, i32)>>),
    /// スレッドを安全に停止する。
    Shutdown,
}

/// 現在アプリが所有している `CLIPBOARD` セレクションデータ。
#[derive(Debug, Clone)]
struct ActiveSelection {
    /// 応答可能な全アトム一覧（`TARGETS`, `TIMESTAMP` + MIME 群）。
    supported_targets: Vec<Atom>,
    /// MIME アトムごとの応答バイト列。
    entries: Vec<(Atom, Vec<u8>)>,
    /// 所有権を取得したサーバー時刻。
    ownership_time: Timestamp,
}

impl ActiveSelection {
    /// [`PreparedSelection`] を X11 アトム解決済みの [`ActiveSelection`] へ変換する。
    fn from_prepared(
        prepared: PreparedSelection,
        atoms: &Atoms,
        ownership_time: Timestamp,
    ) -> Self {
        let mut supported_targets = Vec::with_capacity(prepared.targets.len() + 2);
        supported_targets.push(atoms.TARGETS);
        supported_targets.push(atoms.TIMESTAMP);

        let mut entries = Vec::with_capacity(prepared.targets.len());
        for (mime, data) in prepared.targets {
            if let Some(atom) = atoms.atom_for_mime(mime) {
                if !supported_targets.contains(&atom) {
                    supported_targets.push(atom);
                }
                entries.push((atom, data));
            }
        }

        Self {
            supported_targets,
            entries,
            ownership_time,
        }
    }
}

/// `SelectionRequest` に対する応答方針。
#[derive(Debug, PartialEq, Eq)]
enum SelectionReplyPlan<'a> {
    /// `TARGETS` 要求に対するアトム一覧応答。
    Targets(&'a [Atom]),
    /// `TIMESTAMP` 要求に対する取得時刻応答。
    Timestamp(Timestamp),
    /// 一致する MIME ターゲットの実データ応答。
    Data { target: Atom, bytes: &'a [u8] },
    /// 対象外または未保持による拒否応答（`property = NONE`）。
    Reject,
}

/// `SelectionRequest` に対して返す内容を決定する純粋関数。
fn plan_selection_reply<'a>(
    active: Option<&'a ActiveSelection>,
    atoms: &Atoms,
    req_selection: Atom,
    req_target: Atom,
) -> SelectionReplyPlan<'a> {
    if req_selection != atoms.CLIPBOARD {
        return SelectionReplyPlan::Reject;
    }
    let Some(selection) = active else {
        return SelectionReplyPlan::Reject;
    };

    if req_target == atoms.TARGETS {
        return SelectionReplyPlan::Targets(&selection.supported_targets);
    }
    if req_target == atoms.TIMESTAMP {
        return SelectionReplyPlan::Timestamp(selection.ownership_time);
    }

    selection
        .entries
        .iter()
        .find(|(atom, _)| *atom == req_target)
        .map(|(atom, bytes)| SelectionReplyPlan::Data {
            target: *atom,
            bytes: bytes.as_slice(),
        })
        .unwrap_or(SelectionReplyPlan::Reject)
}

/// ICCCM に従い、`property == NONE` の場合は `target` を格納先プロパティとして補完する。
fn effective_target_property(property: Atom, target: Atom) -> Atom {
    if property == x11rb::NONE {
        target
    } else {
        property
    }
}

/// `TARGETS` に含まれるアトム列から、テキスト取得に用いる優先ターゲット順を抽出する。
fn select_text_targets(available_targets: &[Atom], atoms: &Atoms) -> Vec<Atom> {
    let preferred = [atoms.UTF8_STRING, atoms.TEXT_PLAIN_UTF8, atoms.STRING];
    preferred
        .into_iter()
        .filter(|target| available_targets.contains(target))
        .collect()
}

/// 取得したテキストバイト列をターゲット種別に応じて UTF-8 文字列へ変換する。
fn decode_text_bytes(target: Atom, bytes: &[u8], atoms: &Atoms) -> String {
    if target == atoms.STRING {
        match String::from_utf8(bytes.to_vec()) {
            Ok(utf8) => utf8,
            Err(err) => err.into_bytes().into_iter().map(char::from).collect(),
        }
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// `INCR` プロトコルによる分割送信の進行状態。
struct IncrSendState {
    target: Atom,
    data: Vec<u8>,
    offset: usize,
    chunk_size: usize,
    last_activity: Instant,
}

/// イベントディスパッチの結果としてワーカーの状態変化を示す列挙型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchOutcome {
    /// 通常のイベントまたは非破壊的コマンド（`QueryPointer` 等）のみを処理した。
    Continue,
    /// `SetContent` または `Shutdown` が処理されたため、進行中の読み取りを中断すべき。
    AbortPendingRead,
}

/// 待機ループのイベント横取り判定。
enum InterceptResult<T> {
    /// 目的のイベントが完結し、待機ループを終了する。
    Finish(T),
    /// チャンク更新などを処理し、待機を継続する（`reset_idle` が真ならアイドル期限を延長）。
    Handled { reset_idle: bool },
    /// 待機対象外のイベントのため共通ディスパッチへ委譲する。
    Forward(Event),
}

/// 共通タイムアウト待機ループの結果。
enum WaitLoopOutcome<T> {
    /// 待機対象イベントにより値が確定した。
    Matched(T),
    /// タイムアウトにより終了した。
    TimedOut,
    /// コマンド割り込みまたは接続切断により中断された。
    Aborted,
}

/// 外部セレクションから指定ターゲットを読み取った結果。
enum FetchOutcome {
    /// 正常にバイト列を取得した。
    Data(Vec<u8>),
    /// 拒否・タイムアウト・上限超過などで取得できなかった。
    Unavailable,
    /// コマンド割り込みまたはシャットダウンにより読み取りを中断した。
    Aborted,
}

/// バックグラウンドの X11 クリップボード監視・所有権スレッドへの操作ハンドル。
pub struct ClipboardHandle {
    cmd_tx: Mutex<mpsc::Sender<ClipboardCommand>>,
    wake_conn: Mutex<RustConnection>,
    self_window: Window,
    wake_atom: Atom,
    shutdown_requested: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl ClipboardHandle {
    /// X11 へ接続し、クリップボード監視および所有権提供スレッドを起動する。
    pub fn spawn(on_new_content: impl Fn(ClipContent) + Send + 'static) -> Result<Self, String> {
        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|e| format!("X11 サーバーへの接続に失敗しました: {e}"))?;
        let (wake_conn, _) = x11rb::connect(None)
            .map_err(|e| format!("X11 ウェイクアップ接続の確立に失敗しました: {e}"))?;

        conn.prefetch_maximum_request_bytes();

        let atoms = Atoms::new(&conn)
            .map_err(|e| format!("X11 アトム登録リクエストに失敗しました: {e}"))?
            .reply()
            .map_err(|e| format!("X11 アトム登録の応答取得に失敗しました: {e}"))?;

        xfixes::query_version(&conn, 5, 0)
            .map_err(|e| format!("XFixes バージョン照会に失敗しました: {e}"))?
            .reply()
            .map_err(|e| format!("XFixes バージョン応答の取得に失敗しました: {e}"))?;

        let root_window = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| format!("X11 スクリーン {screen_num} が見つかりません"))?
            .root;

        let self_window = conn
            .generate_id()
            .map_err(|e| format!("隠しウィンドウ ID の生成に失敗しました: {e}"))?;
        let win_aux = CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE);
        conn.create_window(
            0,
            self_window,
            root_window,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &win_aux,
        )
        .map_err(|e| format!("クリップボード用隠しウィンドウの作成に失敗しました: {e}"))?;

        conn.xfixes_select_selection_input(
            self_window,
            atoms.CLIPBOARD,
            xfixes::SelectionEventMask::SET_SELECTION_OWNER,
        )
        .map_err(|e| format!("XFixes セレクション監視の登録に失敗しました: {e}"))?;

        conn.sync()
            .map_err(|e| format!("X11 初期化同期に失敗しました: {e}"))?;

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let wake_atom = atoms.CLIPON_WAKE;

        let mut worker_state = ClipboardWorker {
            conn,
            root_window,
            self_window,
            atoms,
            active_selection: None,
            incr_sends: HashMap::new(),
            last_server_time: x11rb::CURRENT_TIME,
            on_new_content: Box::new(on_new_content),
            cmd_rx,
            pending_clipboard_refresh: false,
            shutdown_flag: false,
        };

        let worker = std::thread::Builder::new()
            .name("clipon-x11-clipboard".to_string())
            .spawn(move || worker_state.run())
            .map_err(|e| format!("クリップボード監視スレッドの起動に失敗しました: {e}"))?;

        Ok(Self {
            cmd_tx: Mutex::new(cmd_tx),
            wake_conn: Mutex::new(wake_conn),
            self_window,
            wake_atom,
            shutdown_requested: AtomicBool::new(false),
            worker: Mutex::new(Some(worker)),
        })
    }

    /// 指定したペイロードで `CLIPBOARD` セレクションの所有権を取得・設定する。
    pub fn set_content(&self, payload: OutgoingPayload) -> Result<(), String> {
        let prepared = build_selection_payload(&payload)?;
        self.send_command(ClipboardCommand::SetContent(prepared))
    }

    /// 現在のマウスカーソル座標 `(root_x, root_y)` を取得する。
    pub fn query_pointer(&self) -> Option<(i32, i32)> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return None;
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .send_command(ClipboardCommand::QueryPointer(reply_tx))
            .is_err()
        {
            return None;
        }
        reply_rx.recv_timeout(POINTER_QUERY_TIMEOUT).ok().flatten()
    }

    /// バックグラウンドスレッドを安全に停止して待機する。
    pub fn shutdown(&self) {
        if !self.shutdown_requested.swap(true, Ordering::AcqRel) {
            let _ = self.send_command_unconditionally(ClipboardCommand::Shutdown);
        }
        if let Ok(mut guard) = self.worker.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
    }

    /// シャットダウン済みか確認した上でコマンドとウェイクアップイベントを送信する。
    fn send_command(&self, cmd: ClipboardCommand) -> Result<(), String> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return Err("クリップボードスレッドは既に停止しています".to_string());
        }
        self.send_command_unconditionally(cmd)
    }

    /// コマンドをチャネルへ積み、第2コネクションの `ClientMessage` で待機スレッドを即時起床させる。
    fn send_command_unconditionally(&self, cmd: ClipboardCommand) -> Result<(), String> {
        {
            let tx = self
                .cmd_tx
                .lock()
                .map_err(|_| "クリップボードコマンドチャネルのロックに失敗しました".to_string())?;
            tx.send(cmd)
                .map_err(|_| "クリップボードスレッドとの通信が切断されました".to_string())?;
        }
        self.wake_worker()?;
        Ok(())
    }

    /// `ClientMessage` を隠しウィンドウ宛に送信して `wait_for_event` を即時起床させる。
    fn wake_worker(&self) -> Result<(), String> {
        let wake_conn = self
            .wake_conn
            .lock()
            .map_err(|_| "X11 ウェイクアップ接続のロックに失敗しました".to_string())?;
        let event = ClientMessageEvent::new(32, self.self_window, self.wake_atom, [0u32; 5]);
        wake_conn
            .send_event(false, self.self_window, EventMask::NO_EVENT, event)
            .map_err(|e| format!("X11 ウェイクアップイベント送信に失敗しました: {e}"))?;
        wake_conn
            .flush()
            .map_err(|e| format!("X11 ウェイクアップフラッシュに失敗しました: {e}"))?;
        Ok(())
    }
}

impl Drop for ClipboardHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// X11 イベントループとセレクション所有権を単一スレッドで管理する内部ワーカー。
struct ClipboardWorker {
    conn: RustConnection,
    root_window: Window,
    self_window: Window,
    atoms: Atoms,
    active_selection: Option<ActiveSelection>,
    incr_sends: HashMap<(Window, Atom), IncrSendState>,
    last_server_time: Timestamp,
    on_new_content: Box<dyn Fn(ClipContent) + Send>,
    cmd_rx: mpsc::Receiver<ClipboardCommand>,
    pending_clipboard_refresh: bool,
    shutdown_flag: bool,
}

impl ClipboardWorker {
    /// メインイベントループを実行する。
    fn run(&mut self) {
        // 起動直後に既存のクリップボード内容を1回スナップショット取得する。
        self.refresh_clipboard_if_external();

        while !self.shutdown_flag {
            // 読み取り中に新しい所有者通知が割り込んでいれば、待機せず続けて最新内容を読む。
            if self.pending_clipboard_refresh {
                self.pending_clipboard_refresh = false;
                self.refresh_clipboard_if_external();
                continue;
            }

            self.cleanup_stale_incr_sends();

            // コマンドが既に積まれている場合はブロックせずに先行処理する。
            if self.drain_commands() == DispatchOutcome::AbortPendingRead && self.shutdown_flag {
                break;
            }
            if self.pending_clipboard_refresh {
                continue;
            }

            let event = match self.conn.wait_for_event() {
                Ok(ev) => ev,
                Err(_) => break,
            };

            let _ = self.dispatch_common_event(event);
            self.drain_queued_events();
        }

        let _ = self.conn.destroy_window(self.self_window);
        let _ = self.conn.flush();
    }

    /// ソケットに溜まっている保留イベントをブロックせずに一括処理する。
    fn drain_queued_events(&mut self) {
        while !self.shutdown_flag {
            match self.conn.poll_for_event() {
                Ok(Some(ev)) => {
                    let _ = self.dispatch_common_event(ev);
                }
                Ok(None) => break,
                Err(_) => {
                    self.shutdown_flag = true;
                    break;
                }
            }
        }
    }

    /// 共通の X11 イベントおよびウェイクアップコマンドを処理する。
    fn dispatch_common_event(&mut self, event: Event) -> DispatchOutcome {
        match event {
            Event::SelectionRequest(req) => {
                self.update_server_time(req.time);
                self.handle_selection_request(req);
                DispatchOutcome::Continue
            }
            Event::SelectionClear(ev) => {
                self.update_server_time(ev.time);
                if ev.selection == self.atoms.CLIPBOARD {
                    self.active_selection = None;
                    self.incr_sends.clear();
                }
                DispatchOutcome::Continue
            }
            Event::PropertyNotify(ev) => {
                self.update_server_time(ev.time);
                if ev.state == Property::DELETE && self.incr_sends.contains_key(&(ev.window, ev.atom))
                {
                    self.handle_incr_send_step(ev.window, ev.atom);
                }
                DispatchOutcome::Continue
            }
            Event::XfixesSelectionNotify(ev) => {
                self.update_server_time(ev.selection_timestamp);
                if ev.selection == self.atoms.CLIPBOARD {
                    if ev.owner == self.self_window {
                        // 自身の所有権取得による通知では読み取りを行わず、確定時刻のみ記録する。
                        if let Some(active) = &mut self.active_selection {
                            active.ownership_time = ev.selection_timestamp;
                        }
                    } else if self.is_external_owner_active() {
                        // 遅延到着した過去通知で最新の自己所有権を消さないよう、実所有者確認後にのみ更新する。
                        self.active_selection = None;
                        self.incr_sends.clear();
                        if ev.owner != x11rb::NONE {
                            self.pending_clipboard_refresh = true;
                        }
                    }
                }
                DispatchOutcome::Continue
            }
            Event::ClientMessage(_) => self.drain_commands(),
            _ => DispatchOutcome::Continue,
        }
    }

    /// 現在の X11 `CLIPBOARD` 所有者が自分自身でないかをサーバーへ照会する。
    fn is_external_owner_active(&self) -> bool {
        if self.active_selection.is_none() {
            return true;
        }
        self.conn
            .get_selection_owner(self.atoms.CLIPBOARD)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.owner != self.self_window)
    }

    /// コマンドチャネルをドレインし、`SetContent` / `QueryPointer` / `Shutdown` を実行する。
    fn drain_commands(&mut self) -> DispatchOutcome {
        let mut outcome = DispatchOutcome::Continue;
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                ClipboardCommand::SetContent(prepared) => {
                    self.apply_set_content(prepared);
                    self.pending_clipboard_refresh = false;
                    outcome = DispatchOutcome::AbortPendingRead;
                }
                ClipboardCommand::QueryPointer(reply_tx) => {
                    let pos = self.read_pointer_position();
                    let _ = reply_tx.send(pos);
                }
                ClipboardCommand::Shutdown => {
                    self.shutdown_flag = true;
                    self.pending_clipboard_refresh = false;
                    return DispatchOutcome::AbortPendingRead;
                }
            }
        }
        outcome
    }

    /// 新しいペイロードを現在のセレクションとして登録し、X11 `CLIPBOARD` の所有権を取得する。
    fn apply_set_content(&mut self, prepared: PreparedSelection) {
        let active = ActiveSelection::from_prepared(prepared, &self.atoms, self.last_server_time);
        self.active_selection = Some(active);
        self.incr_sends.clear();

        if self
            .conn
            .set_selection_owner(self.self_window, self.atoms.CLIPBOARD, x11rb::CURRENT_TIME)
            .is_ok()
        {
            let _ = self.conn.flush();
        }
    }

    /// 現在のマウスカーソル座標を取得する。
    fn read_pointer_position(&self) -> Option<(i32, i32)> {
        let reply = self
            .conn
            .query_pointer(self.root_window)
            .ok()?
            .reply()
            .ok()?;
        Some((i32::from(reply.root_x), i32::from(reply.root_y)))
    }

    /// `SelectionRequest` に対して `TARGETS` / `TIMESTAMP` / 各 MIME データを返却する。
    fn handle_selection_request(&mut self, req: SelectionRequestEvent) {
        let plan = plan_selection_reply(
            self.active_selection.as_ref(),
            &self.atoms,
            req.selection,
            req.target,
        );
        let dest_prop = effective_target_property(req.property, req.target);

        let notify_prop = match plan {
            SelectionReplyPlan::Targets(atoms_list) => {
                if self
                    .conn
                    .change_property32(
                        PropMode::REPLACE,
                        req.requestor,
                        dest_prop,
                        AtomEnum::ATOM,
                        atoms_list,
                    )
                    .is_ok()
                {
                    dest_prop
                } else {
                    x11rb::NONE
                }
            }
            SelectionReplyPlan::Timestamp(ts) => {
                if self
                    .conn
                    .change_property32(
                        PropMode::REPLACE,
                        req.requestor,
                        dest_prop,
                        AtomEnum::INTEGER,
                        &[ts],
                    )
                    .is_ok()
                {
                    dest_prop
                } else {
                    x11rb::NONE
                }
            }
            SelectionReplyPlan::Data { target, bytes } => {
                let bytes = bytes.to_vec();
                self.write_selection_data_or_start_incr(req.requestor, dest_prop, target, bytes)
            }
            SelectionReplyPlan::Reject => x11rb::NONE,
        };

        let notify = SelectionNotifyEvent {
            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: req.time,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property: notify_prop,
        };
        let _ = self
            .conn
            .send_event(false, req.requestor, EventMask::NO_EVENT, notify);
        let _ = self.conn.flush();
    }

    /// データを一括で書き込むか、X11 最大リクエスト長を超える場合は `INCR` 転送を開始する。
    fn write_selection_data_or_start_incr(
        &mut self,
        requestor: Window,
        property: Atom,
        target: Atom,
        data: Vec<u8>,
    ) -> Atom {
        let max_chunk = self
            .conn
            .maximum_request_bytes()
            .saturating_sub(X11_REQUEST_HEADER_MARGIN)
            .max(MIN_INCR_CHUNK_BYTES);

        if data.len() <= max_chunk {
            if self
                .conn
                .change_property8(PropMode::REPLACE, requestor, property, target, &data)
                .is_ok()
            {
                return property;
            }
            return x11rb::NONE;
        }

        // リクエスタ側の PropertyNotify(DELETE) を受信できるようマスクを拡張する。
        let current_mask = self
            .conn
            .get_window_attributes(requestor)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.your_event_mask)
            .unwrap_or_default();
        let new_mask = current_mask | EventMask::PROPERTY_CHANGE;
        if self
            .conn
            .change_window_attributes(
                requestor,
                &ChangeWindowAttributesAux::new().event_mask(new_mask),
            )
            .is_err()
        {
            return x11rb::NONE;
        }

        let total_size_hint = u32::try_from(data.len()).unwrap_or(u32::MAX);
        if self
            .conn
            .change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                self.atoms.INCR,
                &[total_size_hint],
            )
            .is_err()
        {
            return x11rb::NONE;
        }

        self.incr_sends.insert(
            (requestor, property),
            IncrSendState {
                target,
                data,
                offset: 0,
                chunk_size: max_chunk,
                last_activity: Instant::now(),
            },
        );
        property
    }

    /// `INCR` 送信においてリクエスタがプロパティを削除した際の次チャンク書き込みを行う。
    fn handle_incr_send_step(&mut self, window: Window, property: Atom) {
        let Some(state) = self.incr_sends.get_mut(&(window, property)) else {
            return;
        };
        state.last_activity = Instant::now();

        if state.offset < state.data.len() {
            let end = (state.offset + state.chunk_size).min(state.data.len());
            let chunk = &state.data[state.offset..end];
            if self
                .conn
                .change_property8(PropMode::REPLACE, window, property, state.target, chunk)
                .is_err()
            {
                self.incr_sends.remove(&(window, property));
                return;
            }
            state.offset = end;
        } else {
            let _ = self.conn.change_property8(
                PropMode::REPLACE,
                window,
                property,
                state.target,
                &[],
            );
            self.incr_sends.remove(&(window, property));
        }
        let _ = self.conn.flush();
    }

    /// 応答途絶した古い `INCR` 送信状態を掃除してメモリを解放する。
    fn cleanup_stale_incr_sends(&mut self) {
        if self.incr_sends.is_empty() {
            return;
        }
        let now = Instant::now();
        self.incr_sends
            .retain(|_, state| now.duration_since(state.last_activity) < INCR_SEND_STALE_TIMEOUT);
    }

    /// 外部プロセスが `CLIPBOARD` 所有者である場合のみ内容を読み取り、コールバックへ通知する。
    fn refresh_clipboard_if_external(&mut self) {
        let Ok(owner_cookie) = self.conn.get_selection_owner(self.atoms.CLIPBOARD) else {
            return;
        };
        let Ok(owner_reply) = owner_cookie.reply() else {
            return;
        };
        if owner_reply.owner == self.self_window || owner_reply.owner == x11rb::NONE {
            return;
        }

        if let Some(content) = self.read_external_clipboard() {
            (self.on_new_content)(content);
        }
    }

    /// `TARGETS` 問い合わせと優先順位に基づき外部クリップボードを読み取る。
    fn read_external_clipboard(&mut self) -> Option<ClipContent> {
        let available_targets = match self.read_available_targets() {
            TargetsFetch::Targets(targets) => targets,
            TargetsFetch::Unavailable => Vec::new(),
            TargetsFetch::Aborted => return None,
        };

        // 1. text/uri-list を最優先で確認し、ローカルファイルパス群があれば採用する。
        if available_targets.contains(&self.atoms.TEXT_URI_LIST) {
            match self.request_selection_bytes(self.atoms.TEXT_URI_LIST) {
                FetchOutcome::Data(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let paths = parse_uri_list(&text);
                    if !paths.is_empty() {
                        return Some(ClipContent::FilePaths(paths));
                    }
                }
                FetchOutcome::Unavailable => {}
                FetchOutcome::Aborted => return None,
            }
        }

        // 2. image/png があれば取得して解像度を解析する。
        if available_targets.contains(&self.atoms.IMAGE_PNG) {
            match self.request_selection_bytes(self.atoms.IMAGE_PNG) {
                FetchOutcome::Data(bytes) => {
                    if let Some((width, height)) = parse_png_dimensions(&bytes) {
                        return Some(ClipContent::Image {
                            png_bytes: bytes,
                            width,
                            height,
                        });
                    }
                }
                FetchOutcome::Unavailable => {}
                FetchOutcome::Aborted => return None,
            }
        }

        // 3. テキストターゲットを優先順で試す。TARGETS 取得に失敗した非標準所有者向けフォールバックも含む。
        let text_targets = if available_targets.is_empty() {
            vec![self.atoms.UTF8_STRING, self.atoms.STRING]
        } else {
            select_text_targets(&available_targets, &self.atoms)
        };

        for target in text_targets {
            match self.request_selection_bytes(target) {
                FetchOutcome::Data(bytes) => {
                    let text = decode_text_bytes(target, &bytes, &self.atoms);
                    if !text.is_empty() {
                        return Some(ClipContent::Text(text));
                    }
                }
                FetchOutcome::Unavailable => {}
                FetchOutcome::Aborted => return None,
            }
        }

        None
    }

    /// 現在の `CLIPBOARD` 所有者から `TARGETS` アトム一覧を取得する。
    fn read_available_targets(&mut self) -> TargetsFetch {
        match self.request_selection_bytes(self.atoms.TARGETS) {
            FetchOutcome::Data(raw) => {
                let mut atoms = Vec::with_capacity(raw.len() / 4);
                for chunk in raw.chunks_exact(4) {
                    atoms.push(u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
                TargetsFetch::Targets(atoms)
            }
            FetchOutcome::Unavailable => TargetsFetch::Unavailable,
            FetchOutcome::Aborted => TargetsFetch::Aborted,
        }
    }

    /// 指定ターゲットを `ConvertSelection` で要求し、通常または `INCR` で受信したバイト列を返す。
    fn request_selection_bytes(&mut self, target: Atom) -> FetchOutcome {
        let prop = self.atoms.CLIPON_SELECTION_PROP;
        let _ = self.conn.delete_property(self.self_window, prop);
        if self
            .conn
            .convert_selection(
                self.self_window,
                self.atoms.CLIPBOARD,
                target,
                prop,
                x11rb::CURRENT_TIME,
            )
            .is_err()
        {
            return FetchOutcome::Unavailable;
        }
        if self.conn.flush().is_err() {
            return FetchOutcome::Aborted;
        }

        let clipboard_atom = self.atoms.CLIPBOARD;
        let self_win = self.self_window;
        let notify_prop = match self.wait_with_event_loop(
            prop,
            Instant::now() + SELECTION_READ_TIMEOUT,
            None,
            |_, event| match event {
                Event::SelectionNotify(ev)
                    if ev.requestor == self_win
                        && ev.selection == clipboard_atom
                        && ev.target == target =>
                {
                    InterceptResult::Finish(ev.property)
                }
                other => InterceptResult::Forward(other),
            },
        ) {
            WaitLoopOutcome::Matched(notify_prop) => notify_prop,
            WaitLoopOutcome::TimedOut => return FetchOutcome::Unavailable,
            WaitLoopOutcome::Aborted => return FetchOutcome::Aborted,
        };

        if notify_prop == x11rb::NONE {
            return FetchOutcome::Unavailable;
        }
        self.read_property_or_incr(notify_prop)
    }

    /// 指定プロパティから通常データまたは `INCR` 分割転送を読み取る。
    fn read_property_or_incr(&mut self, prop: Atom) -> FetchOutcome {
        let max_words = (MAX_SELECTION_BYTES / 4) as u32;
        let Ok(cookie) = self.conn.get_property(
            true,
            self.self_window,
            prop,
            GetPropertyType::ANY,
            0,
            max_words.max(MAX_TARGETS_WORDS),
        ) else {
            return FetchOutcome::Unavailable;
        };
        let Ok(reply) = cookie.reply() else {
            return FetchOutcome::Unavailable;
        };

        if reply.type_ != self.atoms.INCR {
            if reply.bytes_after > 0 || reply.value.len() > MAX_SELECTION_BYTES {
                return FetchOutcome::Unavailable;
            }
            let mut bytes = Vec::new();
            append_property_bytes(&mut bytes, &reply);
            return FetchOutcome::Data(bytes);
        }

        // 初回の `get_property(delete = true)` で開始合図が飛んでいるため、各チャンクを待機する。
        let _ = self.conn.flush();
        let self_win = self.self_window;
        let mut accumulated = Vec::new();
        let mut transfer_failed = false;

        match self.wait_with_event_loop(
            prop,
            Instant::now() + INCR_TOTAL_TIMEOUT,
            Some(INCR_IDLE_TIMEOUT),
            |worker, event| match event {
                Event::PropertyNotify(ev)
                    if ev.window == self_win
                        && ev.atom == prop
                        && ev.state == Property::NEW_VALUE =>
                {
                    worker.update_server_time(ev.time);
                    let chunk_reply = worker
                        .conn
                        .get_property(true, self_win, prop, GetPropertyType::ANY, 0, max_words)
                        .ok()
                        .and_then(|c| c.reply().ok());
                    let Some(reply) = chunk_reply else {
                        transfer_failed = true;
                        return InterceptResult::Finish(());
                    };

                    if reply.value.is_empty() {
                        return InterceptResult::Finish(());
                    }
                    if accumulated.len().saturating_add(reply.value.len()) > MAX_SELECTION_BYTES
                        || reply.bytes_after > 0
                    {
                        transfer_failed = true;
                        return InterceptResult::Finish(());
                    }

                    append_property_bytes(&mut accumulated, &reply);
                    let _ = worker.conn.flush();
                    InterceptResult::Handled { reset_idle: true }
                }
                other => InterceptResult::Forward(other),
            },
        ) {
            WaitLoopOutcome::Matched(()) if !transfer_failed => FetchOutcome::Data(accumulated),
            WaitLoopOutcome::Matched(()) | WaitLoopOutcome::TimedOut => {
                self.cleanup_selection_property(prop);
                FetchOutcome::Unavailable
            }
            WaitLoopOutcome::Aborted => FetchOutcome::Aborted,
        }
    }

    /// タイムアウト・コマンド割り込み・他イベントの並列処理を統一的に扱う待機ループ。
    fn wait_with_event_loop<T>(
        &mut self,
        prop_to_cleanup: Atom,
        total_deadline: Instant,
        idle_timeout: Option<Duration>,
        mut on_event: impl FnMut(&mut Self, Event) -> InterceptResult<T>,
    ) -> WaitLoopOutcome<T> {
        let mut idle_deadline = idle_timeout.map(|d| Instant::now() + d);

        loop {
            if self.shutdown_flag {
                self.cleanup_selection_property(prop_to_cleanup);
                return WaitLoopOutcome::Aborted;
            }
            let now = Instant::now();
            if now >= total_deadline || idle_deadline.is_some_and(|d| now >= d) {
                self.cleanup_selection_property(prop_to_cleanup);
                return WaitLoopOutcome::TimedOut;
            }

            match self.conn.poll_for_event() {
                Ok(Some(event)) => match on_event(self, event) {
                    InterceptResult::Finish(value) => return WaitLoopOutcome::Matched(value),
                    InterceptResult::Handled { reset_idle } => {
                        if reset_idle {
                            idle_deadline = idle_timeout.map(|d| Instant::now() + d);
                        }
                    }
                    InterceptResult::Forward(other) => {
                        if self.dispatch_common_event(other) == DispatchOutcome::AbortPendingRead {
                            self.cleanup_selection_property(prop_to_cleanup);
                            return WaitLoopOutcome::Aborted;
                        }
                    }
                },
                Ok(None) => {
                    if self.drain_commands() == DispatchOutcome::AbortPendingRead {
                        self.cleanup_selection_property(prop_to_cleanup);
                        return WaitLoopOutcome::Aborted;
                    }
                    std::thread::sleep(WAIT_POLL_INTERVAL);
                }
                Err(_) => {
                    self.shutdown_flag = true;
                    return WaitLoopOutcome::Aborted;
                }
            }
        }
    }

    /// 受信作業用プロパティを削除して X11 サーバーへフラッシュする。
    fn cleanup_selection_property(&self, prop: Atom) {
        let _ = self.conn.delete_property(self.self_window, prop);
        let _ = self.conn.flush();
    }

    /// 有効なサーバータイムスタンプを受信した際に記録を更新する。
    fn update_server_time(&mut self, ts: Timestamp) {
        if ts != x11rb::CURRENT_TIME {
            self.last_server_time = ts;
        }
    }
}

/// `GetPropertyReply` のデータをフォーマットに応じてバイト列へ追記する（32-bit 列はホスト順へ正規化）。
fn append_property_bytes(out: &mut Vec<u8>, reply: &GetPropertyReply) {
    if reply.format == 32 {
        if let Some(iter) = reply.value32() {
            out.reserve(reply.value.len());
            for word in iter {
                out.extend_from_slice(&word.to_ne_bytes());
            }
            return;
        }
    }
    out.extend_from_slice(&reply.value);
}

/// `TARGETS` アトム一覧取得の結果。
enum TargetsFetch {
    Targets(Vec<Atom>),
    Unavailable,
    Aborted,
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};
    use tempfile::TempDir;

    fn find_target<'a>(selection: &'a PreparedSelection, mime: &str) -> Option<&'a [u8]> {
        selection
            .targets
            .iter()
            .find(|(name, _)| *name == mime)
            .map(|(_, data)| data.as_slice())
    }

    fn make_valid_png(width: u32, height: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        let pixels = vec![0u8; (width as usize) * (height as usize) * 3];
        PngEncoder::new(&mut buf)
            .write_image(&pixels, width, height, ExtendedColorType::Rgb8)
            .expect("テスト用PNGのエンコードに失敗");
        buf
    }

    fn make_test_atoms() -> Atoms {
        Atoms {
            CLIPBOARD: 100,
            TARGETS: 101,
            TIMESTAMP: 102,
            INCR: 103,
            UTF8_STRING: 104,
            TEXT_PLAIN_UTF8: 105,
            STRING: 106,
            TEXT_URI_LIST: 107,
            GNOME_COPIED_FILES: 108,
            IMAGE_PNG: 109,
            CLIPON_SELECTION_PROP: 110,
            CLIPON_WAKE: 111,
        }
    }

    #[test]
    fn build_selection_payload_file_mode_path_outputs_utf8_paths() {
        let payload = OutgoingPayload::Files {
            paths: vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")],
            mode: FileMode::Path,
        };
        let prepared = build_selection_payload(&payload).expect("Pathモード変換に成功すべき");

        assert_eq!(
            find_target(&prepared, "UTF8_STRING"),
            Some(b"/tmp/a.txt\n/tmp/b.txt".as_slice())
        );
        assert_eq!(
            find_target(&prepared, "text/plain;charset=utf-8"),
            Some(b"/tmp/a.txt\n/tmp/b.txt".as_slice())
        );
        assert_eq!(find_target(&prepared, "text/uri-list"), None);
    }

    #[test]
    fn build_selection_payload_file_mode_file_outputs_uri_list_and_gnome_copied() {
        let payload = OutgoingPayload::Files {
            paths: vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")],
            mode: FileMode::File,
        };
        let prepared = build_selection_payload(&payload).expect("Fileモード変換に成功すべき");

        assert_eq!(
            find_target(&prepared, "text/uri-list"),
            Some(b"file:///tmp/a.txt\r\nfile:///tmp/b.txt\r\n".as_slice())
        );
        assert_eq!(
            find_target(&prepared, "x-special/gnome-copied-files"),
            Some(b"copy\nfile:///tmp/a.txt\nfile:///tmp/b.txt".as_slice())
        );
    }

    #[test]
    fn build_selection_payload_image_mode_image_and_path() {
        let png_bytes = make_valid_png(2, 3);
        let dir = TempDir::new().expect("TempDirの作成に失敗");
        let hash = 0x1234_5678_9abc_def0;

        let image_mode = OutgoingPayload::Image {
            png_bytes: png_bytes.clone(),
            hash,
            mode: ImageMode::Image,
            cache_dir: dir.path().to_path_buf(),
        };
        let prepared_img =
            build_selection_payload(&image_mode).expect("Imageモード変換に成功すべき");
        assert_eq!(
            find_target(&prepared_img, "image/png"),
            Some(png_bytes.as_slice())
        );

        let path_mode = OutgoingPayload::Image {
            png_bytes: png_bytes.clone(),
            hash,
            mode: ImageMode::Path,
            cache_dir: dir.path().to_path_buf(),
        };
        let prepared_path =
            build_selection_payload(&path_mode).expect("ImagePathモード変換に成功すべき");
        let utf8_bytes =
            find_target(&prepared_path, "UTF8_STRING").expect("UTF8_STRINGが含まれるべき");
        let path_str = std::str::from_utf8(utf8_bytes).expect("有効なUTF-8パスであるべき");
        let expected_path = dir.path().join(format!("clipon_img_{hash:016x}.png"));
        assert_eq!(PathBuf::from(path_str), expected_path);
        assert!(expected_path.is_absolute());
        assert_eq!(std::fs::read(&expected_path).expect("PNG読込"), png_bytes);
    }

    #[test]
    fn parse_png_dimensions_reads_valid_png_and_rejects_garbage() {
        let valid_png = make_valid_png(2, 3);
        assert_eq!(parse_png_dimensions(&valid_png), Some((2, 3)));
        assert_eq!(parse_png_dimensions(b"not a png"), None);
        assert_eq!(parse_png_dimensions(&[]), None);
    }

    #[test]
    fn build_selection_payload_text_and_empty_inputs() {
        let prepared = build_selection_payload(&OutgoingPayload::Text("こんにちは".to_string()))
            .expect("テキスト変換に成功すべき");
        assert_eq!(
            find_target(&prepared, MIME_UTF8_STRING),
            Some("こんにちは".as_bytes())
        );
        assert_eq!(
            find_target(&prepared, MIME_TEXT_PLAIN_UTF8),
            Some("こんにちは".as_bytes())
        );
        assert_eq!(
            find_target(&prepared, MIME_STRING),
            Some("こんにちは".as_bytes())
        );

        let empty_files = OutgoingPayload::Files {
            paths: Vec::new(),
            mode: FileMode::Path,
        };
        assert!(build_selection_payload(&empty_files).is_err());

        let empty_img = OutgoingPayload::Image {
            png_bytes: Vec::new(),
            hash: 1,
            mode: ImageMode::Image,
            cache_dir: PathBuf::from("/tmp"),
        };
        assert!(build_selection_payload(&empty_img).is_err());
    }

    #[test]
    fn selection_reply_planner_and_text_decoding_work_deterministically() {
        let atoms = make_test_atoms();
        let prepared =
            build_selection_payload(&OutgoingPayload::Text("hello".to_string())).unwrap();
        let active = ActiveSelection::from_prepared(prepared, &atoms, 4242);

        assert_eq!(
            plan_selection_reply(Some(&active), &atoms, atoms.CLIPBOARD, atoms.TARGETS),
            SelectionReplyPlan::Targets(&active.supported_targets)
        );
        assert_eq!(
            plan_selection_reply(Some(&active), &atoms, atoms.CLIPBOARD, atoms.TIMESTAMP),
            SelectionReplyPlan::Timestamp(4242)
        );
        assert_eq!(
            plan_selection_reply(Some(&active), &atoms, atoms.CLIPBOARD, atoms.UTF8_STRING),
            SelectionReplyPlan::Data {
                target: atoms.UTF8_STRING,
                bytes: b"hello"
            }
        );
        assert_eq!(
            plan_selection_reply(Some(&active), &atoms, atoms.CLIPBOARD, atoms.IMAGE_PNG),
            SelectionReplyPlan::Reject
        );
        assert_eq!(
            plan_selection_reply(Some(&active), &atoms, 999, atoms.TARGETS),
            SelectionReplyPlan::Reject
        );
        assert_eq!(
            plan_selection_reply(None, &atoms, atoms.CLIPBOARD, atoms.TARGETS),
            SelectionReplyPlan::Reject
        );

        assert_eq!(effective_target_property(x11rb::NONE, atoms.UTF8_STRING), atoms.UTF8_STRING);
        assert_eq!(effective_target_property(555, atoms.UTF8_STRING), 555);

        assert_eq!(
            select_text_targets(&[atoms.STRING, atoms.UTF8_STRING], &atoms),
            vec![atoms.UTF8_STRING, atoms.STRING]
        );
        assert_eq!(
            decode_text_bytes(atoms.STRING, &[0xe9, b't', 0xe9], &atoms),
            "été"
        );
    }
}

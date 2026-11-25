//! 高速フレームレスポップアップ GUI およびシンプル設定画面モジュール (`eframe` / `egui`)。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use image::ImageFormat;

use crate::clipboard::{ClipboardHandle, OutgoingPayload};
use crate::config::{
    autostart_desktop_path, is_autostart_enabled_at, set_autostart_at, AppConfig, FileMode,
    ImageMode, UiLanguage,
};
use crate::history::{
    index_for_shortcut_char, shortcut_char_for_index, ClipContent, ClipItem, HistoryStore,
};
use crate::keyhook::{KeyHookBackend, KeyHookHandle};
use crate::paste::parse_paste_shortcut;
use crate::tray::build_tray_icon_argb32;
use crate::trigger::{canonical_key_name, TriggerKey};

/// 指定された表示言語 (`UiLanguage`) に応じて日本語または英語の静的文言を返す。
pub fn tr(lang: UiLanguage, ja: &'static str, en: &'static str) -> &'static str {
    match lang {
        UiLanguage::Ja => ja,
        UiLanguage::En => en,
    }
}

/// ポップアップウィンドウの固定幅（論理ピクセル）。
const POPUP_WIDTH: f32 = 420.0;
/// ポップアップウィンドウの最小高さ（論理ピクセル）。
const POPUP_MIN_HEIGHT: f32 = 108.0;
/// ポップアップウィンドウの最大高さ（論理ピクセル）。
const POPUP_MAX_HEIGHT: f32 = 460.0;
/// テキスト・ファイル行の推定高さ（論理ピクセル）。
const TEXT_ROW_HEIGHT: f32 = 28.0;
/// 画像行の推定高さ（論理ピクセル）。
const IMAGE_ROW_HEIGHT: f32 = 42.0;
/// ポップアップ上下余白およびフッター領域の高さ（論理ピクセル）。
const POPUP_CHROME_HEIGHT: f32 = 52.0;

/// 設定ウィンドウの幅・高さ（論理ピクセル）。
const SETTINGS_SIZE: (f32, f32) = (480.0, 480.0);

/// ポップアップ表示直後にフォーカス喪失による自動クローズを抑止する猶予時間（ミリ秒）。
const FOCUS_LOSS_GRACE_MS: u64 = 250;
/// 自動貼り付け時のフォーカス復帰待機時間（ミリ秒）。
const AUTO_PASTE_FOCUS_DELAY_MS: u64 = 45;

/// サムネイルテクスチャの最大幅・高さ（ピクセル）。
const THUMB_MAX_WIDTH: u32 = 64;
const THUMB_MAX_HEIGHT: u32 = 48;

/// バックグラウンドスレッドから GUI スレッドへ送る画面制御コマンド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    /// ポップアップウィンドウを表示する（`anchor` 未指定時は現在のカーソル座標を取得）。
    ShowPopup {
        /// 表示基準となるスクリーン座標 `(x, y)`。
        anchor: Option<(i32, i32)>,
    },
    /// 設定ウィンドウを表示する。
    ShowSettings,
    /// 履歴表示スナップショットを更新する。
    RefreshHistory,
    /// アプリケーションを終了する。
    Quit,
}

/// GUI ウィンドウの現在の表示モード。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMode {
    /// バックグラウンド非表示常駐状態。
    Hidden,
    /// 履歴一覧のフレームレスポップアップ表示状態。
    Popup,
    /// シンプル設定画面の表示状態。
    Settings,
}

/// GUI とバックグラウンドワーカー間で共有するアプリケーション状態。
pub struct AppState {
    /// 現在のアプリケーション設定。
    pub config: Mutex<AppConfig>,
    /// 設定ファイル (`config.toml`) の保存先パス。
    pub config_path: PathBuf,
    /// 画像一時保存キャッシュディレクトリ。
    pub cache_dir: PathBuf,
    /// クリップボード履歴ストア。
    pub history: Arc<Mutex<HistoryStore>>,
    /// X11 クリップボード操作ハンドル。
    pub clipboard: ClipboardHandle,
    /// グローバルキー監視ハンドル。
    pub keyhook: KeyHookHandle,
    /// 自動ペースト実行ワーカーへの送信チャネル（`None` 送信でワーカー終了）。
    pub paste_tx: Mutex<Option<Sender<Option<String>>>>,
}

impl AppState {
    /// 自動貼り付けワーカーへペーストキー送出を依頼する。
    pub fn request_auto_paste(&self, paste_key: String) {
        if let Ok(guard) = self.paste_tx.lock() {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(Some(paste_key));
            }
        }
    }
}

/// 別のバックグラウンドスレッドで常駐する自動貼り付けワーカーを起動する。
pub fn spawn_paste_worker() -> (Sender<Option<String>>, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let handle = std::thread::Builder::new()
        .name("clipon-auto-paste".to_string())
        .spawn(move || {
            let mut paster = crate::paste::AutoPaster::new();
            while let Ok(Some(paste_key)) = rx.recv() {
                // ポップアップを非表示にしてから前のウィンドウへフォーカスが戻るまで待機する。
                std::thread::sleep(Duration::from_millis(AUTO_PASTE_FOCUS_DELAY_MS));
                if let Err(err) = paster.send_paste(&paste_key) {
                    eprintln!("ClipOn 警告: 自動貼り付けの送出に失敗しました ({paste_key}): {err}");
                }
            }
        })
        .expect("自動貼り付けワーカースレッドの起動に失敗しました");
    (tx, handle)
}

/// ウィンドウマネージャ（GNOME 等）の上部パネルと重ならないようにする既定の上端安全マージン（論理ピクセル）。
const POPUP_MIN_TOP_MARGIN: f32 = 32.0;

/// マウスカーソル座標を基準としつつ、画面領域 `[0, screen - win]` 内にポップアップ位置をクランプする。
pub fn clamp_popup_position(
    anchor: (i32, i32),
    win_size: (f32, f32),
    screen_size: (f32, f32),
) -> (f32, f32) {
    let max_x = (screen_size.0 - win_size.0).max(0.0);
    let max_y = (screen_size.1 - win_size.1).max(0.0);
    let min_y = POPUP_MIN_TOP_MARGIN.min(max_y);
    let x = (anchor.0 as f32 + 8.0).clamp(0.0, max_x);
    let y = (anchor.1 as f32 + 8.0).clamp(min_y, max_y);
    (x, y)
}

/// 記憶済み座標・フォールバックカーソル座標・画面中央の優先順でポップアップ表示座標を決定する。
pub fn resolve_popup_position(
    saved_pos: Option<(i32, i32)>,
    fallback_anchor: Option<(i32, i32)>,
    win_size: (f32, f32),
    screen_size: (f32, f32),
) -> (f32, f32) {
    let max_x = (screen_size.0 - win_size.0).max(0.0);
    let max_y = (screen_size.1 - win_size.1).max(0.0);
    let min_y = POPUP_MIN_TOP_MARGIN.min(max_y);

    if let Some((sx, sy)) = saved_pos {
        return (
            (sx as f32).clamp(0.0, max_x),
            (sy as f32).clamp(min_y, max_y),
        );
    }

    if let Some((ax, ay)) = fallback_anchor {
        // Wayland 環境等で XQueryPointer が (0, 0) しか返せない場合は画面中央を優先する。
        if ax != 0 || ay != 0 {
            return clamp_popup_position((ax, ay), win_size, screen_size);
        }
    }

    (max_x * 0.5, (max_y * 0.5).max(min_y))
}

/// リスト選択インデックスを前後へ移動し、先頭・末尾でラップする。空リストでは `0` を返す。
pub fn move_selection(current: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len_i128 = len as i128;
    let cur_i128 = current.min(len - 1) as i128;
    (cur_i128 + delta as i128).rem_euclid(len_i128) as usize
}

/// システムまたはユーザーディレクトリから日本語フォントファイルを探索する。
pub fn find_japanese_font_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(home) = user_home_dir() {
        candidates.push(home.join(".local/share/fonts/Noto_Sans_JP/NotoSansJP-VariableFont_wght.ttf"));
        candidates.push(home.join(".local/share/fonts/NotoSansJP-Regular.ttf"));
        candidates.push(home.join(".local/share/fonts/NotoSansCJK-Regular.ttc"));
    }

    candidates.extend([
        PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
        PathBuf::from("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"),
        PathBuf::from("/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc"),
        PathBuf::from("/usr/share/fonts/truetype/noto/NotoSansJP-Regular.ttf"),
        PathBuf::from("/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/fonts-japanese-gothic.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/takao-gothic/TakaoPGothic.ttf"),
    ]);

    for path in candidates {
        if is_valid_font_file(&path) {
            return Some(path);
        }
    }

    // 候補パスに無い場合は `fc-match` でシステムの日本語フォントを照会する。
    let output = Command::new("fc-match")
        .args(["-f", "%{file}", ":lang=ja"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let matched = PathBuf::from(trimmed);
    is_valid_font_file(&matched).then_some(matched)
}

/// `egui::Key` をトリガー設定用の正規化キー名へ変換する。
pub fn egui_key_to_trigger_name(key: egui::Key) -> Option<String> {
    let name = match key {
        egui::Key::A => "a",
        egui::Key::B => "b",
        egui::Key::C => "c",
        egui::Key::D => "d",
        egui::Key::E => "e",
        egui::Key::F => "f",
        egui::Key::G => "g",
        egui::Key::H => "h",
        egui::Key::I => "i",
        egui::Key::J => "j",
        egui::Key::K => "k",
        egui::Key::L => "l",
        egui::Key::M => "m",
        egui::Key::N => "n",
        egui::Key::O => "o",
        egui::Key::P => "p",
        egui::Key::Q => "q",
        egui::Key::R => "r",
        egui::Key::S => "s",
        egui::Key::T => "t",
        egui::Key::U => "u",
        egui::Key::V => "v",
        egui::Key::W => "w",
        egui::Key::X => "x",
        egui::Key::Y => "y",
        egui::Key::Z => "z",
        egui::Key::Num0 => "0",
        egui::Key::Num1 => "1",
        egui::Key::Num2 => "2",
        egui::Key::Num3 => "3",
        egui::Key::Num4 => "4",
        egui::Key::Num5 => "5",
        egui::Key::Num6 => "6",
        egui::Key::Num7 => "7",
        egui::Key::Num8 => "8",
        egui::Key::Num9 => "9",
        egui::Key::F1 => "f1",
        egui::Key::F2 => "f2",
        egui::Key::F3 => "f3",
        egui::Key::F4 => "f4",
        egui::Key::F5 => "f5",
        egui::Key::F6 => "f6",
        egui::Key::F7 => "f7",
        egui::Key::F8 => "f8",
        egui::Key::F9 => "f9",
        egui::Key::F10 => "f10",
        egui::Key::F11 => "f11",
        egui::Key::F12 => "f12",
        egui::Key::Escape => "escape",
        egui::Key::Space => "space",
        egui::Key::Tab => "tab",
        egui::Key::Enter => "enter",
        egui::Key::Backspace => "backspace",
        egui::Key::Insert => "insert",
        egui::Key::Delete => "delete",
        egui::Key::Home => "home",
        egui::Key::End => "end",
        egui::Key::PageUp => "pageup",
        egui::Key::PageDown => "pagedown",
        egui::Key::ArrowUp => "up",
        egui::Key::ArrowDown => "down",
        egui::Key::ArrowLeft => "left",
        egui::Key::ArrowRight => "right",
        egui::Key::Minus => "minus",
        egui::Key::Equals => "equal",
        egui::Key::Backtick => "grave",
        egui::Key::OpenBracket => "leftbrace",
        egui::Key::CloseBracket => "rightbrace",
        egui::Key::Backslash => "backslash",
        egui::Key::Semicolon => "semicolon",
        egui::Key::Quote => "apostrophe",
        egui::Key::Comma => "comma",
        egui::Key::Period => "dot",
        egui::Key::Slash => "slash",
        _ => return None,
    };
    Some(name.to_string())
}

/// 常駐 `eframe` GUI イベントループを起動する。
pub fn run_gui(
    shared: Arc<AppState>,
    ui_rx: Receiver<UiCommand>,
    ctx_tx: Sender<egui::Context>,
) -> Result<(), String> {
    let icon = build_window_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipOn")
            .with_inner_size([POPUP_WIDTH, POPUP_MIN_HEIGHT])
            .with_visible(false)
            .with_taskbar(false)
            .with_resizable(true)
            .with_icon(Arc::new(icon)),
        persist_window: false,
        run_and_return: true,
        ..Default::default()
    };

    eframe::run_native(
        "ClipOn",
        options,
        Box::new(move |cc| {
            install_japanese_font(&cc.egui_ctx);
            let _ = ctx_tx.send(cc.egui_ctx.clone());
            Ok(Box::new(ClipOnApp::new(shared, ui_rx)))
        }),
    )
    .map_err(|err| format!("eframe GUI の起動に失敗しました: {err}"))
}

/// `egui::Key`（`1..=9`, `a..=z`）をショートカット文字へ変換する。
fn egui_key_to_shortcut_char(key: egui::Key) -> Option<char> {
    match key {
        egui::Key::Num1 => Some('1'),
        egui::Key::Num2 => Some('2'),
        egui::Key::Num3 => Some('3'),
        egui::Key::Num4 => Some('4'),
        egui::Key::Num5 => Some('5'),
        egui::Key::Num6 => Some('6'),
        egui::Key::Num7 => Some('7'),
        egui::Key::Num8 => Some('8'),
        egui::Key::Num9 => Some('9'),
        egui::Key::A => Some('a'),
        egui::Key::B => Some('b'),
        egui::Key::C => Some('c'),
        egui::Key::D => Some('d'),
        egui::Key::E => Some('e'),
        egui::Key::F => Some('f'),
        egui::Key::G => Some('g'),
        egui::Key::H => Some('h'),
        egui::Key::I => Some('i'),
        egui::Key::J => Some('j'),
        egui::Key::K => Some('k'),
        egui::Key::L => Some('l'),
        egui::Key::M => Some('m'),
        egui::Key::N => Some('n'),
        egui::Key::O => Some('o'),
        egui::Key::P => Some('p'),
        egui::Key::Q => Some('q'),
        egui::Key::R => Some('r'),
        egui::Key::S => Some('s'),
        egui::Key::T => Some('t'),
        egui::Key::U => Some('u'),
        egui::Key::V => Some('v'),
        egui::Key::W => Some('w'),
        egui::Key::X => Some('x'),
        egui::Key::Y => Some('y'),
        egui::Key::Z => Some('z'),
        _ => None,
    }
}

/// 履歴件数と内容に応じたポップアップの高さを算出する。
fn compute_popup_height(items: &[ClipItem]) -> f32 {
    if items.is_empty() {
        return POPUP_MIN_HEIGHT;
    }
    let rows_height: f32 = items
        .iter()
        .take(12)
        .map(|item| match item.content {
            ClipContent::Image { .. } => IMAGE_ROW_HEIGHT,
            ClipContent::Text(_) | ClipContent::FilePaths(_) => TEXT_ROW_HEIGHT,
        })
        .sum();
    (rows_height + POPUP_CHROME_HEIGHT).clamp(POPUP_MIN_HEIGHT, POPUP_MAX_HEIGHT)
}

/// 現在のユーザーホームディレクトリを取得する。
fn user_home_dir() -> Option<PathBuf> {
    if let Some(dirs) = directories::BaseDirs::new() {
        return Some(dirs.home_dir().to_path_buf());
    }
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `epaint` (`ab_glyph`) が安全に読める TTF / OTF / TTC ファイルかを先頭バイトで簡易検証する。
fn is_valid_font_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 4];
    use std::io::Read as _;
    if file.read_exact(&mut header).is_err() {
        return false;
    }
    matches!(
        &header,
        [0x00, 0x01, 0x00, 0x00] | b"OTTO" | b"ttcf" | b"true"
    )
}

/// 日本語フォントを発見できた場合に `egui::FontDefinitions` へ組み込む。
fn install_japanese_font(ctx: &egui::Context) {
    let Some(path) = find_japanese_font_path() else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    if bytes.len() < 4 {
        return;
    }

    let mut fonts = egui::FontDefinitions::default();
    let font_key = "clipon_japanese".to_string();
    fonts
        .font_data
        .insert(font_key.clone(), Arc::new(egui::FontData::from_owned(bytes)));

    if let Some(proportional) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        let insert_idx = usize::from(!proportional.is_empty());
        proportional.insert(insert_idx, font_key.clone());
    }
    if let Some(monospace) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        let insert_idx = usize::from(!monospace.is_empty());
        monospace.insert(insert_idx, font_key);
    }

    ctx.set_fonts(fonts);
}

/// トレイアイコン用の ARGB32 ビットマップをウィンドウ用 RGBA アイコンへ変換する。
fn build_window_icon() -> egui::IconData {
    let argb = build_tray_icon_argb32();
    let mut rgba = Vec::with_capacity(argb.data.len());
    for px in argb.data.chunks_exact(4) {
        rgba.push(px[1]);
        rgba.push(px[2]);
        rgba.push(px[3]);
        rgba.push(px[0]);
    }
    egui::IconData {
        rgba,
        width: u32::try_from(argb.width).unwrap_or(0),
        height: u32::try_from(argb.height).unwrap_or(0),
    }
}

/// 設定画面の編集ドラフト状態。
struct SettingsDraft {
    config: AppConfig,
    autostart: bool,
    capturing_key: bool,
    capture_rx: Option<Receiver<TriggerKey>>,
    prev_modifiers: egui::Modifiers,
    evdev_setup_rx: Option<Receiver<Result<(), String>>>,
    error_message: Option<String>,
}

impl SettingsDraft {
    /// 現在の共有設定と自動起動ファイル状態から新しい編集ドラフトを生成する。
    fn load(shared: &AppState) -> Self {
        let config = shared
            .config
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let autostart = is_autostart_enabled_at(&autostart_desktop_path());
        Self {
            config,
            autostart,
            capturing_key: false,
            capture_rx: None,
            prev_modifiers: egui::Modifiers::NONE,
            evdev_setup_rx: None,
            error_message: None,
        }
    }
}

/// `eframe::App` 実装本体。常駐ポップアップおよび設定画面を単一ビューポートで切り替える。
struct ClipOnApp {
    shared: Arc<AppState>,
    ui_rx: Receiver<UiCommand>,
    mode: UiMode,
    allow_exit: bool,
    cached_items: Vec<ClipItem>,
    selected_index: usize,
    scroll_to_selection: bool,
    opened_at: Option<Instant>,
    thumbnails: HashMap<u64, egui::TextureHandle>,
    failed_thumbnails: HashSet<u64>,
    settings: Option<SettingsDraft>,
    last_popup_pos: Option<(i32, i32)>,
    pending_popup_pos: Option<(f32, f32)>,
}

impl ClipOnApp {
    /// 新しい `ClipOnApp` を非表示状態で初期化する。
    fn new(shared: Arc<AppState>, ui_rx: Receiver<UiCommand>) -> Self {
        let initial_pos = shared
            .config
            .lock()
            .ok()
            .and_then(|cfg| cfg.popup.pos_x.zip(cfg.popup.pos_y));
        Self {
            shared,
            ui_rx,
            mode: UiMode::Hidden,
            allow_exit: false,
            cached_items: Vec::new(),
            selected_index: 0,
            scroll_to_selection: false,
            opened_at: None,
            thumbnails: HashMap::new(),
            failed_thumbnails: HashSet::new(),
            settings: None,
            last_popup_pos: initial_pos,
            pending_popup_pos: None,
        }
    }

    /// 履歴ストアから最新スナップショットを取得し、不要なサムネイルキャッシュを解放する。
    fn refresh_history_snapshot(&mut self) {
        if let Ok(guard) = self.shared.history.lock() {
            self.cached_items = guard.items().to_vec();
        }
        if self.cached_items.is_empty() {
            self.selected_index = 0;
        } else if self.selected_index >= self.cached_items.len() {
            self.selected_index = self.cached_items.len() - 1;
        }

        let live_hashes: HashSet<u64> = self.cached_items.iter().map(|item| item.hash).collect();
        self.thumbnails.retain(|hash, _| live_hashes.contains(hash));
        self.failed_thumbnails
            .retain(|hash| live_hashes.contains(hash));
    }

    /// 現在のポップアップ表示位置を読み取り、前回保存値と異なっていれば `config.toml` へ永続化する。
    fn persist_popup_position(&mut self, ctx: &egui::Context) {
        if self.pending_popup_pos.is_none() {
            // フレームレス (`Decorations(false)`) ウィンドウでは、winit X11 の `outer_rect` が
            // `_NET_FRAME_EXTENTS`（装飾時のタイトルバー高さ ~37px）を減算して返し、開閉ごとに
            // 画面上部へせり上がるドリフトを引き起こす。そのため実クライアント座標 (`inner_rect`) を用いる。
            if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
                self.last_popup_pos =
                    Some((rect.min.x.round() as i32, rect.min.y.round() as i32));
            }
        }
        let Some((x, y)) = self.last_popup_pos else {
            return;
        };

        let updated_cfg = if let Ok(mut cfg) = self.shared.config.lock() {
            if cfg.popup.pos_x == Some(x) && cfg.popup.pos_y == Some(y) {
                None
            } else {
                cfg.popup.pos_x = Some(x);
                cfg.popup.pos_y = Some(y);
                Some(cfg.clone())
            }
        } else {
            None
        };

        if let Some(cfg) = updated_cfg {
            if let Err(err) = cfg.save_to_path(&self.shared.config_path) {
                eprintln!("ClipOn 警告: ポップアップ位置の保存に失敗しました: {err}");
            }
        }
    }

    /// 設定画面でのキー録音状態を安全に停止する。
    fn stop_capture_if_active(&mut self) {
        if let Some(draft) = &mut self.settings {
            if draft.capturing_key {
                draft.capturing_key = false;
                draft.capture_rx = None;
                self.shared.keyhook.stop_capture();
            }
        }
    }

    /// ウィンドウを非表示化して常駐待機状態 (`UiMode::Hidden`) へ戻す。
    fn hide_window(&mut self, ctx: &egui::Context) {
        if self.mode == UiMode::Popup {
            self.persist_popup_position(ctx);
        }
        self.pending_popup_pos = None;
        self.stop_capture_if_active();
        self.mode = UiMode::Hidden;
        self.opened_at = None;
        self.settings = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// ポップアップウィンドウを記憶済み位置（未設定時はカーソル直下または画面中央）へ表示する。
    fn open_popup(&mut self, ctx: &egui::Context, anchor: Option<(i32, i32)>) {
        self.stop_capture_if_active();
        self.settings = None;
        self.refresh_history_snapshot();
        self.selected_index = 0;
        self.scroll_to_selection = false;
        self.mode = UiMode::Popup;
        self.opened_at = Some(Instant::now());

        let win_height = compute_popup_height(&self.cached_items);
        let win_size = (POPUP_WIDTH, win_height);

        let ppp = ctx.pixels_per_point().max(0.5);
        let screen_size = ctx
            .input(|i| i.viewport().monitor_size)
            .map(|s| (s.x, s.y))
            .filter(|&(w, h)| w > 1.0 && h > 1.0)
            .unwrap_or((1920.0, 1080.0));

        let saved_pos = self.last_popup_pos.or_else(|| {
            self.shared
                .config
                .lock()
                .ok()
                .and_then(|cfg| cfg.popup.pos_x.zip(cfg.popup.pos_y))
        });

        let raw_anchor = anchor.or_else(|| self.shared.clipboard.query_pointer());
        let logical_anchor = raw_anchor.map(|(ax, ay)| {
            (
                (ax as f32 / ppp).round() as i32,
                (ay as f32 / ppp).round() as i32,
            )
        });
        let clamped_pos = resolve_popup_position(saved_pos, logical_anchor, win_size, screen_size);
        self.last_popup_pos = Some((clamped_pos.0.round() as i32, clamped_pos.1.round() as i32));
        self.pending_popup_pos = Some(clamped_pos);

        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(260.0, 80.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(egui::vec2(800.0, 700.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::AlwaysOnTop,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            win_size.0, win_size.1,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            clamped_pos.0,
            clamped_pos.1,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        // フォーカス喪失グレースピリオド終了時に状態再点検できるよう再描画を予約する。
        ctx.request_repaint_after(Duration::from_millis(FOCUS_LOSS_GRACE_MS + 20));
    }

    /// シンプル設定ウィンドウを表示する。
    fn open_settings(&mut self, ctx: &egui::Context) {
        if self.mode == UiMode::Popup {
            self.persist_popup_position(ctx);
        }
        self.pending_popup_pos = None;
        self.stop_capture_if_active();
        self.mode = UiMode::Settings;
        self.opened_at = None;
        let draft = SettingsDraft::load(&self.shared);
        let lang = draft.config.ui.language;
        self.settings = Some(draft);

        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(420.0, 340.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(egui::vec2(1200.0, 1000.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::Normal,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            tr(lang, "ClipOn 設定", "ClipOn Settings").to_string(),
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            SETTINGS_SIZE.0,
            SETTINGS_SIZE.1,
        )));
        if let Some(center_cmd) = egui::ViewportCommand::center_on_screen(ctx) {
            ctx.send_viewport_cmd(center_cmd);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// バックグラウンドスレッドから届いた未処理コマンドをすべて処理する。
    fn drain_ui_commands(&mut self, ctx: &egui::Context) {
        while let Ok(cmd) = self.ui_rx.try_recv() {
            match cmd {
                UiCommand::ShowPopup { anchor } => self.open_popup(ctx, anchor),
                UiCommand::ShowSettings => self.open_settings(ctx),
                UiCommand::RefreshHistory => {
                    if self.mode == UiMode::Popup {
                        self.refresh_history_snapshot();
                    }
                }
                UiCommand::Quit => {
                    if self.mode == UiMode::Popup {
                        self.persist_popup_position(ctx);
                    }
                    self.stop_capture_if_active();
                    self.allow_exit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    break;
                }
            }
        }
    }

    /// 選択された履歴エントリをクリップボードへ転送し、即座にポップアップを閉じて自動貼り付けする。
    fn confirm_selection(&mut self, ctx: &egui::Context, index: usize) {
        let Some(chosen) = self.cached_items.get(index).cloned() else {
            self.hide_window(ctx);
            return;
        };

        // フォーカス復帰と視認レスポンス最優先のため即座に非表示化する。
        self.hide_window(ctx);

        if let Ok(mut history) = self.shared.history.lock() {
            if let Some(pos) = history.items().iter().position(|item| item.id == chosen.id) {
                history.promote_to_front(pos);
            }
        }

        let cfg = self
            .shared
            .config
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        let payload = match chosen.content {
            ClipContent::Text(text) => OutgoingPayload::Text(text),
            ClipContent::FilePaths(paths) => OutgoingPayload::Files {
                paths,
                mode: cfg.media.file_mode,
            },
            ClipContent::Image { png_bytes, .. } => OutgoingPayload::Image {
                png_bytes,
                hash: chosen.hash,
                mode: cfg.media.image_mode,
                cache_dir: self.shared.cache_dir.clone(),
            },
        };

        if let Err(err) = self.shared.clipboard.set_content(payload) {
            eprintln!("ClipOn 警告: クリップボードへの転送に失敗しました: {err}");
            return;
        }

        if cfg.paste.auto_paste {
            self.shared.request_auto_paste(cfg.paste.paste_key);
        }
    }

    /// ポップアップ表示時のキーボード入力およびフォーカス喪失を処理する。
    fn handle_popup_input(&mut self, ctx: &egui::Context) -> Option<usize> {
        let mut close_requested = false;
        let mut confirmed_index: Option<usize> = None;
        let len = self.cached_items.len();

        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    repeat,
                    modifiers,
                    ..
                } = event
                {
                    match key {
                        egui::Key::Escape => {
                            close_requested = true;
                            return;
                        }
                        egui::Key::ArrowDown => {
                            self.selected_index = move_selection(self.selected_index, 1, len);
                            self.scroll_to_selection = true;
                        }
                        egui::Key::ArrowUp => {
                            self.selected_index = move_selection(self.selected_index, -1, len);
                            self.scroll_to_selection = true;
                        }
                        egui::Key::Enter if !*repeat => {
                            if len > 0 {
                                confirmed_index = Some(self.selected_index.min(len - 1));
                                return;
                            }
                        }
                        _ if !*repeat && modifiers.is_none() => {
                            if let Some(ch) = egui_key_to_shortcut_char(*key) {
                                if let Some(idx) = index_for_shortcut_char(ch) {
                                    if idx < len {
                                        confirmed_index = Some(idx);
                                        return;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        });

        if close_requested {
            self.hide_window(ctx);
            return None;
        }
        if confirmed_index.is_some() {
            return confirmed_index;
        }

        // グレースピリオド経過後にフォーカスが外れていれば自動的に閉じる。
        if let Some(opened_at) = self.opened_at {
            let elapsed = opened_at.elapsed();
            if elapsed >= Duration::from_millis(FOCUS_LOSS_GRACE_MS) {
                if ctx.input(|i| i.viewport().focused == Some(false)) {
                    self.hide_window(ctx);
                }
            } else {
                let remaining = Duration::from_millis(FOCUS_LOSS_GRACE_MS) - elapsed;
                ctx.request_repaint_after(remaining + Duration::from_millis(10));
            }
        }

        None
    }

    /// 画像履歴アイテムの縮小サムネイルテクスチャを取得または生成する。
    fn get_or_load_thumbnail(
        &mut self,
        ctx: &egui::Context,
        hash: u64,
        png_bytes: &[u8],
    ) -> Option<egui::TextureHandle> {
        if let Some(tex) = self.thumbnails.get(&hash) {
            return Some(tex.clone());
        }
        if self.failed_thumbnails.contains(&hash) {
            return None;
        }

        let Ok(img) = image::load_from_memory_with_format(png_bytes, ImageFormat::Png) else {
            self.failed_thumbnails.insert(hash);
            return None;
        };
        let thumb = img.thumbnail(THUMB_MAX_WIDTH, THUMB_MAX_HEIGHT).to_rgba8();
        let size = [thumb.width() as usize, thumb.height() as usize];
        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, thumb.as_raw());
        let handle = ctx.load_texture(
            format!("clipon_thumb_{hash:016x}"),
            color_image,
            egui::TextureOptions::LINEAR,
        );
        self.thumbnails.insert(hash, handle.clone());
        Some(handle)
    }

    /// ポップアップ UI を描画する。
    fn render_popup(&mut self, ctx: &egui::Context) {
        if let Some(pos) = self.pending_popup_pos.take() {
            // XWayland / WM のマップ直後の位置リセットに備えて描画フレームでも配置を再適用する。
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                pos.0, pos.1,
            )));
        } else if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            // outer_rect ではなく inner_rect を参照することで、winit X11 の _NET_FRAME_EXTENTS
            // 減算による上方ドリフトを完全に防止しつつ、Super+ドラッグ等による移動も正確に追従する。
            self.last_popup_pos = Some((rect.min.x.round() as i32, rect.min.y.round() as i32));
        }

        if let Some(idx) = self.handle_popup_input(ctx) {
            self.confirm_selection(ctx, idx);
            return;
        }
        if self.mode != UiMode::Popup {
            return;
        }

        let lang = self
            .shared
            .config
            .lock()
            .map(|cfg| cfg.ui.language)
            .unwrap_or(UiLanguage::Ja);
        let mut clicked_index: Option<usize> = None;
        let mut open_settings_clicked = false;
        let mut close_clicked = false;
        let mut start_window_drag = false;
        let items = self.cached_items.clone();
        let selected_index = self.selected_index;
        let scroll_to_selection = self.scroll_to_selection;
        self.scroll_to_selection = false;

        let panel_frame = egui::Frame::central_panel(&ctx.style())
            .stroke(egui::Stroke::new(
                1.0,
                ctx.style().visuals.widgets.noninteractive.bg_stroke.color,
            ))
            .inner_margin(egui::Margin::same(8));

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                if items.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(16.0);
                        ui.label(
                            egui::RichText::new(tr(
                                lang,
                                "クリップボード履歴はまだありません",
                                "No clipboard history yet",
                            ))
                            .color(ui.visuals().weak_text_color()),
                        );
                        ui.add_space(16.0);
                    });
                } else {
                    let footer_reserve = 30.0;
                    let scroll_max_height = (ui.available_height() - footer_reserve).max(40.0);
                    egui::ScrollArea::vertical()
                        .max_height(scroll_max_height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (idx, item) in items.iter().enumerate() {
                                let is_selected = idx == selected_index;
                                let badge = shortcut_char_for_index(idx)
                                    .map(|c| format!("[{c}]"))
                                    .unwrap_or_else(|| " · ".to_string());

                                let mut prepared = egui::Frame::NONE
                                    .corner_radius(4.0)
                                    .inner_margin(egui::Margin::symmetric(6, 4))
                                    .begin(ui);
                                {
                                    let row_ui = &mut prepared.content_ui;
                                    row_ui.set_min_width(row_ui.available_width());
                                    row_ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(&badge)
                                                .monospace()
                                                .strong()
                                                .color(if is_selected {
                                                    ui.visuals().strong_text_color()
                                                } else {
                                                    ui.visuals().weak_text_color()
                                                }),
                                        );

                                        if let ClipContent::Image { png_bytes, .. } = &item.content
                                        {
                                            if let Some(tex) =
                                                self.get_or_load_thumbnail(ctx, item.hash, png_bytes)
                                            {
                                                ui.add(
                                                    egui::Image::from_texture(&tex)
                                                        .max_size(egui::vec2(36.0, 28.0))
                                                        .corner_radius(2.0),
                                                );
                                            }
                                        }

                                        let label_text = item.display_label_for_lang(52, lang);
                                        ui.add(
                                            egui::Label::new(label_text)
                                                .truncate()
                                                .selectable(false),
                                        );
                                    });
                                }

                                let row_resp =
                                    prepared.allocate_space(ui).interact(egui::Sense::click());
                                if is_selected {
                                    prepared.frame.fill = ui.visuals().selection.bg_fill;
                                } else if row_resp.hovered() {
                                    prepared.frame.fill = ui.visuals().widgets.hovered.bg_fill;
                                }
                                prepared.paint(ui);

                                if is_selected && scroll_to_selection {
                                    row_resp.scroll_to_me(Some(egui::Align::Center));
                                }
                                if row_resp.clicked() {
                                    clicked_index = Some(idx);
                                }
                            }
                        });
                }

                ui.separator();
                ui.horizontal(|ui| {
                    let hint_resp = ui.add(
                        egui::Label::new(
                            egui::RichText::new(tr(
                                lang,
                                "↑↓+Enter / 1-9,a-z で貼付",
                                "↑↓+Enter / 1-9,a-z to paste",
                            ))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                        )
                        .sense(egui::Sense::drag()),
                    );
                    if hint_resp.drag_started_by(egui::PointerButton::Primary) {
                        start_window_drag = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button(tr(lang, "✕ 閉じる", "✕ Close"))
                            .clicked()
                        {
                            close_clicked = true;
                        }
                        if ui
                            .small_button(tr(lang, "⚙ 設定", "⚙ Settings"))
                            .clicked()
                        {
                            open_settings_clicked = true;
                        }
                    });
                });
            });

        if start_window_drag {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        if close_clicked {
            self.hide_window(ctx);
        } else if open_settings_clicked {
            self.open_settings(ctx);
        } else if let Some(idx) = clicked_index {
            self.confirm_selection(ctx, idx);
        }
    }

    /// 設定画面でキー録音中のイベントを処理する。
    fn poll_settings_key_capture(&mut self, ctx: &egui::Context) {
        let Some(draft) = &mut self.settings else {
            return;
        };
        if !draft.capturing_key {
            return;
        }

        // 1. グローバル keyhook（Shift/Ctrl/Alt/Super 単体を含む）からの受信を優先する。
        if let Some(rx) = &draft.capture_rx {
            if let Ok(trigger_key) = rx.try_recv() {
                draft.config.trigger.key = canonical_key_name(&trigger_key);
                draft.capturing_key = false;
                draft.capture_rx = None;
                self.shared.keyhook.stop_capture();
                return;
            }
        }

        // 2. フォールバックとして egui 側のキーイベントおよび修飾キー押下エッジも検知する。
        let (captured_name, current_mods) = ctx.input(|i| {
            let mut found = None;
            for event in &i.events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    ..
                } = event
                {
                    if let Some(name) = egui_key_to_trigger_name(*key) {
                        found = Some(name);
                        break;
                    }
                }
            }
            if found.is_none() {
                if i.modifiers.shift && !draft.prev_modifiers.shift {
                    found = Some("Shift".to_string());
                } else if i.modifiers.ctrl && !draft.prev_modifiers.ctrl {
                    found = Some("Ctrl".to_string());
                } else if i.modifiers.alt && !draft.prev_modifiers.alt {
                    found = Some("Alt".to_string());
                }
            }
            (found, i.modifiers)
        });

        draft.prev_modifiers = current_mods;
        if let Some(name) = captured_name {
            draft.config.trigger.key = name;
            draft.capturing_key = false;
            draft.capture_rx = None;
            self.shared.keyhook.stop_capture();
        } else {
            // バックグラウンドの keyhook スレッドからの到着を即座に反映するため短周期再描画する。
            ctx.request_repaint_after(Duration::from_millis(20));
        }
    }

    /// 設定画面の入力内容を検証して保存・即時反映する。
    fn save_settings_and_close(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.settings.take() else {
            self.hide_window(ctx);
            return;
        };

        if draft.capturing_key {
            draft.capturing_key = false;
            draft.capture_rx = None;
            self.shared.keyhook.stop_capture();
        }

        if let Ok(cfg_guard) = self.shared.config.lock() {
            draft.config.popup = cfg_guard.popup.clone();
        }
        draft.config.sanitize();

        if let Err(err) = parse_paste_shortcut(&draft.config.paste.paste_key) {
            let lang = draft.config.ui.language;
            draft.error_message = Some(match lang {
                UiLanguage::Ja => format!("貼り付けキーの指定が不正です: {err}"),
                UiLanguage::En => format!("Invalid paste key shortcut: {err}"),
            });
            self.settings = Some(draft);
            return;
        }

        if let Err(err) = draft.config.save_to_path(&self.shared.config_path) {
            draft.error_message = Some(err);
            self.settings = Some(draft);
            return;
        }

        let exec_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clipon"));
        if let Err(err) = set_autostart_at(&autostart_desktop_path(), &exec_path, draft.autostart) {
            draft.error_message = Some(err);
            self.settings = Some(draft);
            return;
        }

        if let Ok(mut history) = self.shared.history.lock() {
            history.set_max_items(draft.config.history.max_items);
        }
        self.shared
            .keyhook
            .update_config(draft.config.trigger.clone());

        if let Ok(mut cfg_guard) = self.shared.config.lock() {
            *cfg_guard = draft.config;
        }

        self.refresh_history_snapshot();
        self.hide_window(ctx);
    }

    /// シンプル設定画面 UI を描画する（REQ-1 準拠：色・テーマ等の装飾項目は一切配置しない）。
    fn render_settings(&mut self, ctx: &egui::Context) {
        self.poll_settings_key_capture(ctx);

        let mut trigger_save = false;
        let mut trigger_cancel = false;
        let mut toggle_capture = false;
        let backend_status = self.shared.keyhook.backend_status();

        let current_lang = self
            .settings
            .as_ref()
            .map(|draft| draft.config.ui.language)
            .unwrap_or(UiLanguage::Ja);

        // キー録音中でない場合、Escape キーで設定画面を即座に閉じる。
        let is_capturing = self
            .settings
            .as_ref()
            .is_some_and(|draft| draft.capturing_key);
        if !is_capturing && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            trigger_cancel = true;
        }

        egui::TopBottomPanel::bottom("clipon_settings_footer")
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(tr(current_lang, "キャンセル", "Cancel"))
                            .clicked()
                        {
                            trigger_cancel = true;
                        }
                        if ui
                            .button(tr(current_lang, "保存して閉じる", "Save & Close"))
                            .clicked()
                        {
                            trigger_save = true;
                        }
                    });
                });
                ui.add_space(2.0);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(draft) = &mut self.settings else {
                return;
            };

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let prev_lang = draft.config.ui.language;
                    ui.horizontal(|ui| {
                        ui.heading(tr(prev_lang, "ClipOn 設定", "ClipOn Settings"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.radio_value(
                                &mut draft.config.ui.language,
                                UiLanguage::En,
                                "English",
                            );
                            ui.radio_value(
                                &mut draft.config.ui.language,
                                UiLanguage::Ja,
                                "日本語",
                            );
                            ui.label("🌐");
                        });
                    });
                    let lang = draft.config.ui.language;
                    if lang != prev_lang {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
                            tr(lang, "ClipOn 設定", "ClipOn Settings").to_string(),
                        ));
                    }
                    ui.add_space(6.0);

                    // 1. 呼び出しキーバインド設定
                    ui.group(|ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            egui::RichText::new(tr(
                                lang,
                                "呼び出しキーバインド",
                                "Trigger Keybinding",
                            ))
                            .strong(),
                        );
                        ui.horizontal(|ui| {
                            ui.label(tr(lang, "呼び出しキー:", "Trigger Key:"));
                            if draft.capturing_key {
                                let mut waiting_text =
                                    tr(lang, "キーを押してください...", "Press any key...")
                                        .to_string();
                                ui.add_enabled(
                                    false,
                                    egui::TextEdit::singleline(&mut waiting_text)
                                        .desired_width(140.0),
                                );
                            } else {
                                ui.add(
                                    egui::TextEdit::singleline(&mut draft.config.trigger.key)
                                        .desired_width(140.0),
                                );
                            }
                            let btn_label = if draft.capturing_key {
                                tr(lang, "録音キャンセル", "Cancel Capture")
                            } else {
                                tr(lang, "キーを押して設定", "Press to Record")
                            };
                            if ui.button(btn_label).clicked() {
                                toggle_capture = true;
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label(tr(lang, "タップ回数:", "Tap Count:"));
                            ui.radio_value(
                                &mut draft.config.trigger.tap_count,
                                1,
                                tr(lang, "1回", "1x"),
                            );
                            ui.radio_value(
                                &mut draft.config.trigger.tap_count,
                                2,
                                tr(lang, "2回", "2x"),
                            );
                            ui.radio_value(
                                &mut draft.config.trigger.tap_count,
                                3,
                                tr(lang, "3回", "3x"),
                            );
                        });

                        ui.horizontal(|ui| {
                            ui.label(tr(lang, "連続タップ間隔:", "Tap Interval:"));
                            ui.add(
                                egui::DragValue::new(&mut draft.config.trigger.interval_ms)
                                    .range(80..=2000)
                                    .speed(10)
                                    .suffix(" ms"),
                            );
                        });
                    });

                    ui.add_space(4.0);

                    // 2. 履歴件数 ＆ 3. 貼り付け設定
                    ui.group(|ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(tr(
                                    lang,
                                    "履歴保持件数:",
                                    "Max History Items:",
                                ))
                                .strong(),
                            );
                            ui.add(
                                egui::DragValue::new(&mut draft.config.history.max_items)
                                    .range(5..=200)
                                    .speed(1)
                                    .suffix(tr(lang, " 件", " items")),
                            );
                        });

                        ui.add_space(2.0);
                        ui.checkbox(
                            &mut draft.config.paste.auto_paste,
                            tr(
                                lang,
                                "選択後に直前ウィンドウへ自動貼り付け",
                                "Auto-paste to previous window on select",
                            ),
                        );
                        ui.horizontal(|ui| {
                            ui.label(tr(lang, "貼り付けキー:", "Paste Key:"));
                            ui.add(
                                egui::TextEdit::singleline(&mut draft.config.paste.paste_key)
                                    .desired_width(120.0),
                            );
                            for preset in ["Ctrl+v", "Ctrl+Shift+v", "Shift+Insert"] {
                                if ui.small_button(preset).clicked() {
                                    draft.config.paste.paste_key = preset.to_string();
                                }
                            }
                        });
                    });

                    ui.add_space(4.0);

                    // 4. ファイル ＆ 画像モード
                    ui.group(|ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            egui::RichText::new(tr(
                                lang,
                                "ファイル・画像転送モード",
                                "File & Image Transfer Mode",
                            ))
                            .strong(),
                        );
                        ui.horizontal(|ui| {
                            ui.label(tr(lang, "ファイル選択時:", "On File Select:"));
                            ui.radio_value(
                                &mut draft.config.media.file_mode,
                                FileMode::Path,
                                tr(lang, "絶対パス文字列 (path)", "File Path (path)"),
                            );
                            ui.radio_value(
                                &mut draft.config.media.file_mode,
                                FileMode::File,
                                tr(lang, "ファイル実体 (file)", "File URI (file)"),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label(tr(lang, "画像選択時:", "On Image Select:"));
                            ui.radio_value(
                                &mut draft.config.media.image_mode,
                                ImageMode::Image,
                                tr(lang, "画像データ貼付 (image)", "Image Data (image)"),
                            );
                            ui.radio_value(
                                &mut draft.config.media.image_mode,
                                ImageMode::Path,
                                tr(lang, "一時ファイルパス (path)", "Cached File Path (path)"),
                            );
                        });
                    });

                    ui.add_space(4.0);

                    // 5. OS 自動起動（REQ-7）＆ 6. 入力監視ステータス
                    ui.checkbox(
                        &mut draft.autostart,
                        tr(
                            lang,
                            "Ubuntuログイン時に自動起動して常駐する",
                            "Launch automatically at Ubuntu login (Autostart)",
                        ),
                    );

                    ui.add_space(2.0);
                    let mut start_evdev_setup = false;
                    let mut setup_finished: Option<Result<(), String>> = None;
                    if let Some(rx) = &draft.evdev_setup_rx {
                        if let Ok(res) = rx.try_recv() {
                            setup_finished = Some(res);
                        } else {
                            ctx.request_repaint_after(Duration::from_millis(100));
                        }
                    }
                    if let Some(res) = setup_finished {
                        draft.evdev_setup_rx = None;
                        match res {
                            Ok(()) => {
                                draft.error_message = None;
                                self.shared.keyhook.reload_devices();
                                ctx.request_repaint_after(Duration::from_millis(120));
                            }
                            Err(err) => {
                                draft.error_message = Some(err);
                            }
                        }
                    }

                    match backend_status {
                        KeyHookBackend::Evdev { device_count } => {
                            let status_msg = match lang {
                                UiLanguage::Ja => {
                                    format!("✅ 全画面グローバル監視有効 (evdev: {device_count}台)")
                                }
                                UiLanguage::En => format!(
                                    "✅ Global key monitoring active (evdev: {device_count} device(s))"
                                ),
                            };
                            ui.label(egui::RichText::new(status_msg).small());
                        }
                        KeyHookBackend::X11Fallback => {
                            ui.label(
                                egui::RichText::new(tr(
                                    lang,
                                    "⚠️ X11フォールバック中（Wayland上の他アプリでは Shift×2 等が反応しません）",
                                    "⚠️ X11 fallback active (double-tap won't trigger over Wayland apps)",
                                ))
                                .small(),
                            );
                            let in_progress = draft.evdev_setup_rx.is_some();
                            if ui
                                .add_enabled(
                                    !in_progress,
                                    egui::Button::new(if in_progress {
                                        tr(
                                            lang,
                                            "🔑 権限を設定中...",
                                            "🔑 Configuring permissions...",
                                        )
                                    } else {
                                        tr(
                                            lang,
                                            "🔑 全画面キー監視を有効化 (パスワード入力1回)",
                                            "🔑 Enable Global Key Monitoring (1x Password)",
                                        )
                                    }),
                                )
                                .clicked()
                            {
                                start_evdev_setup = true;
                            }
                        }
                    }

                    if start_evdev_setup {
                        let (tx, rx) = mpsc::channel();
                        draft.evdev_setup_rx = Some(rx);
                        draft.error_message = None;
                        let notify_ctx = ctx.clone();
                        std::thread::spawn(move || {
                            let cmd = "printf '%s\\n' 'KERNEL==\"event*\", SUBSYSTEM==\"input\", ENV{ID_INPUT_KEYBOARD}==\"1\", TAG+=\"uaccess\"' > /etc/udev/rules.d/99-clipon-input.rules && udevadm control --reload-rules && udevadm trigger --subsystem-match=input";
                            let result = Command::new("pkexec")
                                .args(["sh", "-c", cmd])
                                .output()
                                .map_err(|e| {
                                    match lang {
                                        UiLanguage::Ja => format!("pkexec の起動に失敗しました: {e}"),
                                        UiLanguage::En => format!("Failed to start pkexec: {e}"),
                                    }
                                })
                                .and_then(|out| {
                                    if out.status.success() {
                                        Ok(())
                                    } else {
                                        let msg =
                                            String::from_utf8_lossy(&out.stderr).trim().to_string();
                                        if msg.is_empty() {
                                            Err(tr(
                                                lang,
                                                "権限設定がキャンセルまたは拒否されました",
                                                "Permission setup was canceled or denied",
                                            )
                                            .to_string())
                                        } else {
                                            Err(match lang {
                                                UiLanguage::Ja => {
                                                    format!("権限設定に失敗しました: {msg}")
                                                }
                                                UiLanguage::En => {
                                                    format!("Permission setup failed: {msg}")
                                                }
                                            })
                                        }
                                    }
                                });
                            let _ = tx.send(result);
                            notify_ctx.request_repaint();
                        });
                    }

                    if let Some(err) = &draft.error_message {
                        ui.add_space(4.0);
                        ui.colored_label(ui.visuals().error_fg_color, err);
                    }
                });
        });

        if toggle_capture {
            if let Some(draft) = &mut self.settings {
                if draft.capturing_key {
                    draft.capturing_key = false;
                    draft.capture_rx = None;
                    self.shared.keyhook.stop_capture();
                } else {
                    let (tx, rx) = mpsc::channel();
                    draft.capture_rx = Some(rx);
                    draft.capturing_key = true;
                    draft.prev_modifiers = ctx.input(|i| i.modifiers);
                    // Space や Enter のキー録音がボタン再押下として処理されないようフォーカスを解放する。
                    if let Some(focused_id) = ctx.memory(|m| m.focused()) {
                        ctx.memory_mut(|m| m.surrender_focus(focused_id));
                    }
                    self.shared.keyhook.start_capture(tx);
                    ctx.request_repaint_after(Duration::from_millis(20));
                }
            }
        }

        if trigger_save {
            self.save_settings_and_close(ctx);
        } else if trigger_cancel {
            self.hide_window(ctx);
        }
    }
}

impl eframe::App for ClipOnApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_ui_commands(ctx);

        // OS の閉じるボタン押下時は、明示的終了司令 (`Quit`) 以外ではプロセスを殺さず非表示へ戻す。
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(ctx);
            return;
        }

        match self.mode {
            UiMode::Hidden => {
                // eframe 初回フレーム後に内部で set_visible(true) が走る挙動を抑止し非表示常駐を維持する。
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
            UiMode::Popup => self.render_popup(ctx),
            UiMode::Settings => self.render_settings(ctx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_popup_position_keeps_window_inside_screen_bounds() {
        let win = (420.0, 360.0);
        let screen = (1920.0, 1080.0);

        // 通常位置はカーソル少し右下 (x + 8, y + 8)
        assert_eq!(clamp_popup_position((100, 200), win, screen), (108.0, 208.0));

        // 画面右端・下端近くではウィンドウ全体が画面内に収まるようクランプされる
        assert_eq!(
            clamp_popup_position((1850, 1020), win, screen),
            (1500.0, 720.0)
        );

        // 負座標や上端パネル領域は上端マージン (POPUP_MIN_TOP_MARGIN) にクランプされ、
        // 画面より大きいウィンドウでは安全に (0, 0) へ収まる
        assert_eq!(
            clamp_popup_position((-50, -10), win, screen),
            (0.0, POPUP_MIN_TOP_MARGIN)
        );
        assert_eq!(
            clamp_popup_position((100, 100), (500.0, 400.0), (320.0, 240.0)),
            (0.0, 0.0)
        );
    }

    #[test]
    fn navigation_index_wraps_around_list() {
        assert_eq!(move_selection(0, 1, 0), 0);
        assert_eq!(move_selection(0, -1, 0), 0);
        assert_eq!(move_selection(0, 1, 1), 0);
        assert_eq!(move_selection(0, -1, 1), 0);

        assert_eq!(move_selection(0, 1, 5), 1);
        assert_eq!(move_selection(4, 1, 5), 0);
        assert_eq!(move_selection(0, -1, 5), 4);
        assert_eq!(move_selection(2, -1, 5), 1);
    }

    #[test]
    fn find_japanese_font_path_returns_existing_file_on_host() {
        let found = find_japanese_font_path();
        if let Some(path) = &found {
            assert!(
                path.is_file(),
                "返却されたフォントパスが実在ファイルであるべきです: {}",
                path.display()
            );
            assert!(
                is_valid_font_file(path),
                "有効な TTF/OTF/TTC フォントファイルであるべきです: {}",
                path.display()
            );
        }

        // この開発ホスト上の fc-match で日本語フォントが解決できる場合は必ず発見できること
        if let Ok(out) = Command::new("fc-match").args(["-f", "%{file}", ":lang=ja"]).output() {
            if out.status.success() {
                let path_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !path_str.is_empty() && Path::new(&path_str).is_file() {
                    assert!(
                        found.is_some(),
                        "ホストに日本語フォント ({path_str}) があるにもかかわらず発見できませんでした"
                    );
                }
            }
        }
    }

    #[test]
    fn egui_key_to_trigger_name_maps_supported_keys() {
        assert_eq!(egui_key_to_trigger_name(egui::Key::A), Some("a".to_string()));
        assert_eq!(
            egui_key_to_trigger_name(egui::Key::F12),
            Some("f12".to_string())
        );
        assert_eq!(
            egui_key_to_trigger_name(egui::Key::Num0),
            Some("0".to_string())
        );
        assert_eq!(
            egui_key_to_trigger_name(egui::Key::Space),
            Some("space".to_string())
        );
        assert_eq!(egui_key_to_trigger_name(egui::Key::F24), None);
    }

    #[test]
    fn resolve_popup_position_prefers_saved_position_and_centers_when_origin_unavailable() {
        let win = (420.0, 300.0);
        let screen = (1920.0, 1080.0);

        // 記憶済み座標がある場合はカーソル座標や中央より優先し、画面内にクランプする
        assert_eq!(
            resolve_popup_position(Some((700, 400)), Some((100, 100)), win, screen),
            (700.0, 400.0)
        );
        assert_eq!(
            resolve_popup_position(Some((1800, 950)), None, win, screen),
            (1500.0, 780.0)
        );

        // 過去に負の Y 座標や上端パネル領域（< POPUP_MIN_TOP_MARGIN）が保存されていた場合でも安全マージンへ補正する
        assert_eq!(
            resolve_popup_position(Some((1478, -5)), None, win, screen),
            (1478.0, POPUP_MIN_TOP_MARGIN)
        );

        // 連続して開閉を繰り返しても保存座標が一切ドリフトしないこと（不動点条件）
        let mut pos = (1478, 520);
        for _ in 0..20 {
            let (rx, ry) = resolve_popup_position(Some(pos), Some((300, 300)), win, screen);
            pos = (rx.round() as i32, ry.round() as i32);
        }
        assert_eq!(pos, (1478, 520));

        // 未保存かつ XQueryPointer が (0, 0) の場合は左上張り付きではなく画面中央にする
        assert_eq!(
            resolve_popup_position(None, Some((0, 0)), win, screen),
            (750.0, 390.0)
        );

        // 未保存かつ有効なカーソル座標がある場合はカーソル少し右下に表示する
        assert_eq!(
            resolve_popup_position(None, Some((200, 300)), win, screen),
            (208.0, 308.0)
        );
    }

    #[test]
    fn tr_returns_expected_string_for_language() {
        assert_eq!(
            tr(UiLanguage::Ja, "保存して閉じる", "Save & Close"),
            "保存して閉じる"
        );
        assert_eq!(
            tr(UiLanguage::En, "保存して閉じる", "Save & Close"),
            "Save & Close"
        );
    }
}

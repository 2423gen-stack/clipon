//! evdev および X11 フォールバックによるグローバルキー監視モジュール。

use crate::config::TriggerConfig;
use crate::trigger::{TriggerDetector, TriggerKey, TriggerSpec};
use evdev::{Device, EventSummary, KeyCode};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use x11rb::protocol::xproto::ConnectionExt as _;

/// 自己出力ループを防ぐため監視対象から除外する仮想キーボード名。
const VIRTUAL_KEYBOARD_NAME: &str = "clipon-virtual-keyboard";

/// Linux のデバイス切断エラー番号 (`ENODEV`)。
const ERRNO_ENODEV: i32 = 19;

/// evdev ポーリング待機時間（ミリ秒）。
const EVDEV_IDLE_SLEEP_MS: u64 = 8;

/// X11 `query_keymap` ポーリング間隔（ミリ秒）。
const X11_POLL_INTERVAL_MS: u64 = 12;

/// X11 接続不可時の待機時間（ミリ秒）。
const NO_BACKEND_IDLE_SLEEP_MS: u64 = 50;

/// 通常キーの evdev [`KeyCode`] と正規化キー名の対応表。
const EVDEV_KEY_NAME_TABLE: &[(KeyCode, &str)] = &[
    (KeyCode::KEY_A, "a"),
    (KeyCode::KEY_B, "b"),
    (KeyCode::KEY_C, "c"),
    (KeyCode::KEY_D, "d"),
    (KeyCode::KEY_E, "e"),
    (KeyCode::KEY_F, "f"),
    (KeyCode::KEY_G, "g"),
    (KeyCode::KEY_H, "h"),
    (KeyCode::KEY_I, "i"),
    (KeyCode::KEY_J, "j"),
    (KeyCode::KEY_K, "k"),
    (KeyCode::KEY_L, "l"),
    (KeyCode::KEY_M, "m"),
    (KeyCode::KEY_N, "n"),
    (KeyCode::KEY_O, "o"),
    (KeyCode::KEY_P, "p"),
    (KeyCode::KEY_Q, "q"),
    (KeyCode::KEY_R, "r"),
    (KeyCode::KEY_S, "s"),
    (KeyCode::KEY_T, "t"),
    (KeyCode::KEY_U, "u"),
    (KeyCode::KEY_V, "v"),
    (KeyCode::KEY_W, "w"),
    (KeyCode::KEY_X, "x"),
    (KeyCode::KEY_Y, "y"),
    (KeyCode::KEY_Z, "z"),
    (KeyCode::KEY_0, "0"),
    (KeyCode::KEY_1, "1"),
    (KeyCode::KEY_2, "2"),
    (KeyCode::KEY_3, "3"),
    (KeyCode::KEY_4, "4"),
    (KeyCode::KEY_5, "5"),
    (KeyCode::KEY_6, "6"),
    (KeyCode::KEY_7, "7"),
    (KeyCode::KEY_8, "8"),
    (KeyCode::KEY_9, "9"),
    (KeyCode::KEY_F1, "f1"),
    (KeyCode::KEY_F2, "f2"),
    (KeyCode::KEY_F3, "f3"),
    (KeyCode::KEY_F4, "f4"),
    (KeyCode::KEY_F5, "f5"),
    (KeyCode::KEY_F6, "f6"),
    (KeyCode::KEY_F7, "f7"),
    (KeyCode::KEY_F8, "f8"),
    (KeyCode::KEY_F9, "f9"),
    (KeyCode::KEY_F10, "f10"),
    (KeyCode::KEY_F11, "f11"),
    (KeyCode::KEY_F12, "f12"),
    (KeyCode::KEY_ESC, "escape"),
    (KeyCode::KEY_SPACE, "space"),
    (KeyCode::KEY_TAB, "tab"),
    (KeyCode::KEY_ENTER, "enter"),
    (KeyCode::KEY_KPENTER, "enter"),
    (KeyCode::KEY_BACKSPACE, "backspace"),
    (KeyCode::KEY_CAPSLOCK, "capslock"),
    (KeyCode::KEY_INSERT, "insert"),
    (KeyCode::KEY_DELETE, "delete"),
    (KeyCode::KEY_HOME, "home"),
    (KeyCode::KEY_END, "end"),
    (KeyCode::KEY_PAGEUP, "pageup"),
    (KeyCode::KEY_PAGEDOWN, "pagedown"),
    (KeyCode::KEY_UP, "up"),
    (KeyCode::KEY_DOWN, "down"),
    (KeyCode::KEY_LEFT, "left"),
    (KeyCode::KEY_RIGHT, "right"),
    (KeyCode::KEY_PAUSE, "pause"),
    (KeyCode::KEY_SCROLLLOCK, "scrolllock"),
    (KeyCode::KEY_SYSRQ, "print"),
    (KeyCode::KEY_MINUS, "minus"),
    (KeyCode::KEY_EQUAL, "equal"),
    (KeyCode::KEY_GRAVE, "grave"),
    (KeyCode::KEY_LEFTBRACE, "leftbrace"),
    (KeyCode::KEY_RIGHTBRACE, "rightbrace"),
    (KeyCode::KEY_BACKSLASH, "backslash"),
    (KeyCode::KEY_SEMICOLON, "semicolon"),
    (KeyCode::KEY_APOSTROPHE, "apostrophe"),
    (KeyCode::KEY_COMMA, "comma"),
    (KeyCode::KEY_DOT, "dot"),
    (KeyCode::KEY_SLASH, "slash"),
    (KeyCode::KEY_ZENKAKUHANKAKU, "zenkakuhankaku"),
    (KeyCode::KEY_HENKAN, "henkan"),
    (KeyCode::KEY_MUHENKAN, "muhenkan"),
];

/// キー監視バックエンドの稼働状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyHookBackend {
    /// `/dev/input/event*` を直接監視する evdev バックエンド。
    Evdev {
        /// 監視対象として開けたキーボードデバイスの台数。
        device_count: usize,
    },
    /// X11 `query_keymap` ポーリングによるフォールバックバックエンド。
    X11Fallback,
}

/// evdev の [`KeyCode`] を [`TriggerKey`] へ変換する。
pub fn evdev_keycode_to_trigger_key(code: KeyCode) -> Option<TriggerKey> {
    match code {
        KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT => Some(TriggerKey::Shift),
        KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL => Some(TriggerKey::Control),
        KeyCode::KEY_LEFTALT | KeyCode::KEY_RIGHTALT => Some(TriggerKey::Alt),
        KeyCode::KEY_LEFTMETA | KeyCode::KEY_RIGHTMETA => Some(TriggerKey::Super),
        _ => EVDEV_KEY_NAME_TABLE
            .iter()
            .find(|(entry_code, _)| *entry_code == code)
            .map(|(_, name)| TriggerKey::Key((*name).to_string())),
    }
}

/// X11 キーコードを Linux 標準の `evdev + 8` オフセットで [`TriggerKey`] へ変換する。
pub fn x11_keycode_to_trigger_key(keycode: u8) -> Option<TriggerKey> {
    let evdev_code = keycode.checked_sub(8)?;
    evdev_keycode_to_trigger_key(KeyCode::new(u16::from(evdev_code)))
}

/// 監視スレッドと制御ハンドル間で共有する状態。
struct HookSharedState {
    spec: TriggerSpec,
    detector: TriggerDetector,
    capture_tx: Option<Sender<TriggerKey>>,
}

impl HookSharedState {
    /// 初期設定から共有状態を構築する。
    fn new(cfg: &TriggerConfig) -> Self {
        let spec = TriggerSpec::from_config(cfg);
        let detector = TriggerDetector::new(spec.clone());
        Self {
            spec,
            detector,
            capture_tx: None,
        }
    }

    /// 受信した単一のキーイベントを処理し、トリガー発火時に `true` を返す。
    fn process_event(&mut self, key: TriggerKey, pressed: bool, now_ms: u64) -> bool {
        if self.capture_tx.is_some() {
            if pressed {
                if let Some(tx) = self.capture_tx.take() {
                    let _ = tx.send(key);
                }
                self.detector.update_spec(self.spec.clone());
            }
            return false;
        }
        self.detector.on_key_event(&key, pressed, now_ms)
    }
}

/// バックグラウンドで動作するグローバルキー監視スレッドの制御ハンドル。
pub struct KeyHookHandle {
    backend: KeyHookBackend,
    shared: Arc<Mutex<HookSharedState>>,
    stop_flag: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl KeyHookHandle {
    /// 指定された初期設定とコールバックでキー監視スレッドを起動する。
    pub fn spawn(initial_config: TriggerConfig, on_trigger: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Mutex::new(HookSharedState::new(&initial_config)));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let (backend_tx, backend_rx) = mpsc::sync_channel(1);
        let worker_shared = Arc::clone(&shared);
        let worker_stop = Arc::clone(&stop_flag);

        let handle = thread::spawn(move || {
            let devices = collect_keyboard_devices();
            let backend = if devices.is_empty() {
                KeyHookBackend::X11Fallback
            } else {
                KeyHookBackend::Evdev {
                    device_count: devices.len(),
                }
            };
            let _ = backend_tx.send(backend);

            let started_at = Instant::now();
            match backend {
                KeyHookBackend::Evdev { .. } => {
                    run_evdev_loop(devices, &worker_shared, &worker_stop, started_at, &on_trigger);
                }
                KeyHookBackend::X11Fallback => {
                    run_x11_fallback_loop(&worker_shared, &worker_stop, started_at, &on_trigger);
                }
            }
        });

        let backend = backend_rx.recv().unwrap_or(KeyHookBackend::X11Fallback);

        Self {
            backend,
            shared,
            stop_flag,
            worker: Mutex::new(Some(handle)),
        }
    }

    /// 監視中のトリガー設定を動的に更新する。
    pub fn update_config(&self, cfg: TriggerConfig) {
        if let Ok(mut guard) = self.shared.lock() {
            let new_spec = TriggerSpec::from_config(&cfg);
            guard.spec = new_spec.clone();
            guard.detector.update_spec(new_spec);
        }
    }

    /// 現在使用中のキー監視バックエンド状態を返す。
    pub fn backend_status(&self) -> KeyHookBackend {
        self.backend
    }

    /// 設定画面のキー録音モードを開始し、次に押下されたキーを 1 回だけ送信する。
    pub fn start_capture(&self, tx: Sender<TriggerKey>) {
        if let Ok(mut guard) = self.shared.lock() {
            guard.capture_tx = Some(tx);
        }
    }

    /// キー録音モードを停止し、通常のトリガー検知を再開する。
    pub fn stop_capture(&self) {
        if let Ok(mut guard) = self.shared.lock() {
            guard.capture_tx = None;
            let spec = guard.spec.clone();
            guard.detector.update_spec(spec);
        }
    }

    /// キー監視スレッドへ終了を通知して待機する。
    pub fn shutdown(&self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Ok(mut guard) = self.worker.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for KeyHookHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 自己出力ループ回避用にデバイス名を検査し、監視除外対象なら `true` を返す。
fn is_ignored_virtual_device(name: Option<&str>) -> bool {
    name == Some(VIRTUAL_KEYBOARD_NAME)
}

/// `/dev/input/event*` から読み取り可能なキーボードデバイスを収集する。
fn collect_keyboard_devices() -> Vec<Device> {
    let Ok(entries) = fs::read_dir("/dev/input") else {
        return Vec::new();
    };

    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("event"))
        })
        .collect();
    paths.sort();

    let mut devices = Vec::new();
    for path in paths {
        let Ok(device) = Device::open(&path) else {
            continue;
        };
        if is_ignored_virtual_device(device.name()) {
            continue;
        }
        let is_keyboard = device.supported_keys().is_some_and(|keys| {
            keys.contains(KeyCode::KEY_A) && keys.contains(KeyCode::KEY_LEFTSHIFT)
        });
        if !is_keyboard {
            continue;
        }
        if device.set_nonblocking(true).is_err() {
            continue;
        }
        devices.push(device);
    }
    devices
}

/// 共有状態をロックして単一キーイベントを処理し、必要ならトリガーコールバックを実行する。
fn dispatch_key_event(
    shared: &Arc<Mutex<HookSharedState>>,
    key: TriggerKey,
    pressed: bool,
    started_at: Instant,
    on_trigger: &(dyn Fn() + Send),
) {
    let now_ms = started_at.elapsed().as_millis() as u64;
    let fired = match shared.lock() {
        Ok(mut guard) => guard.process_event(key, pressed, now_ms),
        Err(_) => false,
    };
    if fired {
        on_trigger();
    }
}

/// evdev バックエンドのノンブロッキングポーリングループを実行する。
fn run_evdev_loop(
    mut devices: Vec<Device>,
    shared: &Arc<Mutex<HookSharedState>>,
    stop_flag: &AtomicBool,
    started_at: Instant,
    on_trigger: &(dyn Fn() + Send),
) {
    while !stop_flag.load(Ordering::Acquire) {
        let mut had_events = false;
        let mut disconnected = Vec::new();

        for (idx, device) in devices.iter_mut().enumerate() {
            match device.fetch_events() {
                Ok(events) => {
                    for event in events {
                        had_events = true;
                        if let EventSummary::Key(_, code, value) = event.destructure() {
                            let pressed = match value {
                                1 => true,
                                0 => false,
                                // value == 2 のキーリピート等は無視する
                                _ => continue,
                            };
                            if let Some(key) = evdev_keycode_to_trigger_key(code) {
                                dispatch_key_event(shared, key, pressed, started_at, on_trigger);
                            }
                        }
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                Err(err) if err.raw_os_error() == Some(ERRNO_ENODEV) => {
                    disconnected.push(idx);
                }
                Err(_) => {}
            }
        }

        for idx in disconnected.into_iter().rev() {
            devices.remove(idx);
        }

        if !had_events {
            thread::sleep(Duration::from_millis(EVDEV_IDLE_SLEEP_MS));
        }
    }
}

/// 前回と今回の X11 キーマップビットマスク（32 バイト）の差分から `(keycode, pressed)` 列を抽出する。
fn diff_x11_keymap(prev: &[u8; 32], curr: &[u8; 32]) -> Vec<(u8, bool)> {
    let mut edges = Vec::new();
    for byte_idx in 0..32 {
        let diff = prev[byte_idx] ^ curr[byte_idx];
        if diff == 0 {
            continue;
        }
        for bit in 0..8 {
            if (diff & (1 << bit)) != 0 {
                let keycode = (byte_idx * 8 + bit) as u8;
                let pressed = (curr[byte_idx] & (1 << bit)) != 0;
                edges.push((keycode, pressed));
            }
        }
    }
    edges
}

/// X11 `query_keymap` ポーリングによるフォールバックループを実行する。
fn run_x11_fallback_loop(
    shared: &Arc<Mutex<HookSharedState>>,
    stop_flag: &AtomicBool,
    started_at: Instant,
    on_trigger: &(dyn Fn() + Send),
) {
    let Ok((conn, _)) = x11rb::connect(None) else {
        while !stop_flag.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(NO_BACKEND_IDLE_SLEEP_MS));
        }
        return;
    };

    let mut prev_keymap = conn
        .query_keymap()
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| reply.keys)
        .unwrap_or([0u8; 32]);

    while !stop_flag.load(Ordering::Acquire) {
        let Ok(cookie) = conn.query_keymap() else {
            break;
        };
        let Ok(reply) = cookie.reply() else {
            break;
        };

        let edges = diff_x11_keymap(&prev_keymap, &reply.keys);
        prev_keymap = reply.keys;

        for (keycode, pressed) in edges {
            if let Some(key) = x11_keycode_to_trigger_key(keycode) {
                dispatch_key_event(shared, key, pressed, started_at, on_trigger);
            }
        }

        thread::sleep(Duration::from_millis(X11_POLL_INTERVAL_MS));
    }

    while !stop_flag.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(NO_BACKEND_IDLE_SLEEP_MS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evdev_keycode_mapping_handles_modifiers_and_alphanumerics() {
        assert_eq!(
            evdev_keycode_to_trigger_key(KeyCode::KEY_LEFTSHIFT),
            Some(TriggerKey::Shift)
        );
        assert_eq!(
            evdev_keycode_to_trigger_key(KeyCode::KEY_RIGHTSHIFT),
            Some(TriggerKey::Shift)
        );
        assert_eq!(
            evdev_keycode_to_trigger_key(KeyCode::KEY_LEFTCTRL),
            Some(TriggerKey::Control)
        );
        assert_eq!(
            evdev_keycode_to_trigger_key(KeyCode::KEY_LEFTALT),
            Some(TriggerKey::Alt)
        );
        assert_eq!(
            evdev_keycode_to_trigger_key(KeyCode::KEY_LEFTMETA),
            Some(TriggerKey::Super)
        );
        assert_eq!(
            evdev_keycode_to_trigger_key(KeyCode::KEY_V),
            Some(TriggerKey::Key("v".to_string()))
        );
        assert_eq!(
            evdev_keycode_to_trigger_key(KeyCode::KEY_F1),
            Some(TriggerKey::Key("f1".to_string()))
        );
    }

    #[test]
    fn x11_keycode_mapping_matches_evdev_plus_8_offset() {
        assert_eq!(x11_keycode_to_trigger_key(50), Some(TriggerKey::Shift));
        assert_eq!(x11_keycode_to_trigger_key(37), Some(TriggerKey::Control));
        assert_eq!(
            x11_keycode_to_trigger_key(55),
            Some(TriggerKey::Key("v".to_string()))
        );
        assert_eq!(x11_keycode_to_trigger_key(7), None);
    }

    #[test]
    fn x11_keymap_diff_detects_press_and_release_edges() {
        let mut prev = [0u8; 32];
        let mut curr = [0u8; 32];

        // キーコード 50 (byte 6, bit 2) を押下、キーコード 37 (byte 4, bit 5) を解放
        prev[4] = 1 << 5;
        curr[6] = 1 << 2;

        let edges = diff_x11_keymap(&prev, &curr);
        assert_eq!(edges, vec![(37, false), (50, true)]);
    }

    #[test]
    fn capture_mode_suspends_trigger_and_emits_first_pressed_key() {
        let cfg = TriggerConfig {
            key: "Shift".to_string(),
            tap_count: 2,
            interval_ms: 350,
        };
        let mut state = HookSharedState::new(&cfg);
        let (tx, rx) = mpsc::channel();
        state.capture_tx = Some(tx);

        // 解放イベントは無視し、押下イベントで 1 回だけ送信して録音終了する
        assert!(!state.process_event(TriggerKey::Shift, false, 10));
        assert!(rx.try_recv().is_err());

        assert!(!state.process_event(TriggerKey::Alt, true, 20));
        assert_eq!(rx.try_recv().ok(), Some(TriggerKey::Alt));
        assert!(state.capture_tx.is_none());

        // 録音終了後は通常のダブルタップ検知が動作すること
        assert!(!state.process_event(TriggerKey::Shift, true, 100));
        assert!(!state.process_event(TriggerKey::Shift, false, 140));
        assert!(!state.process_event(TriggerKey::Shift, true, 200));
        assert!(state.process_event(TriggerKey::Shift, false, 240));
    }

    #[test]
    fn virtual_keyboard_guard_skips_clipon_uinput_device() {
        assert!(is_ignored_virtual_device(Some("clipon-virtual-keyboard")));
        assert!(!is_ignored_virtual_device(Some("AT Translated Set 2 keyboard")));
        assert!(!is_ignored_virtual_device(None));
    }
}

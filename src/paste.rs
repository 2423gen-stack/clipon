//! uinput 仮想キーボードまたは xdotool フォールバックによるペーストキー送出モジュール。

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};
use std::process::Command;
use std::time::Duration;
use x11rb::protocol::xproto::ConnectionExt as _;

/// 押下と解放の間に挿入する待機時間（ミリ秒）。
const KEY_PRESS_HOLD_MS: u64 = 8;

/// 通常キーの正規化名・evdev キーコード・xdotool 表記の対応表。
const MAIN_KEY_TABLE: &[(&str, KeyCode, &str)] = &[
    ("a", KeyCode::KEY_A, "a"),
    ("b", KeyCode::KEY_B, "b"),
    ("c", KeyCode::KEY_C, "c"),
    ("d", KeyCode::KEY_D, "d"),
    ("e", KeyCode::KEY_E, "e"),
    ("f", KeyCode::KEY_F, "f"),
    ("g", KeyCode::KEY_G, "g"),
    ("h", KeyCode::KEY_H, "h"),
    ("i", KeyCode::KEY_I, "i"),
    ("j", KeyCode::KEY_J, "j"),
    ("k", KeyCode::KEY_K, "k"),
    ("l", KeyCode::KEY_L, "l"),
    ("m", KeyCode::KEY_M, "m"),
    ("n", KeyCode::KEY_N, "n"),
    ("o", KeyCode::KEY_O, "o"),
    ("p", KeyCode::KEY_P, "p"),
    ("q", KeyCode::KEY_Q, "q"),
    ("r", KeyCode::KEY_R, "r"),
    ("s", KeyCode::KEY_S, "s"),
    ("t", KeyCode::KEY_T, "t"),
    ("u", KeyCode::KEY_U, "u"),
    ("v", KeyCode::KEY_V, "v"),
    ("w", KeyCode::KEY_W, "w"),
    ("x", KeyCode::KEY_X, "x"),
    ("y", KeyCode::KEY_Y, "y"),
    ("z", KeyCode::KEY_Z, "z"),
    ("0", KeyCode::KEY_0, "0"),
    ("1", KeyCode::KEY_1, "1"),
    ("2", KeyCode::KEY_2, "2"),
    ("3", KeyCode::KEY_3, "3"),
    ("4", KeyCode::KEY_4, "4"),
    ("5", KeyCode::KEY_5, "5"),
    ("6", KeyCode::KEY_6, "6"),
    ("7", KeyCode::KEY_7, "7"),
    ("8", KeyCode::KEY_8, "8"),
    ("9", KeyCode::KEY_9, "9"),
    ("insert", KeyCode::KEY_INSERT, "Insert"),
    ("enter", KeyCode::KEY_ENTER, "Return"),
    ("space", KeyCode::KEY_SPACE, "space"),
    ("tab", KeyCode::KEY_TAB, "Tab"),
    ("f1", KeyCode::KEY_F1, "F1"),
    ("f2", KeyCode::KEY_F2, "F2"),
    ("f3", KeyCode::KEY_F3, "F3"),
    ("f4", KeyCode::KEY_F4, "F4"),
    ("f5", KeyCode::KEY_F5, "F5"),
    ("f6", KeyCode::KEY_F6, "F6"),
    ("f7", KeyCode::KEY_F7, "F7"),
    ("f8", KeyCode::KEY_F8, "F8"),
    ("f9", KeyCode::KEY_F9, "F9"),
    ("f10", KeyCode::KEY_F10, "F10"),
    ("f11", KeyCode::KEY_F11, "F11"),
    ("f12", KeyCode::KEY_F12, "F12"),
];

/// 仮想キーボードに登録する修飾キーの evdev キーコード一覧（`ctrl:swapcaps` 等にも対応するため左右および CapsLock を網羅）。
const MODIFIER_KEY_CODES: &[KeyCode] = &[
    KeyCode::KEY_LEFTCTRL,
    KeyCode::KEY_RIGHTCTRL,
    KeyCode::KEY_CAPSLOCK,
    KeyCode::KEY_LEFTSHIFT,
    KeyCode::KEY_RIGHTSHIFT,
    KeyCode::KEY_LEFTALT,
    KeyCode::KEY_RIGHTALT,
    KeyCode::KEY_LEFTMETA,
    KeyCode::KEY_RIGHTMETA,
];

/// パース済みのペーストショートカット表現。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteShortcut {
    /// 先行して押下する修飾キー列。
    pub modifiers: Vec<KeyCode>,
    /// 主キー。
    pub key: KeyCode,
}

/// X11 (`GetModifierMapping`) から取得した各修飾子の有効な物理 evdev キーコード表。
///
/// `ctrl:swapcaps` 等の XKB オプションが有効な環境では、物理 `KEY_LEFTCTRL` (evdev 29) が
/// `Caps_Lock` (Lock 修飾) に割り当てられるため、実際のモディファイアマッピングを参照して
/// CapsLock にならない安全な物理キー（`KEY_RIGHTCTRL` や `KEY_CAPSLOCK`）へ解決する。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActiveModifierMap {
    /// Shift (mod index 0) に割り当てられている evdev キーコード群。
    pub shift: Vec<KeyCode>,
    /// Lock / CapsLock (mod index 1) に割り当てられている evdev キーコード群。
    pub lock: Vec<KeyCode>,
    /// Control (mod index 2) に割り当てられている evdev キーコード群。
    pub control: Vec<KeyCode>,
    /// Alt / Mod1 (mod index 3) に割り当てられている evdev キーコード群。
    pub alt: Vec<KeyCode>,
    /// Super / Mod4 (mod index 6) に割り当てられている evdev キーコード群。
    pub super_mod: Vec<KeyCode>,
}

impl ActiveModifierMap {
    /// X11 `GetModifierMapping` の応答データ（1修飾子あたりのキーコード数および全8行の配列）から構築する。
    pub fn from_x11_reply(keycodes_per_modifier: u8, keycodes: &[u8]) -> Self {
        let k = usize::from(keycodes_per_modifier);
        if k == 0 || keycodes.len() < 8 * k {
            return Self::default();
        }
        let extract_row = |row: usize| -> Vec<KeyCode> {
            let start = row * k;
            let end = start + k;
            keycodes[start..end]
                .iter()
                .copied()
                .filter(|&x11_kc| x11_kc >= 8)
                .map(|x11_kc| KeyCode::new(u16::from(x11_kc - 8)))
                .collect()
        };

        Self {
            shift: extract_row(0),
            lock: extract_row(1),
            control: extract_row(2),
            alt: extract_row(3),
            super_mod: extract_row(6),
        }
    }

    /// 稼働中の X11 / XWayland サーバーから現在のモディファイアマッピングを取得する。
    pub fn query_x11() -> Option<Self> {
        let (conn, _) = x11rb::connect(None).ok()?;
        let reply = conn.get_modifier_mapping().ok()?.reply().ok()?;
        Some(Self::from_x11_reply(
            reply.keycodes_per_modifier(),
            &reply.keycodes,
        ))
    }

    /// 論理的な修飾キーに対して、Lock (`Caps_Lock`) を絶対に踏まない有効な物理 evdev [`KeyCode`] を返す。
    pub fn resolve_modifier(&self, logical_mod: KeyCode) -> KeyCode {
        match logical_mod {
            KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL | KeyCode::KEY_CAPSLOCK => {
                // ctrl:swapcaps / ctrl:nocaps / ctrl:lctrl_meta 等の影響を最も受けにくい
                // KEY_RIGHTCTRL を第1候補とし、X11 マッピングと照合して Lock 以外のキーを選ぶ。
                let candidates = [
                    KeyCode::KEY_RIGHTCTRL,
                    KeyCode::KEY_LEFTCTRL,
                    KeyCode::KEY_CAPSLOCK,
                ];
                if !self.control.is_empty() {
                    for &cand in &candidates {
                        if self.control.contains(&cand) && !self.lock.contains(&cand) {
                            return cand;
                        }
                    }
                    for &mapped in &self.control {
                        if MODIFIER_KEY_CODES.contains(&mapped) && !self.lock.contains(&mapped) {
                            return mapped;
                        }
                    }
                }
                if !self.lock.contains(&KeyCode::KEY_RIGHTCTRL) {
                    KeyCode::KEY_RIGHTCTRL
                } else {
                    KeyCode::KEY_CAPSLOCK
                }
            }
            KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT => {
                let candidates = [KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_RIGHTSHIFT];
                if !self.shift.is_empty() {
                    for &cand in &candidates {
                        if self.shift.contains(&cand) && !self.lock.contains(&cand) {
                            return cand;
                        }
                    }
                }
                KeyCode::KEY_LEFTSHIFT
            }
            KeyCode::KEY_LEFTALT | KeyCode::KEY_RIGHTALT => {
                let candidates = [KeyCode::KEY_LEFTALT, KeyCode::KEY_RIGHTALT];
                if !self.alt.is_empty() {
                    for &cand in &candidates {
                        if self.alt.contains(&cand) && !self.lock.contains(&cand) {
                            return cand;
                        }
                    }
                }
                KeyCode::KEY_LEFTALT
            }
            KeyCode::KEY_LEFTMETA | KeyCode::KEY_RIGHTMETA => {
                let candidates = [KeyCode::KEY_LEFTMETA, KeyCode::KEY_RIGHTMETA];
                if !self.super_mod.is_empty() {
                    for &cand in &candidates {
                        if self.super_mod.contains(&cand) && !self.lock.contains(&cand) {
                            return cand;
                        }
                    }
                }
                KeyCode::KEY_LEFTMETA
            }
            other => other,
        }
    }

    /// [`PasteShortcut`] 内の全修飾キーを現在のキーマップで安全な物理 [`KeyCode`] へ変換する。
    pub fn resolve_shortcut(&self, shortcut: &PasteShortcut) -> PasteShortcut {
        let mut modifiers = Vec::with_capacity(shortcut.modifiers.len());
        for &m in &shortcut.modifiers {
            let resolved = self.resolve_modifier(m);
            if !modifiers.contains(&resolved) {
                modifiers.push(resolved);
            }
        }
        PasteShortcut {
            modifiers,
            key: shortcut.key,
        }
    }
}

/// 修飾キー文字列を [`KeyCode`] へ変換する。
fn parse_modifier_key(raw: &str) -> Option<KeyCode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(KeyCode::KEY_LEFTCTRL),
        "shift" => Some(KeyCode::KEY_LEFTSHIFT),
        "alt" => Some(KeyCode::KEY_LEFTALT),
        "super" | "meta" => Some(KeyCode::KEY_LEFTMETA),
        _ => None,
    }
}

/// 主キー文字列を [`KeyCode`] へ変換する。
fn parse_main_key(raw: &str) -> Option<KeyCode> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower == "return" {
        return Some(KeyCode::KEY_ENTER);
    }
    MAIN_KEY_TABLE
        .iter()
        .find(|(name, _, _)| *name == lower)
        .map(|(_, code, _)| *code)
}

/// [`KeyCode`] を `xdotool` 用の修飾キー表記または主キー表記へ変換する。
fn keycode_to_xdotool(code: KeyCode) -> String {
    match code {
        KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL | KeyCode::KEY_CAPSLOCK => {
            "ctrl".to_string()
        }
        KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT => "shift".to_string(),
        KeyCode::KEY_LEFTALT | KeyCode::KEY_RIGHTALT => "alt".to_string(),
        KeyCode::KEY_LEFTMETA | KeyCode::KEY_RIGHTMETA => "super".to_string(),
        _ => MAIN_KEY_TABLE
            .iter()
            .find(|(_, entry_code, _)| *entry_code == code)
            .map(|(_, _, xdo)| (*xdo).to_string())
            .unwrap_or_else(|| format!("0x{:x}", code.0)),
    }
}

/// ショートカット文字列（例: `"Ctrl+v"`, `"Shift+Insert"`）を [`PasteShortcut`] へパースする。
pub fn parse_paste_shortcut(spec: &str) -> Result<PasteShortcut, String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("ペーストショートカット文字列が空です".to_string());
    }

    let parts: Vec<&str> = trimmed.split('+').map(str::trim).collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(format!("不正なショートカット表記です: {spec}"));
    }

    let (modifier_parts, main_part) = parts.split_at(parts.len() - 1);
    let mut modifiers = Vec::with_capacity(modifier_parts.len());
    for raw_mod in modifier_parts {
        let code = parse_modifier_key(raw_mod)
            .ok_or_else(|| format!("未対応の修飾キーです: {raw_mod}"))?;
        if !modifiers.contains(&code) {
            modifiers.push(code);
        }
    }

    let key = parse_main_key(main_part[0])
        .ok_or_else(|| format!("未対応の主キーです: {}", main_part[0]))?;

    Ok(PasteShortcut { modifiers, key })
}

/// [`PasteShortcut`] を `xdotool key` 用のキー表記文字列（例: `"ctrl+v"`, `"shift+Insert"`）へ変換する。
pub fn format_xdotool_key(shortcut: &PasteShortcut) -> String {
    let mut parts: Vec<String> = shortcut
        .modifiers
        .iter()
        .copied()
        .map(keycode_to_xdotool)
        .collect();
    parts.push(keycode_to_xdotool(shortcut.key));
    parts.join("+")
}

/// 仮想キーボード（`/dev/uinput`）または `xdotool` でペーストキーを送出する。
pub struct AutoPaster {
    device: Option<VirtualDevice>,
}

impl AutoPaster {
    /// `/dev/uinput` 仮想キーボードの作成を試み、失敗時は `xdotool` フォールバック構成で初期化する。
    pub fn new() -> Self {
        let device = match create_virtual_keyboard() {
            Ok(dev) => Some(dev),
            Err(err) => {
                eprintln!(
                    "ClipOn: /dev/uinput の初期化に失敗したため xdotool へフォールバックします: {err}"
                );
                None
            }
        };
        Self { device }
    }

    /// `/dev/uinput` 仮想キーボードが利用可能かを返す。
    pub fn is_uinput_ready(&self) -> bool {
        self.device.is_some()
    }

    /// 指定されたショートカット文字列に従ってペーストキーを送出する。
    pub fn send_paste(&mut self, key_spec: &str) -> Result<(), String> {
        let shortcut = parse_paste_shortcut(key_spec)?;
        if let Some(dev) = self.device.as_mut() {
            let mod_map = ActiveModifierMap::query_x11().unwrap_or_default();
            let resolved = mod_map.resolve_shortcut(&shortcut);
            emit_shortcut_via_uinput(dev, &resolved)
        } else {
            send_shortcut_via_xdotool(&shortcut)
        }
    }
}

impl Default for AutoPaster {
    fn default() -> Self {
        Self::new()
    }
}

/// サポート対象の全キーを登録した `/dev/uinput` 仮想キーボードを構築する。
fn create_virtual_keyboard() -> std::io::Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for &code in MODIFIER_KEY_CODES {
        keys.insert(code);
    }
    for &(_, code, _) in MAIN_KEY_TABLE {
        keys.insert(code);
    }

    VirtualDevice::builder()?
        .name("clipon-virtual-keyboard")
        .with_keys(&keys)?
        .build()
}

/// キー押下 (`value=1`) または解放 (`value=0`) の [`InputEvent`] を生成する。
fn key_event(code: KeyCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::KEY.0, code.0, value)
}

/// `VirtualDevice::emit`（末尾に `SYN_REPORT` を自動付与）を用いてペーストキー列を送出する。
fn emit_shortcut_via_uinput(
    dev: &mut VirtualDevice,
    shortcut: &PasteShortcut,
) -> Result<(), String> {
    if !shortcut.modifiers.is_empty() {
        let mod_downs: Vec<InputEvent> = shortcut
            .modifiers
            .iter()
            .map(|&code| key_event(code, 1))
            .collect();
        dev.emit(&mod_downs)
            .map_err(|e| format!("uinput 修飾キー押下送出に失敗しました: {e}"))?;
        std::thread::sleep(Duration::from_millis(KEY_PRESS_HOLD_MS));
    }

    dev.emit(&[key_event(shortcut.key, 1)])
        .map_err(|e| format!("uinput 主キー押下送出に失敗しました: {e}"))?;

    std::thread::sleep(Duration::from_millis(KEY_PRESS_HOLD_MS));

    dev.emit(&[key_event(shortcut.key, 0)])
        .map_err(|e| format!("uinput 主キー解放送出に失敗しました: {e}"))?;

    if !shortcut.modifiers.is_empty() {
        std::thread::sleep(Duration::from_millis(KEY_PRESS_HOLD_MS));
        let mod_ups: Vec<InputEvent> = shortcut
            .modifiers
            .iter()
            .rev()
            .map(|&code| key_event(code, 0))
            .collect();
        dev.emit(&mod_ups)
            .map_err(|e| format!("uinput 修飾キー解放送出に失敗しました: {e}"))?;
    }

    Ok(())
}

/// `xdotool` サブプロセスを起動してフォールバック送出を行う。
fn send_shortcut_via_xdotool(shortcut: &PasteShortcut) -> Result<(), String> {
    let key_expr = format_xdotool_key(shortcut);
    let status = Command::new("xdotool")
        .args(["key", "--clearmodifiers", &key_expr])
        .status()
        .map_err(|e| format!("xdotool の起動に失敗しました: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("xdotool が異常終了しました (status: {status})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ctrl_v_and_shift_insert_and_ctrl_shift_v() {
        let ctrl_v = parse_paste_shortcut("Ctrl+v").expect("Ctrl+v のパースに失敗");
        assert_eq!(ctrl_v.modifiers, vec![KeyCode::KEY_LEFTCTRL]);
        assert_eq!(ctrl_v.key, KeyCode::KEY_V);
        assert_eq!(format_xdotool_key(&ctrl_v), "ctrl+v");

        let ctrl_shift_v =
            parse_paste_shortcut("ctrl+shift+v").expect("ctrl+shift+v のパースに失敗");
        assert_eq!(
            ctrl_shift_v.modifiers,
            vec![KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT]
        );
        assert_eq!(ctrl_shift_v.key, KeyCode::KEY_V);
        assert_eq!(format_xdotool_key(&ctrl_shift_v), "ctrl+shift+v");

        let shift_insert =
            parse_paste_shortcut("Shift+Insert").expect("Shift+Insert のパースに失敗");
        assert_eq!(shift_insert.modifiers, vec![KeyCode::KEY_LEFTSHIFT]);
        assert_eq!(shift_insert.key, KeyCode::KEY_INSERT);
        assert_eq!(format_xdotool_key(&shift_insert), "shift+Insert");
    }

    #[test]
    fn parse_rejects_empty_or_unknown_key() {
        assert!(parse_paste_shortcut("").is_err());
        assert!(parse_paste_shortcut("   ").is_err());
        assert!(parse_paste_shortcut("Ctrl+").is_err());
        assert!(parse_paste_shortcut("Ctrl+UnknownFoo").is_err());
        assert!(parse_paste_shortcut("Ctrl+Shift").is_err());
    }

    #[test]
    fn parse_supports_additional_modifiers_digits_and_function_keys() {
        let super_f12 = parse_paste_shortcut("Super+Alt+F12").expect("Super+Alt+F12 のパースに失敗");
        assert_eq!(
            super_f12.modifiers,
            vec![KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTALT]
        );
        assert_eq!(super_f12.key, KeyCode::KEY_F12);
        assert_eq!(format_xdotool_key(&super_f12), "super+alt+F12");

        let control_0 = parse_paste_shortcut("Control+0").expect("Control+0 のパースに失敗");
        assert_eq!(control_0.modifiers, vec![KeyCode::KEY_LEFTCTRL]);
        assert_eq!(control_0.key, KeyCode::KEY_0);
        assert_eq!(format_xdotool_key(&control_0), "ctrl+0");
    }

    #[test]
    fn x11_modifier_map_resolves_ctrl_swapcaps_without_hitting_capslock() {
        // ctrl:swapcaps 環境の X11 GetModifierMapping (keycodes_per_modifier = 4) を再現:
        // row 0 (Shift):   0x32 (LEFTSHIFT=42), 0x3e (RIGHTSHIFT=54)
        // row 1 (Lock):    0x25 (LEFTCTRL=29 -> Caps_Lock にスワップされている!)
        // row 2 (Control): 0x42 (CAPSLOCK=58 -> Control_L), 0x69 (RIGHTCTRL=97 -> Control_R)
        // row 3 (Mod1):    0x40 (LEFTALT=56), 0x6c (RIGHTALT=100)
        // row 6 (Mod4):    0x85 (LEFTMETA=125), 0x86 (RIGHTMETA=126)
        let mut table = [0u8; 32];
        table[0] = 0x32;
        table[1] = 0x3e;
        table[4] = 0x25; // Lock = physical Left Ctrl!
        table[8] = 0x42; // Control = physical CapsLock
        table[9] = 0x69; // Control = physical Right Ctrl
        table[12] = 0x40;
        table[13] = 0x6c;
        table[24] = 0x85;
        table[25] = 0x86;

        let map = ActiveModifierMap::from_x11_reply(4, &table);
        assert_eq!(map.lock, vec![KeyCode::KEY_LEFTCTRL]);
        assert_eq!(
            map.control,
            vec![KeyCode::KEY_CAPSLOCK, KeyCode::KEY_RIGHTCTRL]
        );

        let shortcut = parse_paste_shortcut("Ctrl+v").expect("Ctrl+v パース成功");
        let resolved = map.resolve_shortcut(&shortcut);

        // KEY_LEFTCTRL は Lock (Caps_Lock) に割り当てられているため絶対に送出してはならず、
        // Control に割り当てられている KEY_RIGHTCTRL が選択されること
        assert_eq!(resolved.modifiers, vec![KeyCode::KEY_RIGHTCTRL]);
        assert!(!resolved.modifiers.contains(&KeyCode::KEY_LEFTCTRL));
        assert_eq!(resolved.key, KeyCode::KEY_V);
    }

    #[test]
    fn x11_modifier_map_resolves_when_only_capslock_is_control() {
        // 左右 Ctrl とも別キーへ潰され、物理 CapsLock (0x42 -> evdev 58) のみが Control の環境
        let mut table = [0u8; 32];
        table[4] = 0x25; // Lock = physical Left Ctrl
        table[8] = 0x42; // Control = physical CapsLock のみ

        let map = ActiveModifierMap::from_x11_reply(4, &table);
        let shortcut = parse_paste_shortcut("Ctrl+v").expect("Ctrl+v パース成功");
        let resolved = map.resolve_shortcut(&shortcut);

        assert_eq!(resolved.modifiers, vec![KeyCode::KEY_CAPSLOCK]);
    }

    #[test]
    fn default_empty_modifier_map_uses_rightctrl_to_avoid_swapcaps() {
        // X11 マッピング未取得時のフォールバックでも、ctrl:swapcaps で CapsLock 化される
        // KEY_LEFTCTRL を避けて KEY_RIGHTCTRL へ解決されること
        let map = ActiveModifierMap::default();
        let shortcut = parse_paste_shortcut("Ctrl+v").expect("Ctrl+v パース成功");
        let resolved = map.resolve_shortcut(&shortcut);

        assert_eq!(resolved.modifiers, vec![KeyCode::KEY_RIGHTCTRL]);
    }
}

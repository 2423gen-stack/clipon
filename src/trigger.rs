//! グローバルキー入力から呼び出しトリガー（連続タップ・修飾キーコンボ）を検知するステートマシン。

use crate::config::TriggerConfig;
use std::collections::HashMap;

/// 単一タップとして許容する最大押下時間（ミリ秒）。
const MAX_TAP_HOLD_MS: u64 = 500;
/// OS からのキー解放イベント欠落（スリープ復帰・フォーカス奪取・USB瞬断等）による
/// 幽霊キー残留（永久デッドロック）を自己治癒するための最大押下保持時間（ミリ秒）。
const STALE_HELD_KEY_TIMEOUT_MS: u64 = 2_000;

/// 正規化されたトリガーキー識別子。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TriggerKey {
    /// 左右 Shift キー。
    Shift,
    /// 左右 Control キー。
    Control,
    /// 左右 Alt キー。
    Alt,
    /// Super / Meta / Win キー。
    Super,
    /// その他の通常キー（小文字正規化済み）。
    Key(String),
}

/// トリガー検知条件の仕様。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerSpec {
    /// 先行して押下されている必要がある修飾キー群（重複排除済み）。
    pub modifiers: Vec<TriggerKey>,
    /// 主キー（単一タップ対象またはコンボの末尾キー）。
    pub main_key: TriggerKey,
    /// 発火に必要な連続タップ回数。
    pub tap_count: u8,
    /// 連続タップ判定を許容する最大間隔（ミリ秒）。
    pub interval_ms: u64,
}

impl TriggerSpec {
    /// [`TriggerConfig`] からトリガー仕様を構築する。
    pub fn from_config(cfg: &TriggerConfig) -> Self {
        let mut parsed: Vec<TriggerKey> = cfg
            .key
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(parse_key_name)
            .collect();

        let main_key = parsed.pop().unwrap_or(TriggerKey::Shift);
        let mut modifiers = Vec::with_capacity(parsed.len());
        for key in parsed {
            if !modifiers.contains(&key) {
                modifiers.push(key);
            }
        }

        Self {
            modifiers,
            main_key,
            tap_count: cfg.tap_count.max(1),
            interval_ms: cfg.interval_ms,
        }
    }

    /// UI・ログ表示向けの可読なトリガー文字列表現を返す。
    pub fn display_string(&self) -> String {
        let mut parts: Vec<String> = self.modifiers.iter().map(canonical_key_name).collect();
        parts.push(canonical_key_name(&self.main_key));
        let key_expr = parts.join("+");
        if self.modifiers.is_empty() && self.tap_count >= 2 {
            format!("{key_expr} x{}", self.tap_count)
        } else {
            key_expr
        }
    }
}

/// キー名文字列を大文字・小文字および左右エイリアスを吸収して [`TriggerKey`] へ正規化する。
pub fn parse_key_name(raw: &str) -> TriggerKey {
    let lower = raw.trim().to_lowercase();
    match lower.as_str() {
        "shift" | "lshift" | "rshift" => TriggerKey::Shift,
        "ctrl" | "control" | "lctrl" | "rctrl" => TriggerKey::Control,
        "alt" | "lalt" | "ralt" => TriggerKey::Alt,
        "super" | "meta" | "win" => TriggerKey::Super,
        _ => TriggerKey::Key(lower),
    }
}

/// [`TriggerKey`] の標準的な表示用キー名を返す。
pub fn canonical_key_name(key: &TriggerKey) -> String {
    match key {
        TriggerKey::Shift => "Shift".to_string(),
        TriggerKey::Control => "Ctrl".to_string(),
        TriggerKey::Alt => "Alt".to_string(),
        TriggerKey::Super => "Super".to_string(),
        TriggerKey::Key(name) => name.clone(),
    }
}

/// キーイベント列からトリガー発火を判定する純粋なステートマシン。
#[derive(Debug, Clone)]
pub struct TriggerDetector {
    spec: TriggerSpec,
    held_keys: HashMap<TriggerKey, u64>,
    press_started_at: Option<u64>,
    tainted: bool,
    tap_streak: u8,
    last_tap_released_at: Option<u64>,
}

impl TriggerDetector {
    /// 指定された [`TriggerSpec`] で新しい検知器を初期化する。
    pub fn new(spec: TriggerSpec) -> Self {
        Self {
            spec,
            held_keys: HashMap::new(),
            press_started_at: None,
            tainted: false,
            tap_streak: 0,
            last_tap_released_at: None,
        }
    }

    /// 検知条件を更新し、進行中のタップ状態をリセットする。
    pub fn update_spec(&mut self, spec: TriggerSpec) {
        self.spec = spec;
        self.held_keys.clear();
        self.press_started_at = None;
        self.tainted = false;
        self.tap_streak = 0;
        self.last_tap_released_at = None;
    }

    /// 単一のキー押下／解放イベントを処理し、トリガー発火時に `true` を返す。
    pub fn on_key_event(&mut self, key: &TriggerKey, pressed: bool, now_ms: u64) -> bool {
        if pressed {
            // 解放イベント欠落で残留した幽霊キーを自動パージして永久デッドロックを防ぐ
            self.held_keys.retain(|_, &mut pressed_at| {
                now_ms.saturating_sub(pressed_at) <= STALE_HELD_KEY_TIMEOUT_MS
            });
            // OS のキーリピートによる重複押下は無視する
            if self.held_keys.contains_key(key) {
                return false;
            }
            self.held_keys.insert(key.clone(), now_ms);
        } else if self.held_keys.remove(key).is_none() {
            return false;
        }

        if !self.spec.modifiers.is_empty() || self.spec.tap_count <= 1 {
            return self.handle_combo_or_single_event(key, pressed);
        }

        self.handle_multi_tap_event(key, pressed, now_ms)
    }

    /// 修飾キー付きコンボおよび単発キーモードの判定を行う。
    fn handle_combo_or_single_event(&self, key: &TriggerKey, pressed: bool) -> bool {
        pressed
            && *key == self.spec.main_key
            && self
                .spec
                .modifiers
                .iter()
                .all(|modifier| self.held_keys.contains_key(modifier))
    }

    /// 単一キーの連続タップモードの判定を行う。
    fn handle_multi_tap_event(&mut self, key: &TriggerKey, pressed: bool, now_ms: u64) -> bool {
        if *key != self.spec.main_key {
            if pressed {
                // 主キー以外の押下が混入した場合はタップ連鎖を無効化する
                self.tainted = true;
                self.tap_streak = 0;
                self.last_tap_released_at = None;
            }
            return false;
        }

        if pressed {
            // 他キー押下中に主キーが押された場合も誤爆防止のため汚染扱いにする
            if self.held_keys.len() > 1 {
                self.tainted = true;
                self.press_started_at = None;
                self.tap_streak = 0;
                self.last_tap_released_at = None;
            } else {
                self.tainted = false;
                self.press_started_at = Some(now_ms);
            }
            return false;
        }

        let held_duration = self
            .press_started_at
            .take()
            .map(|start| now_ms.saturating_sub(start));
        let is_valid_tap = !self.tainted
            && matches!(held_duration, Some(duration) if duration <= MAX_TAP_HOLD_MS);
        self.tainted = false;

        if !is_valid_tap {
            self.tap_streak = 0;
            self.last_tap_released_at = None;
            return false;
        }

        let within_interval = self
            .last_tap_released_at
            .is_some_and(|prev| now_ms.saturating_sub(prev) <= self.spec.interval_ms);
        self.tap_streak = if within_interval {
            self.tap_streak.saturating_add(1)
        } else {
            1
        };
        self.last_tap_released_at = Some(now_ms);

        if self.tap_streak >= self.spec.tap_count {
            self.tap_streak = 0;
            self.last_tap_released_at = None;
            return true;
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shift_double_tap_detector() -> TriggerDetector {
        let cfg = TriggerConfig {
            key: "Shift".to_string(),
            tap_count: 2,
            interval_ms: 350,
        };
        TriggerDetector::new(TriggerSpec::from_config(&cfg))
    }

    #[test]
    fn double_tap_shift_fires_within_interval() {
        let mut detector = shift_double_tap_detector();

        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 0));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 50));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 200));
        assert!(detector.on_key_event(&TriggerKey::Shift, false, 260));

        // 発火直後にカウントがリセットされ、次の単発タップでは再発火しないこと
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 300));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 340));
    }

    #[test]
    fn double_tap_does_not_fire_when_interval_exceeded() {
        let mut detector = shift_double_tap_detector();

        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 0));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 50));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 500));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 550));
    }

    #[test]
    fn typing_capital_letter_does_not_misfire_double_tap_shift() {
        let mut detector = shift_double_tap_detector();
        let key_a = TriggerKey::Key("a".to_string());

        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 0));
        assert!(!detector.on_key_event(&key_a, true, 20));
        assert!(!detector.on_key_event(&key_a, false, 40));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 60));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 120));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 150));
    }

    #[test]
    fn long_hold_cancels_tap() {
        let mut detector = shift_double_tap_detector();

        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 0));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 700));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 750));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 800));
    }

    #[test]
    fn triple_tap_respects_tap_count_3() {
        let cfg = TriggerConfig {
            key: "Shift".to_string(),
            tap_count: 3,
            interval_ms: 350,
        };
        let mut detector = TriggerDetector::new(TriggerSpec::from_config(&cfg));

        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 0));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 40));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 120));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 160));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 240));
        assert!(detector.on_key_event(&TriggerKey::Shift, false, 280));
    }

    #[test]
    fn combo_shortcut_fires_on_main_key_press() {
        let cfg = TriggerConfig {
            key: "Ctrl+Alt+v".to_string(),
            tap_count: 1,
            interval_ms: 350,
        };
        let mut detector = TriggerDetector::new(TriggerSpec::from_config(&cfg));
        let key_v = TriggerKey::Key("v".to_string());

        // 修飾不足（Control のみ + v）では発火しないこと
        assert!(!detector.on_key_event(&TriggerKey::Control, true, 0));
        assert!(!detector.on_key_event(&key_v, true, 20));
        assert!(!detector.on_key_event(&key_v, false, 40));

        // Alt を追加押下して v を押した瞬間に発火すること
        assert!(!detector.on_key_event(&TriggerKey::Alt, true, 60));
        assert!(detector.on_key_event(&key_v, true, 80));
        assert!(!detector.on_key_event(&key_v, false, 100));
    }

    #[test]
    fn key_repeat_press_events_do_not_double_count() {
        let mut detector = shift_double_tap_detector();

        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 0));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 30));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 60));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 90));

        // リピートは無視されるため、1回目の解放時点ではまだ発火しない
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 180));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 210));
        assert!(detector.on_key_event(&TriggerKey::Shift, false, 240));
    }

    #[test]
    fn parse_and_spec_aliases_and_display_work() {
        assert_eq!(parse_key_name("LShift"), TriggerKey::Shift);
        assert_eq!(parse_key_name("rshift"), TriggerKey::Shift);
        assert_eq!(parse_key_name("Control"), TriggerKey::Control);
        assert_eq!(parse_key_name("lctrl"), TriggerKey::Control);
        assert_eq!(parse_key_name("RAlt"), TriggerKey::Alt);
        assert_eq!(parse_key_name("Win"), TriggerKey::Super);
        assert_eq!(parse_key_name("META"), TriggerKey::Super);
        assert_eq!(parse_key_name("F12"), TriggerKey::Key("f12".to_string()));

        let spec = TriggerSpec::from_config(&TriggerConfig {
            key: "Ctrl+control+Alt+V".to_string(),
            tap_count: 1,
            interval_ms: 350,
        });
        assert_eq!(spec.modifiers, vec![TriggerKey::Control, TriggerKey::Alt]);
        assert_eq!(spec.main_key, TriggerKey::Key("v".to_string()));
        assert_eq!(spec.display_string(), "Ctrl+Alt+v");

        let double_spec = TriggerSpec::from_config(&TriggerConfig {
            key: "Shift".to_string(),
            tap_count: 2,
            interval_ms: 350,
        });
        assert_eq!(double_spec.display_string(), "Shift x2");
    }

    #[test]
    fn backward_timestamp_uses_saturating_sub_safely() {
        let mut detector = shift_double_tap_detector();

        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 1_000));
        // 時刻が巻き戻ってもパニックせず安全に処理されること
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 900));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 800));
        assert!(detector.on_key_event(&TriggerKey::Shift, false, 850));
    }

    #[test]
    fn other_key_between_taps_and_update_spec_reset_streak() {
        let mut detector = shift_double_tap_detector();
        let key_a = TriggerKey::Key("a".to_string());

        // Shift タップの間に別キーを挟んだ場合は誤爆しないこと
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 0));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 30));
        assert!(!detector.on_key_event(&key_a, true, 80));
        assert!(!detector.on_key_event(&key_a, false, 100));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 140));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 170));

        // 仕様更新後は進行中タップがリセットされ、新しい単発キー仕様で発火すること
        let single_f12 = TriggerSpec::from_config(&TriggerConfig {
            key: "F12".to_string(),
            tap_count: 1,
            interval_ms: 350,
        });
        detector.update_spec(single_f12);
        assert!(detector.on_key_event(&TriggerKey::Key("f12".to_string()), true, 200));
    }

    #[test]
    fn empty_key_config_falls_back_to_shift() {
        let spec = TriggerSpec::from_config(&TriggerConfig {
            key: "  +  ".to_string(),
            tap_count: 0,
            interval_ms: 350,
        });
        assert!(spec.modifiers.is_empty());
        assert_eq!(spec.main_key, TriggerKey::Shift);
        assert_eq!(spec.tap_count, 1);
        assert_eq!(spec.display_string(), "Shift");
    }

    #[test]
    fn orphaned_held_key_self_heals_after_stale_timeout() {
        let mut detector = shift_double_tap_detector();
        let key_a = TriggerKey::Key("a".to_string());

        // OS が key_a の解放イベントを取りこぼした（幽霊キー残留）ケースを模擬
        assert!(!detector.on_key_event(&key_a, true, 100));
        // さらに Shift 自体の解放イベントも取りこぼしたケースを模擬
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 200));

        // タイムアウト経過後（例: 3000ms 経過した t=3500）に Shift x2 を叩いた際、
        // 残留していた幽霊キーが自動パージされて正常に発火すること
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 3_500));
        assert!(!detector.on_key_event(&TriggerKey::Shift, false, 3_550));
        assert!(!detector.on_key_event(&TriggerKey::Shift, true, 3_700));
        assert!(
            detector.on_key_event(&TriggerKey::Shift, false, 3_750),
            "幽霊キーが自己治癒によりパージされ、Shift x2 が正常発火するべき"
        );
    }
}

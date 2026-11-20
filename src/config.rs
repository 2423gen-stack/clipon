//! ClipOn の設定ファイル (`config.toml`) および OS 自動起動管理モジュール。

use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `tap_count` の許容最小値。
const MIN_TAP_COUNT: u8 = 1;
/// `tap_count` の許容最大値。
const MAX_TAP_COUNT: u8 = 3;
/// `interval_ms` の許容最小値（ミリ秒）。
const MIN_INTERVAL_MS: u64 = 80;
/// `interval_ms` の許容最大値（ミリ秒）。
const MAX_INTERVAL_MS: u64 = 2000;
/// `max_items` の許容最小値。
const MIN_MAX_ITEMS: usize = 1;
/// `max_items` の許容最大値。
const MAX_MAX_ITEMS: usize = 500;

/// デフォルトの呼び出しキー名。
const DEFAULT_TRIGGER_KEY: &str = "Shift";
/// デフォルトの貼り付けキー表記。
const DEFAULT_PASTE_KEY: &str = "Ctrl+v";

/// アプリケーション全体の設定。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// 呼び出しトリガー設定。
    pub trigger: TriggerConfig,
    /// クリップボード履歴の保持設定。
    pub history: HistoryConfig,
    /// 選択後の自動貼り付け設定。
    pub paste: PasteConfig,
    /// ファイルおよび画像の転送モード設定。
    pub media: MediaConfig,
}

/// 呼び出しトリガーのキーおよびタップ判定設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggerConfig {
    /// 呼び出しキー名（例: `"Shift"`, `"Control"`, `"Alt"`）。
    pub key: String,
    /// 判定タップ数（`1..=3`）。
    pub tap_count: u8,
    /// 連続タップ判定の最大間隔（ミリ秒、`80..=2000`）。
    pub interval_ms: u64,
}

/// クリップボード履歴の保持件数設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    /// 保持・表示する最大履歴件数（`1..=500`）。
    pub max_items: usize,
}

/// 選択決定後の貼り付け動作設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PasteConfig {
    /// 選択後に直前ウィンドウへ自動貼り付けを実行するか。
    pub auto_paste: bool,
    /// 貼り付け時に送出するキー表記（例: `"Ctrl+v"`, `"Shift+Insert"`）。
    pub paste_key: String,
}

/// ファイルコピー選択時の転送モード。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileMode {
    /// 絶対パス文字列としてクリップボードへ転送する。
    #[default]
    Path,
    /// ファイル実体（URIリスト等）としてクリップボードへ転送する。
    File,
}

/// 画像コピー選択時の転送モード。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageMode {
    /// 画像バイナリデータのままクリップボードへ転送する。
    #[default]
    Image,
    /// 一時保存した画像ファイルの絶対パス文字列を転送する。
    Path,
}

/// ファイルおよび画像の転送モード設定。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaConfig {
    /// ファイルコピー選択時のモード。
    pub file_mode: FileMode,
    /// 画像コピー選択時のモード。
    pub image_mode: ImageMode,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            key: DEFAULT_TRIGGER_KEY.to_string(),
            tap_count: 2,
            interval_ms: 350,
        }
    }
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self { max_items: 25 }
    }
}

impl Default for PasteConfig {
    fn default() -> Self {
        Self {
            auto_paste: true,
            paste_key: DEFAULT_PASTE_KEY.to_string(),
        }
    }
}

impl AppConfig {
    /// 設定値を安全な範囲・形式へ正規化する。
    pub fn sanitize(&mut self) {
        let trimmed_key = self.trigger.key.trim();
        if trimmed_key.is_empty() {
            self.trigger.key = DEFAULT_TRIGGER_KEY.to_string();
        } else {
            self.trigger.key = trimmed_key.to_string();
        }

        self.trigger.tap_count = self.trigger.tap_count.clamp(MIN_TAP_COUNT, MAX_TAP_COUNT);
        self.trigger.interval_ms = self
            .trigger
            .interval_ms
            .clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);
        self.history.max_items = self.history.max_items.clamp(MIN_MAX_ITEMS, MAX_MAX_ITEMS);

        let trimmed_paste = self.paste.paste_key.trim();
        if trimmed_paste.is_empty() {
            self.paste.paste_key = DEFAULT_PASTE_KEY.to_string();
        } else {
            self.paste.paste_key = trimmed_paste.to_string();
        }
    }

    /// 指定パスから `config.toml` を読み込む。ファイルが存在しない場合はデフォルト設定を返す。
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => {
                return Err(format!(
                    "設定ファイルの読み込みに失敗しました ({}): {err}",
                    path.display()
                ));
            }
        };

        let mut cfg: Self = toml::from_str(&raw).map_err(|err| {
            format!(
                "設定ファイル (TOML) の解析に失敗しました ({}): {err}",
                path.display()
            )
        })?;
        cfg.sanitize();
        Ok(cfg)
    }

    /// 設定を TOML 形式で指定パスへ原子的に保存する。
    pub fn save_to_path(&self, path: &Path) -> Result<(), String> {
        let mut normalized = self.clone();
        normalized.sanitize();
        let serialized = toml::to_string_pretty(&normalized)
            .map_err(|err| format!("設定の TOML シリアライズに失敗しました: {err}"))?;
        atomic_write_file(path, serialized.as_bytes())
    }

    /// デフォルトの設定ファイルパス（`~/.config/clipon/config.toml`）を返す。
    pub fn default_config_path() -> PathBuf {
        xdg_config_dir().join("clipon").join("config.toml")
    }

    /// デフォルトのキャッシュディレクトリパス（`~/.cache/clipon`）を返す。
    pub fn default_cache_dir() -> PathBuf {
        xdg_cache_dir().join("clipon")
    }
}

/// XDG Autostart の `.desktop` ファイル配置パス（`~/.config/autostart/clipon.desktop`）を返す。
pub fn autostart_desktop_path() -> PathBuf {
    xdg_config_dir().join("autostart").join("clipon.desktop")
}

/// 指定パスの自動起動 `.desktop` ファイルが有効状態で存在するか判定する。
pub fn is_autostart_enabled_at(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut gnome_disabled = false;
    let mut hidden = false;
    for line in content.lines().map(str::trim) {
        if line.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false") {
            gnome_disabled = true;
        } else if line.eq_ignore_ascii_case("Hidden=true") {
            hidden = true;
        }
    }
    !gnome_disabled && !hidden
}

/// 指定パスに対して自動起動 `.desktop` ファイルを作成または削除する。
pub fn set_autostart_at(path: &Path, exec_path: &Path, enabled: bool) -> Result<(), String> {
    if enabled {
        let content = render_desktop_entry(exec_path);
        atomic_write_file(path, content.as_bytes())
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!(
                "自動起動ファイルの削除に失敗しました ({}): {err}",
                path.display()
            )),
        }
    }
}

/// 実行ファイルパスから XDG Autostart 用の `.desktop` エントリ文字列を生成する。
pub fn render_desktop_entry(exec_path: &Path) -> String {
    let quoted_exec = quote_desktop_exec_path(exec_path);
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=ClipOn\n\
         Comment=超軽量クリップボードランチャー\n\
         Exec={quoted_exec}\n\
         Terminal=false\n\
         Categories=Utility;\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// ユーザー設定ディレクトリ（通常 `~/.config`）を取得する。
fn xdg_config_dir() -> PathBuf {
    if let Some(base) = BaseDirs::new() {
        return base.config_dir().to_path_buf();
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config");
    }
    PathBuf::from(".config")
}

/// ユーザーキャッシュディレクトリ（通常 `~/.cache`）を取得する。
fn xdg_cache_dir() -> PathBuf {
    if let Some(base) = BaseDirs::new() {
        return base.cache_dir().to_path_buf();
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache");
    }
    PathBuf::from(".cache")
}

/// Desktop Entry の `Exec=` フィールド向けにパス文字列を安全にクォートする。
fn quote_desktop_exec_path(exec_path: &Path) -> String {
    let raw = exec_path.to_string_lossy();
    let needs_quote = raw.is_empty()
        || raw
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '$' | '`' | '%'));
    if !needs_quote {
        return raw.into_owned();
    }

    let mut escaped = String::with_capacity(raw.len() + 2);
    escaped.push('"');
    for ch in raw.chars() {
        match ch {
            '"' | '\\' | '$' | '`' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            '%' => escaped.push_str("%%"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

/// 同一ディレクトリ内の一時ファイルを経由して原子的にファイルを書き込む。
fn atomic_write_file(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|err| {
        format!(
            "親ディレクトリの作成に失敗しました ({}): {err}",
            parent.display()
        )
    })?;

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("clipon");
    let tmp_path = parent.join(format!(".{file_name}.tmp.{}", std::process::id()));

    std::fs::write(&tmp_path, content).map_err(|err| {
        format!(
            "一時ファイルの書き込みに失敗しました ({}): {err}",
            tmp_path.display()
        )
    })?;

    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!(
            "ファイルの原子的置換に失敗しました ({}): {err}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_config_matches_spec() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.trigger.key, "Shift");
        assert_eq!(cfg.trigger.tap_count, 2);
        assert_eq!(cfg.trigger.interval_ms, 350);
        assert_eq!(cfg.history.max_items, 25);
        assert!(cfg.paste.auto_paste);
        assert_eq!(cfg.paste.paste_key, "Ctrl+v");
        assert_eq!(cfg.media.file_mode, FileMode::Path);
        assert_eq!(cfg.media.image_mode, ImageMode::Image);
    }

    #[test]
    fn roundtrip_toml_load_and_save() {
        let dir = TempDir::new().expect("TempDir作成");
        let path = dir.path().join("nested").join("config.toml");

        let missing = AppConfig::load_from_path(&path).expect("未存在時はデフォルトを返すべき");
        assert_eq!(missing, AppConfig::default());

        let mut custom = AppConfig::default();
        custom.trigger.key = "Control".to_string();
        custom.trigger.tap_count = 3;
        custom.trigger.interval_ms = 500;
        custom.history.max_items = 40;
        custom.paste.auto_paste = false;
        custom.paste.paste_key = "Shift+Insert".to_string();
        custom.media.file_mode = FileMode::File;
        custom.media.image_mode = ImageMode::Path;

        custom.save_to_path(&path).expect("保存成功");
        let loaded = AppConfig::load_from_path(&path).expect("再読込成功");
        assert_eq!(loaded, custom);
    }

    #[test]
    fn sanitizes_invalid_bounds_on_load() {
        let dir = TempDir::new().expect("TempDir作成");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[trigger]
key = ""
tap_count = 0
interval_ms = 10

[history]
max_items = 0

[paste]
paste_key = "   "
"#,
        )
        .expect("異常値TOML書き込み");

        let loaded = AppConfig::load_from_path(&path).expect("読込成功");
        assert_eq!(loaded.trigger.key, "Shift");
        assert_eq!(loaded.trigger.tap_count, 1);
        assert_eq!(loaded.trigger.interval_ms, 80);
        assert_eq!(loaded.history.max_items, 1);
        assert_eq!(loaded.paste.paste_key, "Ctrl+v");

        // 上限クランプも確認する
        std::fs::write(
            &path,
            r#"
[trigger]
tap_count = 99
interval_ms = 99999

[history]
max_items = 5000
"#,
        )
        .expect("上限超過TOML書き込み");
        let loaded_upper = AppConfig::load_from_path(&path).expect("読込成功");
        assert_eq!(loaded_upper.trigger.tap_count, 3);
        assert_eq!(loaded_upper.trigger.interval_ms, 2000);
        assert_eq!(loaded_upper.history.max_items, 500);
    }

    #[test]
    fn autostart_enable_and_disable_writes_and_removes_desktop_file() {
        let dir = TempDir::new().expect("TempDir作成");
        let desktop_path = dir.path().join("autostart").join("clipon.desktop");
        let exec_path = Path::new("/opt/my tools/clipon");

        assert!(!is_autostart_enabled_at(&desktop_path));

        set_autostart_at(&desktop_path, exec_path, true).expect("自動起動有効化");
        assert!(is_autostart_enabled_at(&desktop_path));

        let content = std::fs::read_to_string(&desktop_path).expect(".desktop読込");
        assert!(content.contains("[Desktop Entry]"));
        assert!(content.contains("Type=Application"));
        assert!(content.contains("Name=ClipOn"));
        assert!(content.contains("Exec=\"/opt/my tools/clipon\""));
        assert!(content.contains("X-GNOME-Autostart-enabled=true"));

        set_autostart_at(&desktop_path, exec_path, false).expect("自動起動無効化");
        assert!(!is_autostart_enabled_at(&desktop_path));
        assert!(!desktop_path.exists());

        // 既に存在しない状態で無効化しても Ok(()) を返すこと
        set_autostart_at(&desktop_path, exec_path, false).expect("冪等な無効化");
    }
}

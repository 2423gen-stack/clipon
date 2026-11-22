//! クリップボード履歴のリングバッファおよびファイル・画像メディア管理モジュール。

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::config::UiLanguage;

/// FNV-1a 64-bit の初期オフセット基底。
const FNV1A_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit の素数乗数。
const FNV1A_PRIME: u64 = 0x0100_0000_01b3;

/// コンテンツ種別ごとのドメイン分離タグ。
const HASH_TAG_TEXT: u8 = 0x01;
const HASH_TAG_FILE_PATHS: u8 = 0x02;
const HASH_TAG_IMAGE: u8 = 0x03;

/// クリップボード履歴アイテムのコンテンツ種別。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipContent {
    /// プレーンテキスト。
    Text(String),
    /// ローカルファイルパスのリスト。
    FilePaths(Vec<PathBuf>),
    /// PNG エンコード済み画像データと解像度。
    Image {
        /// PNG バイナリデータ。
        png_bytes: Vec<u8>,
        /// 画像の幅（ピクセル）。
        width: u32,
        /// 画像の高さ（ピクセル）。
        height: u32,
    },
}

/// 履歴内の単一エントリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipItem {
    /// 単調増加の一意 ID。
    pub id: u64,
    /// コンテンツの確定的 FNV-1a 64-bit ハッシュ値。
    pub hash: u64,
    /// クリップボードの中身。
    pub content: ClipContent,
}

impl ClipItem {
    /// UI 一覧向けの短縮表示ラベル（既定: 日本語）を生成する。
    pub fn display_label(&self, max_chars: usize) -> String {
        self.display_label_for_lang(max_chars, UiLanguage::Ja)
    }

    /// 指定された表示言語に応じた UI 一覧向けの短縮表示ラベルを生成する。
    pub fn display_label_for_lang(&self, max_chars: usize, lang: UiLanguage) -> String {
        let raw_label = match &self.content {
            ClipContent::Text(text) => text.replace("\r\n", "↵").replace(['\n', '\r'], "↵"),
            ClipContent::FilePaths(paths) => {
                if let Some(first) = paths.first() {
                    let name = first
                        .file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_else(|| first.to_string_lossy());
                    if paths.len() == 1 {
                        format!("📄 {name}")
                    } else {
                        format!("📄 {name} (+{})", paths.len() - 1)
                    }
                } else {
                    match lang {
                        UiLanguage::Ja => "📄 (空)".to_string(),
                        UiLanguage::En => "📄 (empty)".to_string(),
                    }
                }
            }
            ClipContent::Image { width, height, .. } => match lang {
                UiLanguage::Ja => format!("🖼️ [画像 {width}x{height}]"),
                UiLanguage::En => format!("🖼️ [Image {width}x{height}]"),
            },
        };

        truncate_chars(&raw_label, max_chars)
    }

    /// コンテンツ種別ごとのドメインタグ付き確定的 FNV-1a 64-bit ハッシュを計算する。
    pub fn content_hash(content: &ClipContent) -> u64 {
        let mut h = FNV1A_OFFSET_BASIS;
        match content {
            ClipContent::Text(text) => {
                h = fnv1a_update(h, &[HASH_TAG_TEXT]);
                h = fnv1a_update(h, text.as_bytes());
            }
            ClipContent::FilePaths(paths) => {
                h = fnv1a_update(h, &[HASH_TAG_FILE_PATHS]);
                h = fnv1a_update(h, &(paths.len() as u64).to_le_bytes());
                for path in paths {
                    let raw = path.as_os_str().as_bytes();
                    h = fnv1a_update(h, &(raw.len() as u64).to_le_bytes());
                    h = fnv1a_update(h, raw);
                }
            }
            ClipContent::Image {
                png_bytes,
                width,
                height,
            } => {
                h = fnv1a_update(h, &[HASH_TAG_IMAGE]);
                h = fnv1a_update(h, &width.to_le_bytes());
                h = fnv1a_update(h, &height.to_le_bytes());
                h = fnv1a_update(h, png_bytes);
            }
        }
        h
    }
}

/// クリップボード履歴を先頭最新で保持するリングバッファ。
#[derive(Debug, Clone)]
pub struct HistoryStore {
    items: Vec<ClipItem>,
    max_items: usize,
    next_id: u64,
}

impl HistoryStore {
    /// 指定した最大件数で空の履歴ストアを生成する。
    pub fn new(max_items: usize) -> Self {
        Self {
            items: Vec::with_capacity(max_items.min(64)),
            max_items,
            next_id: 1,
        }
    }

    /// 履歴を先頭へ追加する。空データは無視して `false`、重複時は既存要素を先頭へ昇格して `true` を返す。
    pub fn push(&mut self, content: ClipContent) -> bool {
        if self.max_items == 0 || is_empty_content(&content) {
            return false;
        }

        let hash = ClipItem::content_hash(&content);
        if let Some(existing_idx) = self
            .items
            .iter()
            .position(|item| item.hash == hash && item.content == content)
        {
            self.promote_to_front(existing_idx);
            return true;
        }

        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.items.insert(0, ClipItem { id, hash, content });
        self.items.truncate(self.max_items);
        true
    }

    /// 先頭（最新）から末尾（最古）の順で履歴スライスを返す。
    pub fn items(&self) -> &[ClipItem] {
        &self.items
    }

    /// 現在の履歴件数を返す。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 履歴が空かどうかを返す。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 最大履歴件数を変更し、超過分を即座に切り詰める。
    pub fn set_max_items(&mut self, max_items: usize) {
        self.max_items = max_items;
        self.items.truncate(max_items);
    }

    /// 履歴をすべて消去する。
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// 指定インデックスの履歴エントリを取得する。
    pub fn get(&self, index: usize) -> Option<&ClipItem> {
        self.items.get(index)
    }

    /// 指定インデックスの履歴エントリを先頭へ移動し、その参照を返す。
    pub fn promote_to_front(&mut self, index: usize) -> Option<&ClipItem> {
        if index >= self.items.len() {
            return None;
        }
        if index > 0 {
            self.items[..=index].rotate_right(1);
        }
        self.items.first()
    }
}

/// 表示インデックスに対応するショートカット文字（0..9 -> `'1'..='9'`, 9..35 -> `'a'..='z'`）を返す。
pub fn shortcut_char_for_index(index: usize) -> Option<char> {
    match index {
        0..=8 => char::from_u32(('1' as u32) + index as u32),
        9..=34 => char::from_u32(('a' as u32) + (index - 9) as u32),
        _ => None,
    }
}

/// ショートカット文字を履歴インデックスへ逆引きする（大文字小文字を区別しない）。
pub fn index_for_shortcut_char(c: char) -> Option<usize> {
    match c {
        '1'..='9' => Some((c as usize) - ('1' as usize)),
        'a'..='z' => Some(9 + (c as usize) - ('a' as usize)),
        'A'..='Z' => Some(9 + (c as usize) - ('A' as usize)),
        _ => None,
    }
}

/// `text/uri-list` 形式の文字列からローカル `file://` パス一覧をデコードする。
pub fn parse_uri_list(raw: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for line in raw.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(after_scheme) = strip_file_scheme(line) else {
            continue;
        };

        let encoded_path = if after_scheme.starts_with('/') {
            after_scheme
        } else if after_scheme
            .get(..10)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("localhost/"))
        {
            &after_scheme[9..]
        } else {
            continue;
        };

        let decoded_bytes = percent_decode_bytes(encoded_path.as_bytes());
        paths.push(PathBuf::from(OsString::from_vec(decoded_bytes)));
    }
    paths
}

/// パス一覧を `text/uri-list` 形式（`file://` パーセントエンコード + `\r\n` 終端）へ変換する。
pub fn encode_uri_list(paths: &[PathBuf]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for path in paths {
        out.push_str("file://");
        for &b in path.as_os_str().as_bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                let _ = write!(out, "%{b:02X}");
            }
        }
        out.push_str("\r\n");
    }
    out
}

/// 画像キャッシュディレクトリ内に `clipon_img_{hash:016x}.png` を原子的に生成または再利用し、パスを返す。
pub fn ensure_cached_image_path(
    cache_dir: &Path,
    hash: u64,
    png_bytes: &[u8],
) -> Result<PathBuf, String> {
    if png_bytes.is_empty() {
        return Err("画像データが空のためキャッシュを作成できません".to_string());
    }

    std::fs::create_dir_all(cache_dir).map_err(|err| {
        format!(
            "画像キャッシュディレクトリの作成に失敗しました ({}): {err}",
            cache_dir.display()
        )
    })?;

    let target_path = cache_dir.join(format!("clipon_img_{hash:016x}.png"));
    if target_path.exists() {
        return Ok(target_path);
    }

    let tmp_path = cache_dir.join(format!(
        ".clipon_img_{hash:016x}.png.tmp.{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, png_bytes).map_err(|err| {
        format!(
            "画像キャッシュ一時ファイルの書き込みに失敗しました ({}): {err}",
            tmp_path.display()
        )
    })?;

    if let Err(err) = std::fs::rename(&tmp_path, &target_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!(
            "画像キャッシュの原子的配置に失敗しました ({}): {err}",
            target_path.display()
        ));
    }
    Ok(target_path)
}

/// コンテンツが履歴登録対象外の空データであるか判定する。
fn is_empty_content(content: &ClipContent) -> bool {
    match content {
        ClipContent::Text(text) => text.trim().is_empty(),
        ClipContent::FilePaths(paths) => paths.is_empty(),
        ClipContent::Image { png_bytes, .. } => png_bytes.is_empty(),
    }
}

/// FNV-1a 64-bit ハッシュにバイト列を追加する。
fn fnv1a_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV1A_PRIME);
    }
    hash
}

/// UTF-8 文字境界を壊さずに最大 `max_chars` 文字へ切り詰め、超過時は末尾に `…` を付与する。
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if max_chars == 1 {
        let mut chars = text.chars();
        return match (chars.next(), chars.next()) {
            (Some(_), Some(_)) => "…".to_string(),
            _ => text.to_string(),
        };
    }

    let mut cut_byte = text.len();
    for (count, (idx, _)) in text.char_indices().enumerate() {
        if count == max_chars - 1 {
            cut_byte = idx;
        } else if count == max_chars {
            let mut out = String::with_capacity(cut_byte + '…'.len_utf8());
            out.push_str(&text[..cut_byte]);
            out.push('…');
            return out;
        }
    }
    text.to_string()
}

/// URI 先頭の `file://` スキームを大文字小文字を区別せずに取り除く。
fn strip_file_scheme(uri: &str) -> Option<&str> {
    let prefix = uri.get(..7)?;
    if prefix.eq_ignore_ascii_case("file://") {
        Some(&uri[7..])
    } else {
        None
    }
}

/// `%XX` 形式のパーセントエンコードをバイト列へデコードする。
fn percent_decode_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// ASCII 16進文字1桁を 4-bit 数値へ変換する。
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn push_deduplicates_and_promotes_existing_item_to_front() {
        let mut store = HistoryStore::new(10);
        assert!(store.push(ClipContent::Text("A".to_string())));
        assert!(store.push(ClipContent::Text("B".to_string())));
        assert!(store.push(ClipContent::Text("A".to_string())));

        assert_eq!(store.len(), 2);
        assert_eq!(store.items()[0].content, ClipContent::Text("A".to_string()));
        assert_eq!(store.items()[1].content, ClipContent::Text("B".to_string()));
    }

    #[test]
    fn enforces_max_items_ring_buffer_and_resize() {
        let mut store = HistoryStore::new(3);
        store.push(ClipContent::Text("1".to_string()));
        store.push(ClipContent::Text("2".to_string()));
        store.push(ClipContent::Text("3".to_string()));
        store.push(ClipContent::Text("4".to_string()));

        assert_eq!(store.len(), 3);
        assert_eq!(store.items()[0].content, ClipContent::Text("4".to_string()));
        assert_eq!(store.items()[1].content, ClipContent::Text("3".to_string()));
        assert_eq!(store.items()[2].content, ClipContent::Text("2".to_string()));

        store.set_max_items(2);
        assert_eq!(store.len(), 2);
        assert_eq!(store.items()[0].content, ClipContent::Text("4".to_string()));
        assert_eq!(store.items()[1].content, ClipContent::Text("3".to_string()));
    }

    #[test]
    fn ignores_empty_or_whitespace_only_text() {
        let mut store = HistoryStore::new(10);
        assert!(!store.push(ClipContent::Text(String::new())));
        assert!(!store.push(ClipContent::Text("   \t\r\n  ".to_string())));
        assert!(!store.push(ClipContent::FilePaths(Vec::new())));
        assert!(!store.push(ClipContent::Image {
            png_bytes: Vec::new(),
            width: 10,
            height: 10,
        }));
        assert!(store.is_empty());

        assert!(store.push(ClipContent::Text("hello".to_string())));
        assert!(store.push(ClipContent::Text(
            "fn main() {\n    println!(\"ok\");\n}".to_string()
        )));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn parse_and_encode_uri_list_handles_japanese_and_spaces() {
        let raw = "file:///home/gen/my%20file/%E6%97%A5%E6%9C%AC%E8%AA%9E.txt\r\n\
                   #comment\r\n\
                   \r\n\
                   https://example.com/skip\r\n\
                   file://localhost/tmp/test%2B1.txt\r\n";

        let paths = parse_uri_list(raw);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/home/gen/my file/日本語.txt"),
                PathBuf::from("/tmp/test+1.txt"),
            ]
        );

        let encoded = encode_uri_list(&paths);
        assert_eq!(
            encoded,
            "file:///home/gen/my%20file/%E6%97%A5%E6%9C%AC%E8%AA%9E.txt\r\n\
             file:///tmp/test%2B1.txt\r\n"
        );
        assert_eq!(parse_uri_list(&encoded), paths);
    }

    #[test]
    fn display_label_formats_text_files_and_images() {
        let text_item = ClipItem {
            id: 1,
            hash: 1,
            content: ClipContent::Text("1行目\n2行目\r\nあいうえおかきくけこ".to_string()),
        };
        let label = text_item.display_label(10);
        assert!(label.contains('↵'));
        assert!(label.chars().count() <= 10);
        assert_eq!(label, "1行目↵2行目↵あ…");

        let single_file = ClipItem {
            id: 2,
            hash: 2,
            content: ClipContent::FilePaths(vec![PathBuf::from("/home/gen/docs/name.txt")]),
        };
        assert_eq!(single_file.display_label(60), "📄 name.txt");

        let multi_files = ClipItem {
            id: 3,
            hash: 3,
            content: ClipContent::FilePaths(vec![
                PathBuf::from("/home/gen/docs/name.txt"),
                PathBuf::from("/home/gen/docs/other1.txt"),
                PathBuf::from("/home/gen/docs/other2.txt"),
            ]),
        };
        assert_eq!(multi_files.display_label(60), "📄 name.txt (+2)");

        let image_item = ClipItem {
            id: 4,
            hash: 4,
            content: ClipContent::Image {
                png_bytes: vec![0x89, b'P', b'N', b'G'],
                width: 1920,
                height: 1080,
            },
        };
        assert_eq!(image_item.display_label(60), "🖼️ [画像 1920x1080]");
        assert_eq!(
            image_item.display_label_for_lang(60, UiLanguage::En),
            "🖼️ [Image 1920x1080]"
        );
    }

    #[test]
    fn shortcut_mapping_covers_1_to_9_and_a_to_z() {
        assert_eq!(shortcut_char_for_index(0), Some('1'));
        assert_eq!(shortcut_char_for_index(8), Some('9'));
        assert_eq!(shortcut_char_for_index(9), Some('a'));
        assert_eq!(shortcut_char_for_index(34), Some('z'));
        assert_eq!(shortcut_char_for_index(35), None);

        assert_eq!(index_for_shortcut_char('1'), Some(0));
        assert_eq!(index_for_shortcut_char('9'), Some(8));
        assert_eq!(index_for_shortcut_char('a'), Some(9));
        assert_eq!(index_for_shortcut_char('A'), Some(9));
        assert_eq!(index_for_shortcut_char('z'), Some(34));
        assert_eq!(index_for_shortcut_char('Z'), Some(34));
        assert_eq!(index_for_shortcut_char('0'), None);
        assert_eq!(index_for_shortcut_char('!'), None);
    }

    #[test]
    fn ensure_cached_image_path_writes_png_file() {
        let dir = TempDir::new().expect("TempDir作成");
        let cache_dir = dir.path().join("images");
        let png_bytes = b"\x89PNG\r\n\x1a\ndummy".to_vec();
        let hash = ClipItem::content_hash(&ClipContent::Image {
            png_bytes: png_bytes.clone(),
            width: 64,
            height: 64,
        });

        let first =
            ensure_cached_image_path(&cache_dir, hash, &png_bytes).expect("キャッシュ生成成功");
        assert!(first.exists());
        assert_eq!(
            first.file_name().and_then(|n| n.to_str()),
            Some(format!("clipon_img_{hash:016x}.png").as_str())
        );
        assert_eq!(std::fs::read(&first).expect("PNG読込"), png_bytes);

        let second =
            ensure_cached_image_path(&cache_dir, hash, &png_bytes).expect("キャッシュ再利用成功");
        assert_eq!(first, second);
    }
}

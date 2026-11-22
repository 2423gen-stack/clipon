# ClipOn

**Ultra-Lightweight, Zero-Bloat Linux Clipboard Launcher in Pure Rust**  
**純Rust製・贅肉ゼロの超軽量Linuxクリップボードランチャー**

[![Language: Rust](https://img.shields.io/badge/Language-Rust_2021-orange.svg)](https://www.rust-lang.org/)
[![Platform: Linux](https://img.shields.io/badge/Platform-Ubuntu_22.04%2B_%7C_Linux-blue.svg)](#compatibility--os--バージョン対応表)

[English](#english) | [日本語](#日本語)

---

<a id="english"></a>

## English

### Overview

**ClipOn** is a minimalist, high-speed clipboard manager and launcher built for Ubuntu Linux (Wayland + XWayland / X11). Inspired by the effortless workflow of Windows' *Clibor* and designed as a clean alternative to heavy tools like CopyQ, ClipOn focuses strictly on **instant double-tap invocation, single-key selection, and immediate auto-paste**.

- **Zero C-Library Build Dependencies**: Pure Rust stack (`x11rb`, `evdev`, `ksni`, `eframe`/`egui`). Links only against `libc.so.6`, `libm.so.6`, and `libgcc_s.so.1`.
- **Zero Bloat**: No cloud sync, no theme/color customization clutter, no complex tagging or scripting engines.

### Key Features

1. **Double-Tap Key Trigger**: Double-tap `Shift` (or `Ctrl`, `Alt`, `Super`, or any custom key/combo) to summon the history popup in sub-milliseconds. Built-in anti-misfire state machine ignores modifier holds during normal typing (e.g., typing capital letters).
2. **Instant Keyboard Selection**:
   - Navigate with `↑` / `↓` + `Enter`, or
   - Press a single shortcut key (`1`–`9`, `a`–`z`) to select and paste immediately.
3. **Auto-Paste to Active Window**: Uses a persistent `/dev/uinput` kernel virtual keyboard (with `xdotool` fallback) to send `Ctrl+v` (configurable: `Ctrl+Shift+v`, `Shift+Insert`, etc.) directly to your previous window.
4. **Text, Files & Images**:
   - **Text**: Single-line preview with newline markers (`↵`).
   - **Files (`📄 filename`)**: Paste either the absolute file path string (`path` mode) or file URI object (`file` mode).
   - **Images (`🖼️ [Image WxH]`)**: Live thumbnail preview; paste raw PNG image data (`image` mode) or a cached local PNG file path (`path` mode).
5. **Remembers Popup Position**: Move the popup anywhere with `Super + Left Mouse Drag`; ClipOn automatically remembers and persists the position to `config.toml` when closed.
6. **System Tray & Autostart**: Top-bar `StatusNotifierItem` (AppIndicator) tray icon with **Show / Settings / Quit**, plus a 1-click checkbox for Ubuntu login autostart (`~/.config/autostart/clipon.desktop`).
7. **Bilingual GUI (`日本語` / `English`)**: Switch the UI language live from the Settings window (`🌐 日本語 / English`).

---

### Compatibility / OS・バージョン対応表

Actual runtime dynamic library links of the release binary (`ldd clipon`):
- `libc.so.6` (Maximum symbol version required: **`GLIBC_2.35`**)
- `libm.so.6`
- `libgcc_s.so.1`

| Environment / OS | Prebuilt Binary (`x86_64`) | Notes / 補足 |
| :--- | :---: | :--- |
| **Ubuntu 22.04 LTS / Pro (x86_64)** | ◎ **Native** | Primary build target (`GLIBC 2.35`, GNOME 42 Wayland + XWayland). |
| **Ubuntu 24.04 LTS+ (x86_64)** | ○ **Works directly** | Upper-compatible (`GLIBC 2.39+`). AppIndicator, XWayland, `pkexec`, and `uaccess` work out of the box. |
| **Ubuntu 20.04 LTS or older** | △ **Build from source** | Uses `GLIBC 2.31`; run `cargo build --release` on the target machine. |
| **Other Linux (Fedora / Arch / Debian / KDE)** | ○ **Works** | Requires X11/XWayland (`DISPLAY`) and OpenGL (`libGL.so.1`). If a StatusNotifierItem tray watcher is absent, ClipOn logs a warning and continues normally. |
| **ARM64 (Raspberry Pi, etc.)** | △ **Build from source** | Compile natively with `cargo build --release`. |

---

### Environment Variables & System Requirements

ClipOn checks the following environment variables and system interfaces at runtime:

| Variable / Resource | Required? | Purpose & Fallback Behavior |
| :--- | :---: | :--- |
| `DISPLAY` | **Yes** | X11 / XWayland display connection (e.g., `:0`) used by `x11rb` (XFixes event-driven clipboard monitoring) and `eframe` X11 window positioning. |
| `XDG_RUNTIME_DIR` | Recommended | Directory for the single-instance IPC socket (`$XDG_RUNTIME_DIR/clipon.sock`). Falls back to `/tmp/clipon-<uid>.sock` if unset. |
| `DBUS_SESSION_BUS_ADDRESS` | Recommended | Used by `ksni` to register the top-bar system tray icon (`org.kde.StatusNotifierItem`). If unavailable, ClipOn runs without a tray icon. |
| `XDG_CONFIG_HOME` | Optional | Config base path. Defaults to `~/.config` (`~/.config/clipon/config.toml` and `~/.config/autostart/clipon.desktop`). |
| `XDG_CACHE_HOME` | Optional | Image cache base path for `image_mode = "path"`. Defaults to `~/.cache/clipon`. |
| `HOME` | Yes | Fallback base directory when `XDG_*` variables are unset, and user font discovery (`~/.local/share/fonts`). |
| `/dev/input/event*` | Recommended | Used by `evdev` for global double-tap detection across all Wayland/X11 windows. Can be enabled with 1 click in Settings (`pkexec` udev `uaccess` rule). Falls back to X11 keymap polling if inaccessible. |
| `/dev/uinput` | Recommended | Used for kernel-level virtual keyboard auto-paste. Accessible by default on Ubuntu desktop sessions via `uaccess` ACL; falls back to `xdotool` if unavailable. |

---

### Build & Quick Start

#### 1. Build & Install

```bash
git clone https://github.com/2423gen-stack/clipon.git
cd clipon
cargo build --release
mkdir -p ~/.local/bin
cp -f target/release/clipon ~/.local/bin/clipon
```

#### 2. Enable Global Key Monitoring on Wayland (First Time Only)

To allow double-tap detection (`Shift`×2) over native Wayland windows without running ClipOn as root:
- Launch `clipon &`, right-click the top-right tray icon → **Settings (設定)**, and click **"🔑 Enable Global Key Monitoring (1x Password)"**, **OR**
- Run the helper script once in your terminal:
  ```bash
  ./scripts/setup-input-access.sh
  ```

#### 3. CLI Usage

```text
clipon              Start background daemon (or show popup if already running)
clipon --toggle     Show clipboard history popup (starts daemon if not running)
clipon --show       Alias for --toggle
clipon --settings   Open the Settings window
clipon --quit       Cleanly stop the running ClipOn daemon
clipon --help       Print CLI help
```

---

### Changelog — What's New in v1.1.0

- **IPC Socket Hardening (`0600`)**: The Unix domain socket (`$XDG_RUNTIME_DIR/clipon.sock` or `/tmp/clipon-<uid>.sock`) is now strictly locked to owner-only permissions (`0600`) upon binding, preventing unauthorized local users on multi-user systems from sending `QUIT` or UI disruption commands.
- **Private Image Cache & Config Permissions (`0700` / `0600`)**: The image cache directory (`~/.cache/clipon`) is now enforced at `0700` and all cached PNG screenshots and atomic config writes (`config.toml`) are enforced at `0600`, protecting sensitive clipboard screenshots from local information disclosure.
- **State Machine Self-Healing (Orphaned Ghost Key Purge)**: `TriggerDetector` now tracks per-key press timestamps and automatically purges stale held-key states (`> 2000 ms`) if the OS drops a `KeyRelease` event (e.g., during suspend/resume, compositor focus grabs, or USB reconnects), preventing permanent trigger deadlocks.
- **100% Safe Rust Guarantee (`#![forbid(unsafe_code)]`)**: Enforced compile-time prohibition of `unsafe` blocks across the entire library and binary crate.

---

<a id="日本語"></a>

## 日本語

### 概要

**ClipOn** は、Ubuntu 22.04 LTS 以降（Wayland + XWayland / X11）向けに設計された、**「思考のリズムを1ミリも邪魔しない」** 純Rust製の超軽量クリップボードランチャーです。  
Windowsの名作『Clibor』のような直感的な動線をLinuxネイティブで実現し、CopyQ等の重厚長大なツールの煩雑さを排除しています。

### 主な特徴

1. **任意キーの連続叩き（ダブルタップ）で0.001秒ポップアップ**:
   - デフォルトは `Shift` 2回押し（設定画面で「キーを押して設定」により任意のキー・タップ回数・間隔に変更可能）。
   - 通常タイピング中（例：`Shift` を押しながら大文字を入力したとき等）は他キー混入を検知してカウントをリセットするため、**誤爆ゼロ**で動作します。
2. **直感的なキー選択 ＆ 自動貼り付け**:
   - `↑` / `↓` キー ＋ `Enter`、または各行の **`1`〜`9` / `a`〜`z` の単打** で即確定。
   - 確定と同時にクリップボードへセットし、`/dev/uinput` 仮想キーボード（`xdotool` フォールバック付き）で直前のウィンドウへ `Ctrl+v`（変更可能）を自動送出します。
3. **テキスト・ファイル・画像（サムネイル付き）対応**:
   - **ファイル (`📄 filename`)**: 絶対パス文字列（`path`）またはファイル実体URI（`file`）として転送可能。
   - **画像 (`🖼️ [画像 WxH]`)**: ポップアップ内に縮小サムネイルを表示。画像データ貼り付け（`image`）または一時保存したPNGパス文字列（`path`）を切り替え可能。
4. **ポップアップ表示位置の自動記憶**:
   - `Super + マウス左ドラッグ` で履歴窓を見やすい場所へ移動して閉じると、その座標を `config.toml` に自動保存し、次回（PC再起動後も含む）から同じ場所に出現します。
5. **システムトレイ常駐 ＆ OSログイン時自動起動**:
   - Ubuntu トップバー（AppIndicator / StatusNotifierItem）にアイコンが常駐し、右クリックから **「表示」「設定」「終了」** を選択可能。
   - 設定画面のチェックボックス1つで `~/.config/autostart/clipon.desktop` を自動生成・削除します。
6. **日本語 / 英語 UI リアルタイム切り替え**:
   - 設定画面右上の `🌐 日本語 / English` ラジオボタンから、表示言語を即座に切り替えられます。

---

### 環境変数・システム要件チェック一覧

| 環境変数 / リソース | 必須 | 用途および未設定時のフォールバック挙動 |
| :--- | :---: | :--- |
| `DISPLAY` | **必須** | X11 / XWayland 接続先（例: `:0`）。`x11rb`（XFixes イベント駆動クリップボード監視）およびウィンドウ座標制御に使用します。 |
| `XDG_RUNTIME_DIR` | 推奨 | 多重起動防止・CLI通信用のUNIXドメインソケット（`$XDG_RUNTIME_DIR/clipon.sock`）配置先。未設定時は `/tmp/clipon-<uid>.sock` を使用します。 |
| `DBUS_SESSION_BUS_ADDRESS` | 推奨 | トップバーへのトレイアイコン登録（`org.kde.StatusNotifierItem`）に使用します。未接続環境でも警告のみで本体機能は動作します。 |
| `XDG_CONFIG_HOME` | 任意 | 設定保存先基底（既定: `~/.config` → `~/.config/clipon/config.toml`）。 |
| `XDG_CACHE_HOME` | 任意 | 画像パスモード用の一時PNG保存先（既定: `~/.cache/clipon`）。 |
| `HOME` | 必須 | `XDG_*` 未設定時の基底パスおよび日本語フォント探索（`~/.local/share/fonts`）に使用します。 |
| `/dev/input/event*` | 推奨 | Wayland全画面での `Shift`×2 検知（`evdev`）に使用します。設定画面の「🔑 全画面キー監視を有効化」ボタンからワンクリックで権限設定（`uaccess` udevルール）が可能です。 |
| `/dev/uinput` | 推奨 | カーネル仮想キーボードによる自動ペースト送出に使用します（Ubuntuデスクトップ環境では標準ACLでアクセス可能、不可時は `xdotool` へ自動フォールバック）。 |

---

### 設定ファイル仕様 (`~/.config/clipon/config.toml`)

```toml
[trigger]
key = "Shift"       # 呼び出しキー ("Shift", "Control", "Alt", "Super", または任意キー)
tap_count = 2       # タップ回数 (1〜3)
interval_ms = 350   # 連続タップ判定間隔 (80〜2000 ms)

[history]
max_items = 25      # 保持・表示する最大履歴件数 (5〜200)

[paste]
auto_paste = true   # 選択後に直前ウィンドウへ自動貼り付けするか
paste_key = "Ctrl+v" # 送出する貼り付けキー ("Ctrl+v", "Ctrl+Shift+v", "Shift+Insert" 等)

[media]
file_mode = "path"  # ファイル選択時: "path" (絶対パス文字列) | "file" (ファイルURI)
image_mode = "image" # 画像選択時: "image" (画像データ) | "path" (キャッシュPNGパス)

[popup]
pos_x = 640         # 前回閉じたポップアップ X 座標 (未設定時は自動配置)
pos_y = 320         # 前回閉じたポップアップ Y 座標

[ui]
language = "ja"     # UI表示言語: "ja" (日本語) | "en" (English)
```

---

### 更新履歴 (Changelog)

#### v1.1.0 — セキュリティ要塞化 ＆ ステートマシン自己治癒アップデート
- **IPC ソケットの権限制限 (`0600`)**: UNIXドメインソケット（`$XDG_RUNTIME_DIR/clipon.sock` または `/tmp/clipon-<uid>.sock`）のパーミッションをバインド直後に所有者限定 (`0600`) へ厳格化し、マルチユーザー環境における他ユーザーからの不正コマンド送信（`QUIT` や画面表示妨害）をカーネルレベルで遮断。
- **画像キャッシュ ＆ 設定ファイルのプライバシー保護 (`0700` / `0600`)**: 画像キャッシュディレクトリ (`~/.cache/clipon`) を `0700`、一時保存されるPNG画像および原子的設定ファイル書き込み (`config.toml`) を `0600` に統一し、機密性の高いスクリーンショット等のローカル漏洩を防止。
- **トリガー検知ステートマシンの自己治癒機構（幽霊キー自動パージ）**: OSのスリープ復帰・ウィンドウフォーカス切り替え・USBキーボード瞬断等で `KeyRelease` イベントが欠落した際、`held_keys` に残留した幽霊キー（`2,000 ms` 超過）を次のキー押下時に自動パージし、`Shift`×2 が永久不発になるデッドロックを解消。
- **完全 Safe Rust 保証 (`#![forbid(unsafe_code)]`)**: ライブラリおよびバイナリ全体で `unsafe` コードをコンパイルレベルで完全禁止。

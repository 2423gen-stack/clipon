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
2. **Instant Keyboard Selection & `←` / `→` Tab Switching**:
   - Navigate with `↑` / `↓` + `Enter`, or press a single shortcut key (`1`–`9`, `a`–`z`) to select and paste immediately.
   - Switch between **📋 History** and **📌 Pinned (Templates)** instantaneously with the `←` and `→` arrow keys.
3. **📌 Pinned Templates (Persistent Snippets)**: Click the `📌` icon on any history item (text, file paths, or image) to pin it as a permanent template. Pinned items never expire from the ring buffer and are automatically persisted across reboots in `~/.config/clipon/pinned.toml` (`0600`).
4. **Auto-Paste to Active Window**: Uses a persistent `/dev/uinput` kernel virtual keyboard (with `xdotool` fallback) to send `Ctrl+v` (configurable: `Ctrl+Shift+v`, `Shift+Insert`, etc.) directly to your previous window.
5. **Text, Files & Images**:
   - **Text**: Single-line preview with newline markers (`↵`).
   - **Files (`📄 filename`)**: Paste either the absolute file path string (`path` mode) or file URI object (`file` mode).
   - **Images (`🖼️ [Image WxH]`)**: Live thumbnail preview; paste raw PNG image data (`image` mode) or a cached local PNG file path (`path` mode).
6. **Remembers Popup Position**: Move the popup anywhere by dragging the bottom hint bar or using `Super + Left Mouse Drag`; ClipOn automatically remembers and persists the exact client position to `config.toml` when closed.
7. **System Tray, Desktop Launcher & Autostart**: Top-bar `StatusNotifierItem` (AppIndicator) tray icon, GNOME desktop application entry (`clipon.desktop`), plus a 1-click checkbox for Ubuntu login autostart (`~/.config/autostart/clipon.desktop`).
8. **Bilingual GUI (`日本語` / `English`)**: Switch the UI language live from the Settings window (`🌐 日本語 / English`).

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

### Changelog

#### v1.2.0 — 📌 Pinned Templates Tab & `←` / `→` Arrow Tab Switching
- **📌 Pin Any History Item as a Template**: Added a high-contrast `📌` toggle button to each row. Clicking `📌` pins (or unpins) the item into the **📌 Pinned (定型句)** tab so frequently used snippets, paths, or images are permanently protected from rolling out of the history buffer. Fixed button hit-testing separation from row selection so pinning is instant and reliable.
- **Instant `←` / `→` Keyboard Tab Navigation**: Press `→` in the popup to switch to the **📌 Pinned** tab, and press `←` to return to **📋 History** (or click the tab headers directly). All single-key shortcuts (`1`–`9`, `a`–`z`) and `↑` / `↓` + `Enter` auto-paste work identically in both tabs.
- **Atomic Persistence (`pinned.toml`)**: Pinned templates (including cached PNG images) are saved atomically with `0600` owner-only permissions to `~/.config/clipon/pinned.toml` and automatically restored on daemon startup.
- **Desktop Application Launcher Integration**: Added freedesktop `.desktop` entry and multi-resolution PNG / scalable SVG icons (`assets/`), with absolute icon path resolution ensuring launcher icons render reliably on Ubuntu GNOME.

#### v1.1.2 — Popup Window Upward Drift Fix
- **Fixed Popup Window Drifting Upward on Each Open**: Under X11 / XWayland (such as GNOME Mutter), `winit`'s `outer_rect` subtracts cached `_NET_FRAME_EXTENTS` (decorated title-bar height, ~37 px) from `XTranslateCoordinates` even on frameless (`Decorations(false)`) popup windows, whereas `OuterPosition` (`XConfigureWindow`) positions the client area directly. Reading `outer_rect` on close created a negative feedback loop (`y' = y - 37`) that shifted the popup upward on every invocation until it hit the top bar. ClipOn now tracks `inner_rect` (true client root coordinates), enforces a top-panel safety margin (`32 px`), and supports dragging the popup directly via its footer hint bar.

#### v1.1.1 — XKB `ctrl:swapcaps` & Custom Modifier Map Fix
- **Fixed CapsLock Toggling ON & Raw `'v'` Input on Auto-Paste**: On systems with XKB modifier remapping (such as `ctrl:swapcaps` or `ctrl:nocaps`, where physical `KEY_LEFTCTRL` is mapped to `Caps_Lock`), emitting `KEY_LEFTCTRL` via `/dev/uinput` previously caused the compositor to toggle CapsLock ON and type an unmodified `'v'`. `AutoPaster` and `KeyHook` now dynamically query the X11/XWayland modifier map (`GetModifierMapping`) via `ActiveModifierMap` and resolve `Ctrl` to a non-Lock physical keycode (preferring `KEY_RIGHTCTRL` or `KEY_CAPSLOCK`), with stabilized inter-event settle timing (`8 ms`).

#### v1.1.0 — Security Hardening & State Machine Self-Healing

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
2. **直感的なキー選択 ＆ `←` `→` タブ切り替え**:
   - `↑` / `↓` キー ＋ `Enter`、または各行の **`1`〜`9` / `a`〜`z` の単打** で即確定し、`/dev/uinput` 仮想キーボードで直前のウィンドウへ `Ctrl+v`（変更可能）を自動送出します。
   - **`→` キーで「📌 定型句」タブへ移動、`←` キーで「📋 履歴」タブへ瞬時に切り替え**られます。
3. **📌 ピン留め定型句（永続化機能）**:
   - 履歴行の **`📌` マーク**をクリックすると、そのアイテム（テキスト・ファイル・画像すべて対応）を「定型句」として固定できます。
   - 固定された定型句は通常履歴の件数上限で押し流されず、`~/.config/clipon/pinned.toml`（権限 `0600`）へ自動保存されるためPC再起動後も維持されます。
4. **テキスト・ファイル・画像（サムネイル付き）対応**:
   - **ファイル (`📄 filename`)**: 絶対パス文字列（`path`）またはファイル実体URI（`file`）として転送可能。
   - **画像 (`🖼️ [画像 WxH]`)**: ポップアップ内に縮小サムネイルを表示。画像データ貼り付け（`image`）または一時保存したPNGパス文字列（`path`）を切り替え可能。
5. **ポップアップ表示位置の自動記憶**:
   - フッターガイド部分のドラッグまたは `Super + マウス左ドラッグ` で履歴窓を見やすい場所へ移動して閉じると、その座標を `config.toml` に自動保存し、次回（PC再起動後も含む）から同じ場所に出現します。
6. **システムトレイ常駐・デスクトップランチャー登録・OS自動起動**:
   - Ubuntu トップバー（AppIndicator / StatusNotifierItem）にアイコンが常駐し、右クリックから **「表示」「設定」「終了」** を選択可能。
   - 設定画面のチェックボックス1つで `~/.config/autostart/clipon.desktop` を自動生成・削除します。
7. **日本語 / 英語 UI リアルタイム切り替え**:
   - 設定画面右上の `🌐 日本語 / English` ラジオボタンから、表示言語を即座に切り替えられます。

---

### 環境変数・システム要件チェック一覧

| 環境変数 / リソース | 必須 | 用途および未設定時のフォールバック挙動 |
| :--- | :---: | :--- |
| `DISPLAY` | **必須** | X11 / XWayland 接続先（例: `:0`）。`x11rb`（XFixes イベント駆動クリップボード監視）およびウィンドウ座標制御に使用します。 |
| `XDG_RUNTIME_DIR` | 推奨 | 多重起動防止・CLI通信用のUNIXドメインソケット（`$XDG_RUNTIME_DIR/clipon.sock`）配置先。未設定時は `/tmp/clipon-<uid>.sock` を使用します。 |
| `DBUS_SESSION_BUS_ADDRESS` | 推奨 | トップバーへのトレイアイコン登録（`org.kde.StatusNotifierItem`）に使用します。未接続環境でも警告のみで本体機能は動作します。 |
| `XDG_CONFIG_HOME` | 任意 | 設定保存先基底（既定: `~/.config` → `~/.config/clipon/config.toml`, `pinned.toml`）。 |
| `XDG_CACHE_HOME` | 任意 | 画像パスモードおよび定型句画像用の一時PNG保存先（既定: `~/.cache/clipon`）。 |
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

#### v1.2.0 — 📌 ピン留め定型句タブ ＆ `←` `→` キータブ切り替え機能
- **📌 クリックによる定型句固定・解除**: 履歴画面の各行左端に高コントラストな `📌` ボタンを追加。クリックするとそのコピー（テキスト・ファイル・画像）が定型句として固定され（固定中は黄色に点灯）、再クリックで解除できます。行選択のクリック判定エリアとピン留めボタンのヒットテストを分離し、誤タップなく快適・確実に固定化できるように最適化しました。
- **`←` / `→` キーによる「📋 履歴」⇔「📌 定型句」タブ切り替え**: ポップアップ表示中に `→` キーで「📌 定型句」タブへ移動し、`←` キーで「📋 履歴」タブへ戻れる直感的なキーボードナビゲーションを実装（タブのマウスクリックでも切替可能）。定型句タブでも `↑` `↓` + `Enter` や `1`〜`9`, `a`〜`z` 単打による即時自動貼り付けが可能です。
- **定型句の自動永続化 (`pinned.toml`)**: 固定した定型句は `~/.config/clipon/pinned.toml`（所有者限定権限 `0600`）へ原子的に保存され、ClipOn や OS を再起動しても消えずに保持されます。
- **デスクトップアプリランチャー連携 ＆ アイコン表示**: Ubuntu / GNOME アプリケーションランチャー用の `.desktop` ファイルおよびマルチ解像度 PNG（16x16〜512x512）/ SVG アイコンを同梱。GNOME環境で確実にアイコンが表示されるよう、絶対パス参照とアイコン配置を整備しました。

#### v1.1.2 — ポップアップ表示位置の上方ドリフト修正
- **履歴ウィンドウを開くたびに表示位置が画面上部へずれていく不具合を修正**: X11 / XWayland（GNOME Mutter 等）環境において、フレームレス（`Decorations(false)`）ウィンドウであっても `winit` の `outer_rect` がキャッシュ済みの `_NET_FRAME_EXTENTS`（タイトルバー高さ約 37px）を減算して返す一方、`OuterPosition`（`XConfigureWindow`）はクライアント座標へ直接配置するため、開閉のたびに Y 座標が約 37px ずつ上方へせり上がる負のフィードバックループが発生していました。座標の追跡・永続化に実クライアント座標（`inner_rect`）を使用するよう修正し、上端パネル安全マージン（`32 px`）の適用およびフッターガイドのドラッグ移動対応を追加しました。

#### v1.1.1 — XKB `ctrl:swapcaps`（Ctrl / CapsLock 入れ替え環境）対応修正
- **自動ペースト時に CapsLock が ON になり生文字 `'v'` が入力される不具合を修正**: OS側で `ctrl:swapcaps` 等の XKB キーマップオプションが有効な環境では、物理スキャンコード `KEY_LEFTCTRL` (evdev 29) が OS のコンポジタによって `Caps_Lock` に変換されるため、`/dev/uinput` からの自動ペースト時に CapsLock が点灯し、修飾なしの `'v'` が打たれる問題がありました。`ActiveModifierMap` により X11/XWayland の実モディファイアマッピング (`GetModifierMapping`) を動的照会し、`Caps_Lock` (`Lock` 修飾) に割り当てられていない有効な物理キー（`KEY_RIGHTCTRL` または `KEY_CAPSLOCK`）へ自動解決するよう修正しました（併せて修飾キー押下〜主キー押下間の同期ディレイを `8 ms` へ最適化）。

#### v1.1.0 — セキュリティ要塞化 ＆ ステートマシン自己治癒アップデート
- **IPC ソケットの権限制限 (`0600`)**: UNIXドメインソケット（`$XDG_RUNTIME_DIR/clipon.sock` または `/tmp/clipon-<uid>.sock`）のパーミッションをバインド直後に所有者限定 (`0600`) へ厳格化し、マルチユーザー環境における他ユーザーからの不正コマンド送信（`QUIT` や画面表示妨害）をカーネルレベルで遮断。
- **画像キャッシュ ＆ 設定ファイルのプライバシー保護 (`0700` / `0600`)**: 画像キャッシュディレクトリ (`~/.cache/clipon`) を `0700`、一時保存されるPNG画像および原子的設定ファイル書き込み (`config.toml`) を `0600` に統一し、機密性の高いスクリーンショット等のローカル漏洩を防止。
- **トリガー検知ステートマシンの自己治癒機構（幽霊キー自動パージ）**: OSのスリープ復帰・ウィンドウフォーカス切り替え・USBキーボード瞬断等で `KeyRelease` イベントが欠落した際、`held_keys` に残留した幽霊キー（`2,000 ms` 超過）を次のキー押下時に自動パージし、`Shift`×2 が永久不発になるデッドロックを解消。
- **完全 Safe Rust 保証 (`#![forbid(unsafe_code)]`)**: ライブラリおよびバイナリ全体で `unsafe` コードをコンパイルレベルで完全禁止。

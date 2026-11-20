//! StatusNotifierItem (`ksni`) によるシステムトレイ常駐モジュール。

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, Tray};
use std::sync::Arc;

/// 生成するトレイアイコンの一辺サイズ（ピクセル）。
const ICON_SIZE: i32 = 32;

/// システムトレイのクリックまたはメニュー選択で発火するアクション。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// ポップアップウィンドウを表示する。
    ShowPopup,
    /// 設定ウィンドウを表示する。
    ShowSettings,
    /// アプリケーションを終了する。
    Quit,
}

/// 32x32 の視認性の高いクリップボードアイコンを ARGB32 ネットワークバイトオーダーで生成する。
pub fn build_tray_icon_argb32() -> Icon {
    let size = ICON_SIZE as usize;
    let mut data = vec![0u8; size * size * 4];

    // ダーク背景・ライト背景の双方で視認できる高コントラスト配色 (A, R, G, B)
    let outline = [255, 15, 23, 42];
    let board_fill = [255, 56, 189, 248];
    let paper_fill = [255, 248, 250, 252];
    let clip_metal = [255, 226, 232, 240];
    let text_line = [255, 51, 65, 85];

    let mut set_px = |x: usize, y: usize, argb: [u8; 4]| {
        if x < size && y < size {
            let offset = (y * size + x) * 4;
            data[offset..offset + 4].copy_from_slice(&argb);
        }
    };

    // クリップボード本体（角を 1px 落とした高視認性シェイプ）
    for y in 5..30 {
        for x in 5..27 {
            let is_corner = (x == 5 || x == 26) && (y == 5 || y == 29);
            if is_corner {
                continue;
            }
            let is_border = x == 5 || x == 26 || y == 5 || y == 29;
            if is_border {
                set_px(x, y, outline);
            } else {
                set_px(x, y, board_fill);
            }
        }
    }

    // 内側の用紙領域
    for y in 8..27 {
        for x in 8..24 {
            let is_border = x == 8 || x == 23 || y == 8 || y == 26;
            set_px(x, y, if is_border { outline } else { paper_fill });
        }
    }

    // 用紙上のテキスト行アイコン表現
    for &(row, x_end) in &[(13, 21), (17, 21), (21, 18)] {
        for x in 11..x_end {
            set_px(x, row, text_line);
            set_px(x, row + 1, text_line);
        }
    }

    // 上部の金属クリップ金具
    for y in 2..9 {
        for x in 11..21 {
            let is_corner = (x == 11 || x == 20) && y == 2;
            if is_corner {
                continue;
            }
            let is_border = x == 11 || x == 20 || y == 2 || y == 8;
            set_px(x, y, if is_border { outline } else { clip_metal });
        }
    }
    // クリップ中央のリング穴
    set_px(15, 4, outline);
    set_px(16, 4, outline);

    Icon {
        width: ICON_SIZE,
        height: ICON_SIZE,
        data,
    }
}

/// `ksni::Tray` トレイトを実装する ClipOn トレイ本体。
struct ClipOnTray {
    on_action: Arc<dyn Fn(TrayAction) + Send + Sync + 'static>,
}

impl Tray for ClipOnTray {
    fn id(&self) -> String {
        "clipon".to_string()
    }

    fn title(&self) -> String {
        "ClipOn".to_string()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![build_tray_icon_argb32()]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        (self.on_action)(TrayAction::ShowPopup);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let on_show = Arc::clone(&self.on_action);
        let on_settings = Arc::clone(&self.on_action);
        let on_quit = Arc::clone(&self.on_action);

        vec![
            MenuItem::Standard(StandardItem {
                label: "表示 (Show)".to_string(),
                activate: Box::new(move |_| on_show(TrayAction::ShowPopup)),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: "設定 (Settings)".to_string(),
                activate: Box::new(move |_| on_settings(TrayAction::ShowSettings)),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: "終了 (Quit)".to_string(),
                icon_name: "application-exit".to_string(),
                activate: Box::new(move |_| on_quit(TrayAction::Quit)),
                ..Default::default()
            }),
        ]
    }
}

/// バックグラウンドで動作するシステムトレイの制御ハンドル。
pub struct TrayHandle {
    handle: Handle<ClipOnTray>,
}

impl TrayHandle {
    /// システムトレイを生成してバックグラウンドスレッドで起動する。
    pub fn spawn(
        on_action: impl Fn(TrayAction) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let tray = ClipOnTray {
            on_action: Arc::new(on_action),
        };
        let handle = tray
            .spawn()
            .map_err(|err| format!("システムトレイの起動に失敗しました: {err}"))?;
        Ok(Self { handle })
    }

    /// システムトレイを安全に停止してリソースを解放する。
    pub fn shutdown(self) {
        self.handle.shutdown().wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn build_tray_icon_produces_valid_32x32_argb32_pixmap() {
        let icon = build_tray_icon_argb32();
        assert_eq!(icon.width, 32);
        assert_eq!(icon.height, 32);
        assert_eq!(icon.data.len(), 32 * 32 * 4);
        let has_visible_pixel = icon.data.chunks_exact(4).any(|px| px[0] > 0);
        assert!(has_visible_pixel);
    }

    #[test]
    fn tray_menu_contains_only_show_settings_quit() {
        let tray = ClipOnTray {
            on_action: Arc::new(|_| {}),
        };
        let items = tray.menu();
        assert_eq!(items.len(), 4);

        match &items[0] {
            MenuItem::Standard(item) => assert_eq!(item.label, "表示 (Show)"),
            other => panic!("1番目は StandardItem であるべきです: {other:?}"),
        }
        match &items[1] {
            MenuItem::Standard(item) => assert_eq!(item.label, "設定 (Settings)"),
            other => panic!("2番目は StandardItem であるべきです: {other:?}"),
        }
        assert!(matches!(items[2], MenuItem::Separator));
        match &items[3] {
            MenuItem::Standard(item) => {
                assert_eq!(item.label, "終了 (Quit)");
                assert_eq!(item.icon_name, "application-exit");
            }
            other => panic!("4番目は StandardItem であるべきです: {other:?}"),
        }
    }

    #[test]
    fn tray_activate_and_menu_callbacks_dispatch_expected_actions() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&recorded);
        let mut tray = ClipOnTray {
            on_action: Arc::new(move |action| {
                sink.lock().unwrap().push(action);
            }),
        };

        tray.activate(0, 0);
        let mut items = tray.menu();
        for item in items.drain(..) {
            if let MenuItem::Standard(std_item) = item {
                (std_item.activate)(&mut tray);
            }
        }

        let actions = recorded.lock().unwrap().clone();
        assert_eq!(
            actions,
            vec![
                TrayAction::ShowPopup,
                TrayAction::ShowPopup,
                TrayAction::ShowSettings,
                TrayAction::Quit,
            ]
        );
    }
}

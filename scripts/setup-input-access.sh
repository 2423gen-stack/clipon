#!/usr/bin/env bash
# ClipOn グローバルキー監視 (/dev/input/event*) および自動ペースト (/dev/uinput) の権限設定補助スクリプト

set -euo pipefail

UDEV_RULE_PATH="/etc/udev/rules.d/99-clipon-input.rules"
TARGET_USER="${SUDO_USER:-$USER}"

usage() {
  cat <<'EOF'
ClipOn 入力デバイス権限設定スクリプト

使い方:
  ./scripts/setup-input-access.sh [--group | --udev | --all]

オプション:
  --group   現在のユーザーを input グループへ追加します（デフォルト）
  --udev    再起動不要で ACL を適用する udev ルールを配置してリロードします
  --all     input グループ追加と udev ルール配置の両方を実行します
  -h, --help  このヘルプを表示します
EOF
}

add_input_group() {
  echo ">>> ユーザー '${TARGET_USER}' を input グループへ追加します..."
  sudo usermod -aG input "${TARGET_USER}"
  echo "完了: 再ログイン（または 'newgrp input'）後に /dev/input/event* へアクセス可能になります。"
}

install_udev_rule() {
  echo ">>> udev ルールを ${UDEV_RULE_PATH} に作成します..."
  sudo tee "${UDEV_RULE_PATH}" >/dev/null <<'EOF'
# ClipOn: ログイン中ユーザーへ /dev/input/event* および /dev/uinput のアクセス権を付与
KERNEL=="event*", SUBSYSTEM=="input", MODE="0660", GROUP="input", TAG+="uaccess"
KERNEL=="uinput", MODE="0660", GROUP="input", TAG+="uaccess"
EOF
  echo ">>> udev ルールを再読み込みして即時適用します..."
  sudo udevadm control --reload-rules
  sudo udevadm trigger --subsystem-match=input --subsystem-match=misc
  echo "完了: 再起動不要でデバイス権限が適用されました。"
}

MODE="${1:---group}"

case "${MODE}" in
  --group)
    add_input_group
    ;;
  --udev)
    install_udev_rule
    ;;
  --all)
    add_input_group
    install_udev_rule
    ;;
  -h|--help)
    usage
    ;;
  *)
    echo "エラー: 不明なオプション '${MODE}'" >&2
    usage >&2
    exit 1
    ;;
esac

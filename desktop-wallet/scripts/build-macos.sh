#!/bin/zsh
# QLight Wallet · macOS 打包脚本
# 产物：build/QLight Wallet.app + build/QLight-Wallet.dmg
# 依赖：仅 cargo（编译）+ 系统自带 codesign / hdiutil —— 不需要 npm、不需要 tauri-cli
set -e
cd "$(dirname "$0")/.."

APP_NAME="QLight Wallet"
BUNDLE_ID="net.qlight.wallet"
APP_DIR="build/QLight.app"
DMG="build/QLight-Wallet.dmg"

echo "① cargo build --release ..."
export PATH="$HOME/.cargo/bin:$PATH"
# 外置盘会生成 ._ 资源叉文件，会破坏 tauri 的 build script —— 编译输出固定放内置盘
export CARGO_TARGET_DIR=/Users/a666/qlx-build-target
cargo build --release

echo "② 组装 .app bundle ..."
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "$CARGO_TARGET_DIR/release/qlight-wallet" "$APP_DIR/Contents/MacOS/qlight-wallet"
cp icons/icon.icns "$APP_DIR/Contents/Resources/icon.icns"
cp ../logo/draft/icon-dark.svg /dev/null 2>/dev/null || true

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key><string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleExecutable</key><string>qlight-wallet</string>
    <key>CFBundleIconFile</key><string>icon</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
    <key>LSMinimumSystemVersion</key><string>10.15</string>
</dict>
</plist>
PLIST

echo "③ ad-hoc 签名 ..."
find "$APP_DIR" -name '._*' -delete 2>/dev/null
find "$APP_DIR" -name '.DS_Store' -delete 2>/dev/null
codesign --force --sign - "$APP_DIR" 2>/dev/null || echo "（未签名，仅本机使用）"

echo "④ 生成 DMG（经 /tmp 中转——外置盘的路径会被 hdiutil 拒绝）..."
STAGING="/tmp/qlight-dmg-staging"
rm -rf "$STAGING" "/tmp/QLight-Wallet.dmg"
mkdir -p "$STAGING"
cp -RL "$APP_DIR" "$STAGING/QLight.app"
hdiutil create -volname "${APP_NAME}" -srcfolder "$STAGING/QLight.app" -format UDZO -o "/tmp/QLight-Wallet.dmg"
mv "/tmp/QLight-Wallet.dmg" "$DMG"

echo ""
echo "✅ 完成："
ls -lh "$DMG"

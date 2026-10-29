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

# 版本唯一来源 = Cargo.toml（写死在脚本里必然和包体不一致）
VER="$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)"
test -n "$VER" || { echo "读不到 Cargo.toml 的 version"; exit 1; }
echo "版本 v${VER}"

# frontendDist 就是整个 ui/ 目录：编译期会把里面每一个文件原样嵌进二进制。
# 对拍用的假 __TAURI__ 页一旦留在 ui/ 里，就会跟着发行包一起发出去——那是包里躺着一个假钱包。
LEAK="$(find ui -name '__*' -print -quit)"
test -z "$LEAK" || { echo "ui/ 里有对拍临时文件，它会一起进包：$LEAK（挪到 dev/ 再打）"; exit 1; }

echo "① cargo build --release ..."
export PATH="$HOME/.cargo/bin:$PATH"
# 外置盘会生成 ._ 资源叉文件，会破坏 tauri 的 build script —— 编译输出默认放内置盘，
# 但允许用 CARGO_TARGET_DIR 覆盖（CI / 已有缓存目录都能复用）。
export COPYFILE_DISABLE=1
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/Library/Caches/qlight-build-target}"
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
    <key>CFBundleVersion</key><string>${VER}</string>
    <key>CFBundleShortVersionString</key><string>${VER}</string>
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

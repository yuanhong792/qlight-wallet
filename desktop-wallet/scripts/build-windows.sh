#!/bin/zsh
# QLight Wallet · Windows 交叉打包脚本（在 macOS 上出绿色版，不需要 Windows 机器）
# 产物：build/QLight-Wallet-Windows/（exe + WebView2Loader.dll + libunwind.dll + README.txt）
#       build/QLight-Wallet-Windows.zip
# 依赖：llvm-mingw（$MINGW_HOME 或 ~/qlx-tools/llvm-mingw-*）+ rustup 的 x86_64-pc-windows-gnullvm std
#
# 为什么必须是 gnullvm 而不是 *-gnu：见 .cargo/config.toml —— 普通 gnu 目标的预编译 std 链
# -lgcc / -lgcc_eh，llvm-mingw 是 LLVM 实现不提供 libgcc，链接必然失败。
set -e
setopt NULL_GLOB
cd "$(dirname "$0")/.."

TARGET=x86_64-pc-windows-gnullvm
HOST_TRIPLE=x86_64-w64-mingw32
BUNDLE_NAME=QLight-Wallet-Windows
BUNDLE_DIR="build/$BUNDLE_NAME"
ZIP="build/$BUNDLE_NAME.zip"
CARGO_HOME_DIR=${CARGO_HOME:-$HOME/.cargo}
# 编译输出与打包暂存都必须离开外置盘：exFAT 生成的 ._ 资源叉既破坏构建也会污染 zip
TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/qlx-build-target-win}
STAGING=/tmp/qlx-win-build

VER="$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)"
test -n "$VER" || { echo "读不到 Cargo.toml 的 version"; exit 1; }
echo "版本 v${VER} · 目标 $TARGET"

MINGW_HOME=${MINGW_HOME:-$(echo "$HOME"/qlx-tools/llvm-mingw-*(N) /opt/homebrew/opt/llvm-mingw(N) /usr/local/opt/llvm-mingw(N) | awk '{print $1}')}
test -n "$MINGW_HOME" -a -d "$MINGW_HOME" \
  || { echo "找不到 llvm-mingw：设 MINGW_HOME=/path/to/llvm-mingw 再跑"; exit 1; }

# embed-resource 只认 `windres` 这个命令名，llvm-mingw 提供的是 llvm-windres / <triple>-windres
rm -rf "$STAGING"
mkdir -p "$STAGING/shim"
ln -s "$MINGW_HOME/bin/${HOST_TRIPLE}-windres" "$STAGING/shim/windres"
export PATH="$STAGING/shim:$MINGW_HOME/bin:$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="$TARGET_DIR"

echo "① cargo build --release --target $TARGET ..."
cargo build --release --target "$TARGET"

EXE="$TARGET_DIR/$TARGET/release/qlight-wallet.exe"
test -f "$EXE" || { echo "找不到 $EXE"; exit 1; }

echo "② 收集运行时 DLL ..."
WV2=${WV2_DLL:-$(echo "$CARGO_HOME_DIR"/registry/src/*/webview2-com-sys-*/x64/WebView2Loader.dll | awk '{print $1}')}
test -n "$WV2" -a -f "$WV2" \
  || { echo "找不到 WebView2Loader.dll（webview2-com-sys 随依赖下载）：设 WV2_DLL=/path 再跑"; exit 1; }
UNWIND="$MINGW_HOME/$HOST_TRIPLE/bin/libunwind.dll"
test -f "$UNWIND" || { echo "找不到 $UNWIND"; exit 1; }

echo "③ 自检导入表：除系统 DLL 外只允许 WebView2Loader.dll 与 libunwind.dll ..."
# 「三文件同目录」不是传说，是 PE 导入表的可验断言；冒出第三个非系统依赖就得改包
${HOST_TRIPLE}-objdump -p "$EXE" | awk '/DLL Name/{print $3}' | sort -u > "$STAGING/imports.txt"
EXTRA=$(grep -viE '^(WebView2Loader\.dll|libunwind\.dll|KERNEL32\.dll|NTDLL\.dll|advapi32\.dll|api-ms-win.*|bcrypt\.dll|bcryptprimitives\.dll|combase\.dll|comctl32\.dll|dwmapi\.dll|gdi32\.dll|imm32\.dll|iphlpapi\.dll|ole32\.dll|oleaut32\.dll|shell32\.dll|shlwapi\.dll|user32\.dll|ws2_32\.dll)$' "$STAGING/imports.txt" || true)
test -z "$EXTRA" || { echo "意外的非系统依赖：$EXTRA"; exit 1 }

echo "④ 组装绿色版目录（经 /tmp 中转，避开外置盘的 ._ 资源叉）..."
mkdir -p "$STAGING/$BUNDLE_NAME"
cp "$EXE" "$WV2" "$UNWIND" "$STAGING/$BUNDLE_NAME/"
sed "s/{VERSION}/$VER/g" scripts/windows-bundle-README.txt > "$STAGING/$BUNDLE_NAME/README.txt"
find "$STAGING" -name '._*' -delete
(cd "$STAGING" && rm -f "$BUNDLE_NAME.zip" && zip -X -r -q "$BUNDLE_NAME.zip" "$BUNDLE_NAME")

mkdir -p build
rm -rf "$BUNDLE_DIR"
cp -R "$STAGING/$BUNDLE_NAME" "$BUNDLE_DIR"
cp "$STAGING/$BUNDLE_NAME.zip" "$ZIP"

echo ""
echo "✅ 完成："
ls -lh "$ZIP" "$BUNDLE_DIR"
echo "导入表非系统依赖：$(grep -ciE '^(WebView2Loader|libunwind)' "$STAGING/imports.txt") / 全部 $(wc -l < "$STAGING/imports.txt" | tr -d ' ') 个 DLL"
shasum -a 256 "$ZIP" | awk '{print "  sha256 " substr($1,1,16) "… " FILENAME; exit}'

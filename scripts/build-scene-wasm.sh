#!/usr/bin/env bash
# 主题编辑器的候选窗预览（wind-scene-wasm）：release 编 wasm32 → wasm-bindgen --target web →
# （装了才跑）wasm-opt -Oz，打印体积。设计见 docs/design/theme-editor-wasm.md。
#
# 用法:
#   ./scripts/build-scene-wasm.sh [输出目录]
#     输出目录缺省 $CARGO_TARGET_DIR/scene-wasm（未设则 wind_input/target/scene-wasm）。
#     产物: preview.js（推荐入口：出错自动重建）+ wind_scene.js + wind_scene_bg.wasm + *.d.ts +
#           snippets/（text.js）+ VERSION（源提交号）。
#
# 依赖:
#   - rustup target add wasm32-unknown-unknown
#   - wasm-bindgen CLI，版本须与 wind_input/Cargo.lock 里的 wasm-bindgen 完全一致（不一致时
#     产物与库对不上，运行期才报错）。按 $WASM_BINDGEN → PATH → ~/.cache/wi-tools/bin 找；
#     装法: cargo install wasm-bindgen-cli --version <锁定版本> --root ~/.cache/wi-tools
#   - wasm-opt（binaryen）可选，没有就跳过。
#   - node 可选：有就跑一遍冒烟（crates/wind-scene-wasm/js/smoke.mjs，假 OffscreenCanvas），
#     确认产物能加载、API 接得上。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WS="$REPO_ROOT/wind_input"
TARGET_DIR="${CARGO_TARGET_DIR:-$WS/target}"
OUT="${1:-$TARGET_DIR/scene-wasm}"
PROFILE=wasm-release

die() { printf '\033[31m%s\033[0m\n' "$*" >&2; exit 1; }
say() { printf '\033[32m%s\033[0m\n' "$*"; }

rustup target list --installed 2>/dev/null | grep -qx wasm32-unknown-unknown \
    || die "未装 wasm32 目标：rustup target add wasm32-unknown-unknown"

# Cargo.lock 里 wasm-bindgen 的版本（[[package]] name 行的下一行）。
LOCKED="$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' "$WS/Cargo.lock")"
[ -n "$LOCKED" ] || die "Cargo.lock 里没有 wasm-bindgen（先 cargo check -p wind-scene-wasm --target wasm32-unknown-unknown）"

BINDGEN="${WASM_BINDGEN:-}"
if [ -z "$BINDGEN" ]; then
    if command -v wasm-bindgen >/dev/null 2>&1; then
        BINDGEN="$(command -v wasm-bindgen)"
    elif [ -x "$HOME/.cache/wi-tools/bin/wasm-bindgen" ]; then
        BINDGEN="$HOME/.cache/wi-tools/bin/wasm-bindgen"
    else
        die "找不到 wasm-bindgen：cargo install wasm-bindgen-cli --version $LOCKED --root ~/.cache/wi-tools"
    fi
fi
HAVE="$("$BINDGEN" --version | awk '{ print $2 }')"
[ "$HAVE" = "$LOCKED" ] || die "wasm-bindgen CLI 版本 $HAVE（$BINDGEN）≠ Cargo.lock 的 $LOCKED：
  cargo install wasm-bindgen-cli --version $LOCKED --root ~/.cache/wi-tools"

say "cargo build --profile $PROFILE --target wasm32-unknown-unknown -p wind-scene-wasm"
(cd "$WS" && cargo build --profile "$PROFILE" --target wasm32-unknown-unknown -p wind-scene-wasm)
RAW="$TARGET_DIR/wasm32-unknown-unknown/$PROFILE/wind_scene_wasm.wasm"
[ -f "$RAW" ] || die "没有产物：$RAW"

# 只清本脚本的产物：输出目录可能是编辑器仓里检入的目录，别的文件不动。
mkdir -p "$OUT"
rm -rf "$OUT"/wind_scene.js "$OUT"/wind_scene.d.ts "$OUT"/wind_scene_bg.wasm \
    "$OUT"/wind_scene_bg.wasm.d.ts "$OUT"/snippets "$OUT"/VERSION "$OUT"/preview.js
say "wasm-bindgen $HAVE --target web → $OUT"
"$BINDGEN" --target web --out-dir "$OUT" --out-name wind_scene "$RAW"
WASM="$OUT/wind_scene_bg.wasm"
cp "$WS/crates/wind-scene-wasm/js/preview.js" "$OUT/preview.js"

if command -v wasm-opt >/dev/null 2>&1; then
    say "wasm-opt -Oz"
    # Rust 的 wasm32 目标默认开着这些特性，wasm-opt 不显式放行会拒绝读入。
    wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
        --enable-mutable-globals --enable-reference-types --enable-multivalue \
        -o "$WASM.opt" "$WASM"
    mv "$WASM.opt" "$WASM"
else
    printf '未装 wasm-opt（binaryen），跳过 -Oz\n'
fi

(cd "$REPO_ROOT" && git describe --always --dirty --abbrev=12) > "$OUT/VERSION"

size_of() { wc -c < "$1" | tr -d ' '; }
gz_of() { gzip -9 -c "$1" | wc -c | tr -d ' '; }
say "体积（字节，gzip -9）："
for f in "$WASM" "$OUT/wind_scene.js" "$OUT/preview.js" "$OUT"/snippets/*/js/text.js; do
    printf '  %-28s %10s  %10s\n' "${f#"$OUT"/}" "$(size_of "$f")" "$(gz_of "$f")"
done
printf '  来源 %s\n' "$(cat "$OUT/VERSION")"

if command -v node >/dev/null 2>&1; then
    say "冒烟（node）"
    node "$WS/crates/wind-scene-wasm/js/smoke.mjs" "$OUT"
else
    printf '未装 node，跳过冒烟\n'
fi

#!/usr/bin/env bash
# build_macos.sh — macOS 端构建脚本, 镜像 build_all.ps1 的核心子集.
#
# 与 Win 版的差异 (PR-A 阶段):
#   - 不构建 wind_tsf DLL          (macOS 用 IMKit `.app`, 走 PR-A.1)
#   - 不构建 wind_setting (Wails)  (Wails darwin 需 Apple Developer 证书, 延后)
#   - 不构建 wind_portable         (.NET launcher Win-only)
#   - 只构建 Go 服务 + 词库数据准备
#
# 用法:
#   scripts_mac/build/build.sh             # 全量: 下载词库 + 构建服务 + 准备 data
#   scripts_mac/build/build.sh service     # 仅构建 Go 服务
#   scripts_mac/build/build.sh data        # 仅下载词库 + 准备 data
#   scripts_mac/build/build.sh --debug     # debug variant (build_debug/, _debug 后缀)
#   scripts_mac/build/build.sh clean       # 清 build/ 与 build_debug/
#
# 输出 (release): build/{wind_input,data/}
# 输出 (debug)  : build_debug/{wind_input_debug,data/}
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)
CACHE_DIR="$REPO_DIR/.cache"

# -------- 参数解析 --------
TARGETS=()
DEBUG_VARIANT=0
# universal: arm64+x86_64 通用二进制 (lipo 合并). WIND_MAC_UNIVERSAL=1 或 --universal 开启.
# 默认本机单架构. CI 在 job 级设环境变量, 三件套脚本统一继承同一开关.
UNIVERSAL="${WIND_MAC_UNIVERSAL:-0}"
for arg in "$@"; do
    case "$arg" in
        --debug) DEBUG_VARIANT=1 ;;
        --universal) UNIVERSAL=1 ;;
        clean) TARGETS+=("clean") ;;
        service|data|all) TARGETS+=("$arg") ;;
        *) echo "[错误] 未知参数: $arg" >&2; exit 1 ;;
    esac
done
[[ ${#TARGETS[@]} -eq 0 ]] && TARGETS=("all")

if [[ $DEBUG_VARIANT -eq 1 ]]; then
    BUILD_DIR="$REPO_DIR/build_debug"
    EXE_NAME="wind_input_debug"
    GO_TAGS="debugvariant"
    GO_LDFLAGS_EXTRA="-X github.com/huanfeng/wind_input/pkg/buildvariant.variant=debug"
else
    BUILD_DIR="$REPO_DIR/build"
    EXE_NAME="wind_input"
    GO_TAGS=""
    GO_LDFLAGS_EXTRA=""
fi

# -------- 版本号 --------
APP_VERSION="dev"
if [[ -f "$REPO_DIR/VERSION" ]]; then
    APP_VERSION=$(tr -d '\xef\xbb\xbf \t\r\n' < "$REPO_DIR/VERSION")
fi

bold()  { printf "\033[1m%s\033[0m\n" "$*"; }
info()  { printf "  %s\n" "$*"; }
warn()  { printf "\033[33m[警告] %s\033[0m\n" "$*"; }
err()   { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

# -------- clean --------
if [[ " ${TARGETS[*]} " =~ " clean " ]]; then
    bold "==> 清理 build/ build_debug/"
    rm -rf "$REPO_DIR/build" "$REPO_DIR/build_debug"
    info "已删除 build/ build_debug/"
    exit 0
fi

# -------- 工具检查 --------
command -v go     >/dev/null || { err "go 未安装"; exit 1; }
command -v curl   >/dev/null || { err "curl 未安装"; exit 1; }

bold "==> WindInput macOS Build (version=$APP_VERSION, variant=$([[ $DEBUG_VARIANT -eq 1 ]] && echo debug || echo release))"
info "REPO_DIR  = $REPO_DIR"
info "BUILD_DIR = $BUILD_DIR"

mkdir -p "$BUILD_DIR"

# -------- helper: 下载文件 --------
download_file() {
    local url="$1" dst="$2" desc="${3:-}"
    if [[ -f "$dst" ]]; then
        info "[skip] $(basename "$dst") 已存在"
        return
    fi
    info "[get ] $(basename "$dst") $desc"
    if ! curl -fsSL --retry 3 --retry-delay 2 -o "$dst" "$url"; then
        err "下载失败: $url"
        return 1
    fi
}

# -------- Build-GoService --------
build_service() {
    bold "==> [1/3] Build Go service ($EXE_NAME$([[ $UNIVERSAL -eq 1 ]] && echo ", universal"))"
    cd "$REPO_DIR/wind_input"
    local ldflags="-X main.version=$APP_VERSION"
    [[ -n "$GO_LDFLAGS_EXTRA" ]] && ldflags="$ldflags $GO_LDFLAGS_EXTRA"
    local args=(build -ldflags "$ldflags")
    [[ -n "$GO_TAGS" ]] && args+=(-tags "$GO_TAGS")
    if [[ $UNIVERSAL -eq 1 ]]; then
        # darwin 下服务纯 Go (无 cgo), 两架构各自交叉编译再 lipo 合并成通用二进制.
        # CGO_ENABLED=0 显式锁死纯 Go 交叉编译路径 (避免本机默认 CGO_ENABLED=1 触发 cgo).
        local tmp_arm tmp_amd
        tmp_arm=$(mktemp); tmp_amd=$(mktemp)
        GOOS=darwin GOARCH=arm64 CGO_ENABLED=0 go "${args[@]}" -o "$tmp_arm" ./cmd/service
        GOOS=darwin GOARCH=amd64 CGO_ENABLED=0 go "${args[@]}" -o "$tmp_amd" ./cmd/service
        lipo -create "$tmp_arm" "$tmp_amd" -output "$BUILD_DIR/$EXE_NAME"
        rm -f "$tmp_arm" "$tmp_amd"
        info "arch: $(lipo -archs "$BUILD_DIR/$EXE_NAME" 2>/dev/null || echo '?')"
    else
        args+=(-o "$BUILD_DIR/$EXE_NAME" ./cmd/service)
        go "${args[@]}"
    fi
    cd - >/dev/null
    info "$EXE_NAME 构建成功 ($(stat -f%z "$BUILD_DIR/$EXE_NAME") bytes)"
}

# -------- Download-Dictionaries --------
download_dicts() {
    bold "==> [2/3] Download dictionaries -> .cache/"
    local rime_frost="$CACHE_DIR/rime-frost"
    local rime_frost_cn="$rime_frost/cn_dicts"
    local rime_frost_en="$rime_frost/en_dicts"
    local rime_wubi="$CACHE_DIR/rime-wubi"
    local opencc="$CACHE_DIR/opencc/dictionaries"
    mkdir -p "$rime_frost_cn" "$rime_frost_en" "$rime_wubi" "$opencc"

    local FROST_BASE="https://raw.githubusercontent.com/gaboolic/rime-frost/master"
    info "rime-frost (拼音):"
    download_file "$FROST_BASE/rime_frost.dict.yaml"        "$rime_frost/rime_frost.dict.yaml"     "词库入口"
    download_file "$FROST_BASE/cn_dicts/8105.dict.yaml"     "$rime_frost_cn/8105.dict.yaml"        "单字词库 ~106KB"
    download_file "$FROST_BASE/cn_dicts/41448.dict.yaml"    "$rime_frost_cn/41448.dict.yaml"       "扩展字表 ~494KB"
    download_file "$FROST_BASE/cn_dicts/base.dict.yaml"     "$rime_frost_cn/base.dict.yaml"        "基础词库 ~10MB"
    download_file "$FROST_BASE/cn_dicts/ext.dict.yaml"      "$rime_frost_cn/ext.dict.yaml"         "扩展词库 ~8MB"
    download_file "$FROST_BASE/cn_dicts/others.dict.yaml"   "$rime_frost_cn/others.dict.yaml"      "容错词 ~16KB"
    download_file "$FROST_BASE/cn_dicts/corrections.dict.yaml" "$rime_frost_cn/corrections.dict.yaml" "错音词 ~7KB"
    download_file "$FROST_BASE/cn_dicts/tencent.dict.yaml"  "$rime_frost_cn/tencent.dict.yaml"     "腾讯词频 ~17MB"

    info "rime-frost (英文):"
    download_file "$FROST_BASE/en_dicts/en.dict.yaml"     "$rime_frost_en/en.dict.yaml"     "主词库 ~350KB"
    download_file "$FROST_BASE/en_dicts/en_ext.dict.yaml" "$rime_frost_en/en_ext.dict.yaml" "扩展 ~50KB"

    local WUBI_BASE="https://raw.githubusercontent.com/KyleBing/rime-wubi86-jidian/master"
    info "rime-wubi (五笔):"
    download_file "$WUBI_BASE/wubi86_jidian.dict.yaml"                  "$rime_wubi/wubi86_jidian.dict.yaml"                  "主词库"
    download_file "$WUBI_BASE/wubi86_jidian_extra.dict.yaml"            "$rime_wubi/wubi86_jidian_extra.dict.yaml"            "扩展词库"
    download_file "$WUBI_BASE/wubi86_jidian_extra_district.dict.yaml"   "$rime_wubi/wubi86_jidian_extra_district.dict.yaml"   "行政区域"

    local OPENCC_BASE="https://raw.githubusercontent.com/BYVoid/OpenCC/master/data/dictionary"
    info "OpenCC 简繁词典:"
    download_file "$OPENCC_BASE/STCharacters.txt" "$opencc/STCharacters.txt" "简->繁 字级"
    download_file "$OPENCC_BASE/STPhrases.txt"    "$opencc/STPhrases.txt"    "简->繁 词级"
    download_file "$OPENCC_BASE/TWVariants.txt"   "$opencc/TWVariants.txt"   "台湾正体字形"
    download_file "$OPENCC_BASE/TWPhrases.txt"    "$opencc/TWPhrases.txt"    "台湾词汇"
    download_file "$OPENCC_BASE/HKVariants.txt"   "$opencc/HKVariants.txt"   "香港繁体字形"
}

# -------- Prepare-DataFiles --------
prepare_data() {
    bold "==> [3/3] Prepare data files -> $BUILD_DIR/data/"
    local data="$BUILD_DIR/data"
    local schemas="$data/schemas"
    local pinyin="$schemas/pinyin"
    local pinyin_cn="$pinyin/cn_dicts"
    local wubi="$schemas/wubi86"
    local english="$schemas/english"
    mkdir -p "$pinyin_cn" "$wubi" "$english"

    local rime_frost="$CACHE_DIR/rime-frost"

    # 拼音入口 + cn_dicts
    cp -f "$rime_frost/rime_frost.dict.yaml" "$pinyin/" || warn "缺 rime_frost.dict.yaml"
    for f in 8105.dict.yaml 41448.dict.yaml base.dict.yaml ext.dict.yaml others.dict.yaml corrections.dict.yaml; do
        if [[ -f "$rime_frost/cn_dicts/$f" ]]; then
            cp -f "$rime_frost/cn_dicts/$f" "$pinyin_cn/"
        else
            warn "缺 cn_dicts/$f"
        fi
    done

    # Unigram 语言模型
    local unigram_cache="$CACHE_DIR/pinyin-frost/unigram.txt"
    mkdir -p "$(dirname "$unigram_cache")"
    if [[ ! -f "$unigram_cache" ]]; then
        info "生成 Unigram 语言模型 (gen_unigram)..."
        cd "$REPO_DIR/wind_input"
        if go run ./cmd/gen_unigram -rime "$rime_frost/cn_dicts" -output "$unigram_cache"; then
            info "Unigram 生成成功"
        else
            warn "Unigram 生成失败 (智能组句不可用)"
        fi
        cd - >/dev/null
    else
        info "Unigram 已缓存"
    fi
    [[ -f "$unigram_cache" ]] && cp -f "$unigram_cache" "$pinyin/unigram.txt"

    # 五笔主词库 (dictgen 按 unigram 重排)
    if [[ -f "$CACHE_DIR/rime-wubi/wubi86_jidian.dict.yaml" ]]; then
        info "生成五笔主词库 (dictgen)..."
        cd "$REPO_DIR/wind_input"
        if ! go run ./tools/dictgen -config tools/dictgen/dictgen.yaml -output "$wubi/wubi86_jidian.dict.yaml"; then
            err "dictgen 失败"; cd - >/dev/null; return 1
        fi
        cd - >/dev/null
        info "五笔主词库生成成功"
    else
        warn "缺 rime-wubi/wubi86_jidian.dict.yaml, 五笔不可用"
    fi
    # 行政区域 + 用户词库模板 (extra 由 dictgen 拆分, 不复制)
    for f in wubi86_jidian_extra_district.dict.yaml; do
        [[ -f "$CACHE_DIR/rime-wubi/$f" ]] && cp -f "$CACHE_DIR/rime-wubi/$f" "$wubi/"
    done

    # 英文词库
    for f in en.dict.yaml en_ext.dict.yaml; do
        [[ -f "$rime_frost/en_dicts/$f" ]] && cp -f "$rime_frost/en_dicts/$f" "$english/"
    done

    # OpenCC 编译为 .octrie
    if [[ -d "$CACHE_DIR/opencc/dictionaries" ]]; then
        info "编译 OpenCC -> .octrie..."
        mkdir -p "$data/opencc"
        cd "$REPO_DIR/wind_input"
        if ! go run ./cmd/gen_opencc_dict -src "$CACHE_DIR/opencc/dictionaries" -out "$data/opencc"; then
            warn "OpenCC 编译失败"
        fi
        cd - >/dev/null
    fi

    # 预制数据 (data/ → build/data/, 排除 AGENTS.md)
    info "复制 data/ 预制文件 (排除 AGENTS.md)..."
    cd "$REPO_DIR/data"
    find . -type f ! -name "AGENTS.md" -print0 | while IFS= read -r -d '' rel; do
        rel="${rel#./}"
        mkdir -p "$data/$(dirname "$rel")"
        cp -f "$rel" "$data/$rel"
    done
    cd - >/dev/null

    # 主题
    if [[ -d "$REPO_DIR/wind_input/themes" ]]; then
        info "复制主题..."
        mkdir -p "$data/themes"
        for theme_dir in "$REPO_DIR/wind_input/themes"/*/; do
            [[ -f "$theme_dir/theme.yaml" ]] || continue
            local name
            name=$(basename "$theme_dir")
            mkdir -p "$data/themes/$name"
            cp -f "$theme_dir/theme.yaml" "$data/themes/$name/"
        done
    fi
}

# -------- 路由 --------
do_service=0; do_data=0
for t in "${TARGETS[@]}"; do
    case "$t" in
        all)     do_service=1; do_data=1 ;;
        service) do_service=1 ;;
        data)    do_data=1 ;;
    esac
done

[[ $do_service -eq 1 ]] && build_service
[[ $do_data    -eq 1 ]] && { download_dicts; prepare_data; }

# -------- 摘要 --------
bold "==> Build complete"
[[ -f "$BUILD_DIR/$EXE_NAME" ]] && info "binary : $BUILD_DIR/$EXE_NAME ($(stat -f%z "$BUILD_DIR/$EXE_NAME") bytes)"
[[ -d "$BUILD_DIR/data"      ]] && info "data   : $BUILD_DIR/data ($(find "$BUILD_DIR/data" -type f | wc -l | tr -d ' ') 文件)"
echo
info "调试启动:   cd $BUILD_DIR && WIND_INPUT_LOG_LEVEL=debug ./$EXE_NAME"
info "日志:      \$HOME/Library/Logs/WindInput/wind_input.log"
info "Socket:    \$HOME/Library/Application Support/WindInput/bridge{,_push}.sock"

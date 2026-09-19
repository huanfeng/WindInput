#!/usr/bin/env bash
# WindInput 发版编排 (Linux 侧) —— docs/design/release-from-linux.md 第 4~8 节的固化。
#
#   ./scripts/release.sh status                  只读: 五仓状态一览
#   ./scripts/release.sh check                   预检: 五仓 / gh / 编译机 / 签名会话
#   ./scripts/release.sh push <版本|patch|minor> [--force]
#                                             五仓按序打 tag 并推送 (主仓最后)
#                                             --force 覆盖同名 tag (仅限草稿 Release)
#   ./scripts/release.sh wait [版本]             等 release.yml 跑完
#   ./scripts/release.sh sign-draft [版本]       拉 CI 中转产物 → 编译机签名 → 回传 → 上传
#   ./scripts/release.sh auto-sign [版本]        等 CI 跑完再自动接 sign-draft (挂机, 不用守着)
#   ./scripts/release.sh upload [版本]           只做上传+摘横幅+校验 (签名产物已在 dist/ 时)
#
# ── 为什么签名段要「拆开跑」, 而不是在编译机上直接调 release.ps1 sign-draft ──────────
# release.ps1 的 Invoke-SignDraft 已经把「拉产物 → unstage → sign → 上传 → 摘横幅」串成
# 了一条, 可它每一步都要 gh, 而编译机上没有 gh (实测 `Get-Command gh.exe` 为空)。给编译机
# 装 gh 就得在那台机器上再放一份 GitHub 登录凭据, 故这边按能力把流程劈成两侧:
#     Linux 侧 (有 gh):     拉 CI 产物、上传 Release、改正文、端到端校验
#     编译机   (有 signtool): 只做 unstage + sign 8s/9s + verify-sign + 时间戳验证
#
# ⚠️ 代价: 签名段的编排因此在 release.ps1 与本文件各有一份。改动 release.ps1 的
#    Invoke-SignDraft 时请同步本文件的 do_sign_draft —— 尤其是「签名夹在打包中间」
#    (不能对成品补签外壳) 与「摘横幅认特征串」这两条, 错了都不报错。
#
# ── 本机不编译任何东西 ──────────────────────────────────────────────────────────
# 发布产物一律来自 CI 的中转产物。Linux 上的 cargo-xwin/clang 产物在加固宿主里 COM 激活
# 失败 (6dbc8595), dev.sh stage 的 check_native_msvc 闸门也会在出口拦下它们。

set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PRODUCT_ROOT="$(dirname "$SCRIPT_DIR")"
WORK_ROOT="$(dirname "$PRODUCT_ROOT")"
DIST_DIR="$PRODUCT_ROOT/dist"
VERSION_FILE="$PRODUCT_ROOT/docs/VERSION"
GH_REPO="huanfeng/WindInput"

# ---------- 颜色 / 输出 (与 dev.sh 逐字一致) ----------
# 刻意不抽成 lib/ui.sh: dev.sh 是本仓改动最频繁的文件之一, 为四个 printf 去动它,
# 换来的合并冲突风险大于这点重复的代价。
if [ -t 1 ]; then
    C_CYAN='\033[36m'; C_YELLOW='\033[33m'; C_GREEN='\033[32m'
    C_RED='\033[31m'; C_GRAY='\033[90m'; C_RESET='\033[0m'
else
    C_CYAN=''; C_YELLOW=''; C_GREEN=''; C_RED=''; C_GRAY=''; C_RESET=''
fi
say()  { printf '%b%b%b\n' "$C_GREEN" "$1" "$C_RESET"; }
warn() { printf '%b%b%b\n' "$C_YELLOW" "$1" "$C_RESET"; }
err()  { printf '%b%b%b\n' "$C_RED" "$1" "$C_RESET"; }
gray() { printf '%b%b%b\n' "$C_GRAY" "$1" "$C_RESET"; }
cyan() { printf '%b%b%b\n' "$C_CYAN" "$1" "$C_RESET"; }
fsize() { ls -lh "$1" 2>/dev/null | awk '{print $5}'; }

# ---------- 编译机通道 ----------
# rbuild_ps / rbuild_scp / rbuild_lock 全部复用 dev.sh 那条路: 脚本经 UTF-16LE+base64
# 以 -EncodedCommand 传入, 从根上消掉 bash/ssh/PowerShell 三层引号 —— 正是
# release-from-linux.md 第 7 节「实测连踩两次转义坑」说的那个坑。
# 锁与 remote-build.ps1 用同一个锁文件, 所以签名期间编译机上的构建会排队而不是对撞。
[ -f "$SCRIPT_DIR/build.local" ] && . "$SCRIPT_DIR/build.local"
. "$SCRIPT_DIR/lib/remote-build.sh"

# ---------- 参与发布的仓库 ----------
# 顺序即执行顺序: 依赖在前, 主仓最后。第二列 = 是否打版本 tag。
#
# ★ 主仓为什么必须最后: 推 WindInput 的 v* tag 会【立刻】触发 release.yml, 而该 workflow
#   用 actions/checkout 拉附属仓时没有指定 ref, 取的是各自默认分支的最新提交。附属仓没先
#   到位, CI 就会拿旧代码构建出错误的发布包 —— 且全程不报错。
#
# ★ wind-ui-rust 只推送不打 tag: 它是 wind-setting 的 path 依赖 (windui = { path = ... }),
#   必须先到位; 但它是独立发 crates.io 的开源库, 自有版本线, 不该被打上 WindInput 的版本
#   tag。(wind-installer 走 crates.io 的 windui = "0.8", 不受影响)
#
# ⚠️ 与 release.ps1 的 $Repos 逐条对齐, 改一处要改两处。
RELEASE_REPOS=(
    "wind-ui-rust:notag"
    "wind-setting:tag"
    "wind-portable:tag"
    "wind-installer:tag"
    "WindInput:tag"
)
MAIN_REPO="WindInput"

# ---------- 目标分支: 从 repo manifest 读 default revision ----------
manifest_branch() {
    local m="$WORK_ROOT/.repo/manifests/default.xml" rev
    [ -f "$m" ] || { echo main; return 0; }
    rev="$(sed -n 's#.*<default[^>]*revision="\([^"]*\)".*#\1#p' "$m" | head -1)"
    rev="${rev#refs/heads/}"
    echo "${rev:-main}"
}

# ---------- 版本号 ----------
# tag 是版本的唯一真源 (CI 用 GITHUB_REF_NAME 覆盖 docs/VERSION), 所以「当前版本」要问
# 远端的 tag, 而不是读本机 docs/VERSION —— 后者是开发占位, 经常领先或落后于已发版本。
latest_remote_version() {
    git -C "$PRODUCT_ROOT" ls-remote --tags --refs origin 'v*' 2>/dev/null \
        | sed 's#.*refs/tags/v##' \
        | grep -E '^[0-9]+\.[0-9]+\.[0-9]+$' \
        | sort -t. -k1,1n -k2,2n -k3,3n | tail -1
}

bump_version() {
    local cur="$1" kind="$2" a b c
    IFS=. read -r a b c <<<"$cur"
    case "$kind" in
        patch) c=$((c + 1)) ;;
        minor) b=$((b + 1)); c=0 ;;
        *)     err "未知的 bump 方式: $kind"; return 1 ;;
    esac
    printf '%s.%s.%s\n' "$a" "$b" "$c"
}

valid_version() { [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; }

# bump 的基准版本。以远端最新 tag 为准 (tag 是版本真源); 但本机 docs/VERSION 更大时取它
# —— 那说明有人手工提前 bump 过, 拿远端 tag 做基准会让版本号往回退。
#
# ⚠️ 与 release.ps1 的 Show-Menu 是同一套算法, 必须保持一致: 同一个仓库在 Linux 和 Windows
#    上点「发布 Patch」要得出同一个版本号, 否则两台机器发出来的号会岔开。
bump_base() {
    local remote file
    remote="$(latest_remote_version)"
    file="$(tr -d '[:space:]' < "$VERSION_FILE" 2>/dev/null)"
    valid_version "$file" || file=""
    [ -n "$remote" ] || { printf '%s\n' "${file:-0.0.0}"; return 0; }
    [ -n "$file" ]   || { printf '%s\n' "$remote"; return 0; }
    printf '%s\n%s\n' "$remote" "$file" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1
}

# 解析「版本参数」: 显式版本号 / patch / minor / 留空(取远端最新, 用于 wait 等只读场景)。
resolve_version() {
    local arg="${1:-}" cur
    case "$arg" in
        "")            latest_remote_version ;;
        patch|minor)   cur="$(bump_base)"
                       [ -n "$cur" ] || { err "取不到基准版本, 无法 $arg bump"; return 1; }
                       bump_version "$cur" "$arg" ;;
        *)             valid_version "$arg" || { err "版本号格式应为 x.y.z (不带 v): $arg"; return 1; }
                       printf '%s\n' "$arg" ;;
    esac
}

# 确认点。AUTO_YES=1 (auto-sign 挂机模式) 时自动通过。
# ⚠️ 只有「继续做下去」这类确认走这里。涉及会白扣云签名配额、需要人判断的岔路不要塞进来
#    —— 那种在 do_sign_draft 里按 AUTO_YES 单独选了一条更省的路, 而不是闷头 yes。
AUTO_YES=0
confirm() {
    local prompt="$1" default="${2:-n}" ans hint
    if [ "$default" = y ]; then hint="[Y/n]"; else hint="[y/N]"; fi
    if [ "$AUTO_YES" = 1 ]; then
        gray "  $prompt $hint  → (挂机模式) 自动确认"
        return 0
    fi
    if [ ! -t 0 ]; then
        err "  当前不是交互式终端, 无法确认: $prompt"
        return 1
    fi
    read -r -p "  $prompt $hint " ans
    ans="$(printf '%s' "$ans" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
    case "$ans" in
        "")    [ "$default" = y ] ;;
        y|yes) return 0 ;;
        *)     return 1 ;;
    esac
}

require_gh() {
    command -v gh >/dev/null 2>&1 || { err "未安装 gh (GitHub CLI)"; return 1; }
    gh auth status >/dev/null 2>&1 || { err "gh 未登录: 先跑 gh auth login"; return 1; }
    return 0
}

# ============================================================================
# status / check —— 第 4 节: 发版前置检查
# ============================================================================
# 逐仓返回: 0=就绪 1=硬失败(游离 HEAD / 分支不对 / 仓不存在) 2=有需人工判断的情况
repo_state() {
    local name="$1" br="$2" d="$WORK_ROOT/$1" cur dirty ahead behind soft=0
    if [ ! -e "$d/.git" ]; then
        err "  ✗ $name  仓库不存在: $d"; return 1
    fi
    # ⛔ 游离 HEAD: repo sync 之后的常态。此时推送等同盲推, 绝不能发版。
    cur="$(git -C "$d" symbolic-ref --short -q HEAD)"
    if [ -z "$cur" ]; then
        err "  ✗ $name  处于游离 HEAD —— 此状态下不能发版"
        gray "      git -C $d checkout $br"
        return 1
    fi
    if [ "$cur" != "$br" ]; then
        err "  ✗ $name  在 $cur, 目标分支是 $br"
        return 1
    fi
    ahead="$(git -C "$d" rev-list --count "origin/$br..HEAD" 2>/dev/null || echo '?')"
    behind="$(git -C "$d" rev-list --count "HEAD..origin/$br" 2>/dev/null || echo '?')"
    dirty="$(git -C "$d" status --porcelain 2>/dev/null | wc -l)"

    printf '  %b%-16s%b %s' "$C_CYAN" "$name" "$C_RESET" "$cur"
    [ "$ahead"  != 0 ] && printf ' %bahead %s%b'  "$C_YELLOW" "$ahead" "$C_RESET"
    [ "$behind" != 0 ] && printf ' %bbehind %s%b' "$C_YELLOW" "$behind" "$C_RESET"
    [ "$dirty"  != 0 ] && printf ' %b未提交 %s 个文件%b' "$C_YELLOW" "$dirty" "$C_RESET"
    [ "$ahead" = 0 ] && [ "$behind" = 0 ] && [ "$dirty" = 0 ] && printf ' %b干净%b' "$C_GRAY" "$C_RESET"
    printf '\n'

    # ★ 本工作区常有并发会话。ahead 的提交未必是你的, 把别人未完成的工作 tag 进发版
    #   是不可逆的 —— 所以这里只提示, 由人来判断, 不自动放行也不自动拦死。
    if [ "$ahead" != 0 ] && [ "$ahead" != '?' ]; then
        warn "      ⚠️ 有 $ahead 个未推送提交 —— 先确认它们是不是你的 (本工作区常有并发会话)"
        gray "         git -C $d log --oneline origin/$br..HEAD"
        soft=1
    fi
    if [ "$dirty" != 0 ]; then
        warn "      ⚠️ 工作区有未提交改动 —— 它们不会进入本次发版"
        soft=1
    fi
    [ "$soft" = 1 ] && return 2
    return 0
}

do_status() {
    local br r name rc=0 st
    br="$(manifest_branch)"
    cyan "\n五仓状态 (目标分支: $br)"
    for r in "${RELEASE_REPOS[@]}"; do
        name="${r%%:*}"
        repo_state "$name" "$br"; st=$?
        [ "$st" = 1 ] && rc=1
    done
    gray "\n  本机 docs/VERSION: $(tr -d '[:space:]' < "$VERSION_FILE" 2>/dev/null || echo '?')  (开发占位, 不是发版真源)"
    gray "  远端最新 tag:      v$(latest_remote_version)"
    return $rc
}

do_check() {
    local rc=0 soft=0 br r name st out
    br="$(manifest_branch)"

    cyan "\n[1/4] 五仓状态 (目标分支: $br)"
    gray "  先 fetch 一遍, 否则 ahead/behind 是拿过期的 remote-tracking ref 算的"
    for r in "${RELEASE_REPOS[@]}"; do
        name="${r%%:*}"
        [ -e "$WORK_ROOT/$name/.git" ] && git -C "$WORK_ROOT/$name" fetch --quiet origin 2>/dev/null
    done
    for r in "${RELEASE_REPOS[@]}"; do
        name="${r%%:*}"
        repo_state "$name" "$br"; st=$?
        [ "$st" = 1 ] && rc=1
        [ "$st" = 2 ] && soft=1
    done

    cyan "\n[2/4] gh 登录"
    if require_gh; then
        gray "  $(gh auth status 2>&1 | grep -i 'Logged in' | head -1 | sed 's/^ *//')"
        say  "  ✓ 可用"
    else
        rc=1
    fi

    cyan "\n[3/4] 编译机可达 ($WIND_BUILD_REMOTE)"
    if rbuild_require_ready "release.sh check" >/dev/null 2>&1; then
        out="$(rbuild_ps "hostname" 2>&1 | tr -d '\r' | tail -1)"
        if [ -n "$out" ]; then say "  ✓ $out"; else err "  ✗ 连不上"; rc=1; fi
    else
        err "  ✗ 未配置 scripts/build.local (WIND_BUILD_REMOTE / WIND_BUILD_ROOT)"
        rc=1
    fi

    cyan "\n[4/4] 云签名会话"
    gray "  ★ 时序: 签名会话只活 2 小时, CI 约 20 分钟 —— 先在编译机桌面登录会话再触发发版,"
    gray "    这样 CI 跑完时会话仍有效。"
    gray "  ⚠️ HasPrivateKey=True 不是判据 (会话没建立时它照样为 True), 只认下面这段的结论。"
    if [ -n "$WIND_BUILD_ROOT" ]; then
        rbuild_ps "Set-Location '$WIND_BUILD_ROOT'; .\\scripts\\dev.ps1 sign-status" 2>&1 | tr -d '\r'
        if [ "${PIPESTATUS[0]}" != 0 ]; then
            warn "  ⚠️ 签名会话不可用 —— 打 tag 前先去编译机桌面登录 (手机二次验证)"
            soft=1
        fi
    fi

    printf '\n'
    if [ "$rc" != 0 ]; then
        err "预检未通过 —— 上面标 ✗ 的项必须先解决。"
        return 1
    fi
    if [ "$soft" = 1 ]; then
        warn "预检通过, 但有需要你亲自判断的项 (见上面 ⚠️)。"
        return 0
    fi
    say "预检全部通过。下一步: ./scripts/release.sh push <版本|patch|minor>"
    return 0
}

# ============================================================================
# push —— 第 5 节: 五仓按序打 tag 并推送
# ============================================================================

# `--force` 重发前的把关: 目标版本的 Release 必须还是草稿, 或者根本还没有。
#
# ⛔ 覆盖【已发布】版本的 tag = 同一个版本号先后指向两份不同的代码。这比覆盖资产更狠 ——
#    已下载的用户手上那份与仓库对不上, R2 的 latest.json 仍按旧 hash 分发, 而且事后连
#    「这个版本到底是哪份代码」都无从追溯。要改就换个号, tag 是廉价的。
release_overwritable() {
    local tag="$1" out
    if out="$(gh release view "$tag" -R "$GH_REPO" --json isDraft -q .isDraft 2>/dev/null)"; then
        if [ "$out" = true ]; then
            gray "  $tag 的 Release 还是草稿, 可以覆盖"
            return 0
        fi
        err "  ✗ $tag 已经【发布】, 拒绝 --force 重发。"
        err "     同一个版本号会指向两份代码: 已下载用户的包与仓库对不上, R2 的"
        err "     latest.json 也仍按旧 hash 分发。请改用新版本号。"
        return 1
    fi
    gray "  远端还没有 $tag 的 Release (首次发这个号)"
    return 0
}

# $2 = 1 时 --force: 覆盖已存在的同名 tag (本地 -f + 远端 --force)。
do_push() {
    local v="$1" force="${2:-0}" br tag name mode d done_list=() main_tag_kept=0
    br="$(manifest_branch)"
    tag="v$v"

    cyan "\n准备发布 $tag  (目标分支: $br)"

    if [ "$force" = 1 ]; then
        warn "  --force: 已存在的 $tag 会被覆盖 (本地 -f + 远端 --force)"
        require_gh || return 1
        release_overwritable "$tag" || return 1
    else
        # 先把「tag 已存在」查干净。tag 顺序一旦推错, 删远端 tag 的代价远高于换个版本号
        # 重发 (见 release-from-linux.md 第 9 节), 所以宁可在动手前多查一轮。
        local existing=0
        for r in "${RELEASE_REPOS[@]}"; do
            name="${r%%:*}"; mode="${r##*:}"; d="$WORK_ROOT/$name"
            [ "$mode" = tag ] || continue
            if git -C "$d" rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
                err "  ✗ $name 本地已有 tag $tag"; existing=1
            elif [ -n "$(git -C "$d" ls-remote --tags origin "refs/tags/$tag" 2>/dev/null)" ]; then
                err "  ✗ $name 远端已有 tag $tag"; existing=1
            fi
        done
        if [ "$existing" = 1 ]; then
            gray "     重发同一个版本号请加 --force (仅当该版本的 Release 还是草稿)"
            return 1
        fi
        say "  ✓ 五仓均无 $tag, 可以发布"
    fi

    printf '\n'
    warn "即将按下面的顺序推送 —— 主仓的 tag 一到远端就会【立刻】触发 CI 构建:"
    for r in "${RELEASE_REPOS[@]}"; do
        name="${r%%:*}"; mode="${r##*:}"
        if [ "$mode" = tag ]; then gray "    $name  → push $br + tag $tag"
        else                       gray "    $name  → 只 push $br (自有版本线, 不打本产品的 tag)"; fi
    done
    printf '\n'
    local what="确认发布 $tag ?"
    [ "$force" = 1 ] && what="确认 force 重发 $tag (覆盖远端同名 tag) ?"
    confirm "$what" n || { gray "已取消, 什么都没做。"; return 0; }

    for r in "${RELEASE_REPOS[@]}"; do
        name="${r%%:*}"; mode="${r##*:}"; d="$WORK_ROOT/$name"
        printf '\n'
        cyan "── $name ──"

        # ⚠️ 仓库可能装有 pre-push hook (wind-ui-rust 会跑 clippy + 全量测试), 推送会很慢。
        #    耐心等, 别用 --no-verify 绕过。
        gray "  push $br ... (仓内若有 pre-push hook 会跑测试, 可能要几分钟)"
        if ! git -C "$d" push origin "$br"; then
            err "  ✗ $name push 失败"
            push_abort_hint "${done_list[@]}"
            return 1
        fi

        if [ "$mode" = tag ]; then
            # ★ tag 已经指向本轮 HEAD 的仓直接跳过: 那种「覆盖」只是换个 tagger 时间戳,
            #   引用一个字节都不变, 不值得冒一次 force push 的险 (对齐 release.ps1)。
            #   ⚠️ 代价是它不产生新的 release.yml run —— 主仓命中时下面会专门提示。
            local tag_sha head_sha
            tag_sha="$(git -C "$d" rev-parse -q --verify "refs/tags/$tag^{commit}" 2>/dev/null)"
            head_sha="$(git -C "$d" rev-parse HEAD)"
            if [ -n "$tag_sha" ] && [ "$tag_sha" = "$head_sha" ]; then
                say "  ✓ $name  tag 已指向本轮 HEAD, 跳过打 tag"
                [ "$name" = "$MAIN_REPO" ] && main_tag_kept=1
                done_list+=("$name"); continue
            fi
            local tagargs=(tag -a "$tag" -m "Release $tag")
            local pushargs=(push origin "$tag")
            if [ "$force" = 1 ]; then tagargs=(tag -f -a "$tag" -m "Release $tag"); pushargs+=(--force); fi
            if ! git -C "$d" "${tagargs[@]}"; then
                err "  ✗ $name 打 tag 失败"; push_abort_hint "${done_list[@]}"; return 1
            fi
            if ! git -C "$d" "${pushargs[@]}"; then
                err "  ✗ $name push tag 失败 (本地 tag 已打, 可用 git -C $d tag -d $tag 撤掉)"
                push_abort_hint "${done_list[@]}"; return 1
            fi
            say "  ✓ $name  $br + $tag 已到远端"
        else
            say "  ✓ $name  $br 已到远端 (按设计不打 tag)"
        fi
        done_list+=("$name")
    done

    printf '\n'
    if [ "$main_tag_kept" = 1 ]; then
        warn "五仓推送完成, 但主仓的 tag 没有移动 —— 【不会】产生新的 release.yml run。"
        gray "  要重跑构建: gh run rerun <runId> -R $GH_REPO"
        gray "  (run 号: gh run list --workflow release.yml --branch $tag -R $GH_REPO)"
        return 0
    fi
    say "五仓推送完成, CI 应在 1 分钟内起来。"
    gray "  下一步: 菜单 [9] 等CI+签名 (挂机), 或 ./scripts/release.sh auto-sign $v"
    gray "          想分步走: wait $v 然后 sign-draft $v"
    return 0
}

# 中途失败时, 把「已经到远端的仓」说清楚 —— 半推状态下贸然重来会把顺序搞乱。
push_abort_hint() {
    printf '\n'
    if [ "$#" = 0 ]; then
        warn "尚未有任何仓推到远端, 修好问题后原样重跑即可。"
    else
        warn "已经推到远端的仓: $*"
        warn "⚠️ 别直接重跑 —— 顺序已经推进到一半。两个选择:"
        gray "   a) 修好失败的那个仓, 手工把【剩下的】仓按 RELEASE_REPOS 的顺序推完"
        gray "   b) 换一个新版本号整轮重发 (tag 是廉价的, 比删远端 tag 稳妥)"
    fi
}

# ============================================================================
# wait —— 第 6 节: 等 CI
# ============================================================================
# CI 产出的两个 job 缺一不可: 缺了 macOS 就是「发布包静默少组件」—— macOS pkg 曾整版
# 缺设置 app, 全程无报错。
#
# ⚠️ 判据用的是 job 的【显示名】(中文), 不是 workflow 里的 job id。实测
#   `gh run view --json jobs` 返回的 .name 是 `name:` 字段而非 `build-windows` 这种 id,
#   拿 id 去匹配会一个都对不上 ⇒ 「没有失败的 job」恒成立 ⇒ 护栏恒空过。
CI_JOB_WINDOWS="Windows 安装包"
CI_JOB_MACOS="macOS 安装包"

# 定位某个 tag 的 run。$2=only_success 时只取成功的。
ci_find_run() {
    local tag="$1" only="${2:-}" filter='.[0].databaseId'
    [ "$only" = success ] && filter='[.[] | select(.conclusion=="success")][0].databaseId'
    gh run list --workflow release.yml --branch "$tag" --limit 10 -R "$GH_REPO" \
        --json databaseId,conclusion,status,createdAt -q "$filter" 2>/dev/null | grep -v '^null$'
}

do_wait() {
    local v="$1" tag="v$1" run st concl waited=0
    require_gh || return 1

    cyan "\n等待 $tag 的 release.yml ..."
    # tag 触发的 run, 其 headBranch 即 tag 名。
    for _ in $(seq 1 20); do
        run="$(ci_find_run "$tag")"
        [ -n "$run" ] && break
        gray "  还没看到 run, 15s 后再看 ... (推 tag 后通常 1 分钟内起来)"
        sleep 15
    done
    [ -n "$run" ] || { err "5 分钟内没有出现 $tag 的 release.yml run。"; \
        gray "  查看: gh run list --workflow release.yml -R $GH_REPO"; return 1; }

    gray "  run $run  ($(gh run view "$run" -R "$GH_REPO" --json url -q .url 2>/dev/null))"
    while :; do
        st="$(gh run view "$run" -R "$GH_REPO" --json status -q .status 2>/dev/null)"
        [ "$st" = completed ] && break
        printf '\r%b  [%4ds] %s ...%b' "$C_GRAY" "$waited" "${st:-查询中}" "$C_RESET"
        sleep 20; waited=$((waited + 20))
    done
    printf '\r%*s\r' 60 ''

    concl="$(gh run view "$run" -R "$GH_REPO" --json conclusion -q .conclusion 2>/dev/null)"
    cyan "\nCI 结束 (${waited}s), 各 job:"
    gh run view "$run" -R "$GH_REPO" --json jobs -q '.jobs[] | "  \(.conclusion)\t\(.name)"' 2>/dev/null

    if [ "$concl" != success ]; then
        err "\nrun $run 的结论是 $concl —— 不能继续。"
        gray "  gh run view $run --log-failed -R $GH_REPO"
        return 1
    fi

    # 两个产物 job 必须都在且都成功。少一个是静默的, 所以逐个点名而不是只看整体结论。
    local jobs miss=0 j
    jobs="$(gh run view "$run" -R "$GH_REPO" --json jobs -q '.jobs[] | select(.conclusion=="success") | .name' 2>/dev/null)"
    for j in "$CI_JOB_WINDOWS" "$CI_JOB_MACOS"; do
        if ! printf '%s\n' "$jobs" | grep -qxF "$j"; then
            err "  ✗ 没有成功的 job「$j」"; miss=1
        fi
    done
    if [ "$miss" = 1 ]; then
        err "\n产物 job 不齐 —— 发版是 Windows + macOS 两套产物, 缺一不可。"
        gray "  若是 CI 改了 job 的显示名, 请同步本脚本的 CI_JOB_WINDOWS / CI_JOB_MACOS。"
        return 1
    fi

    printf '\n'
    say "CI 通过, 两套产物齐全。"
    warn "  ⚠️ artifact 保留期 14 天, 过期只能重跑 CI。"
    gray "  下一步: ./scripts/release.sh sign-draft $v"
    return 0
}

# ============================================================================
# sign-draft —— 第 7~8 节: 签名 + 上传
# ============================================================================

# Linux 侧独立验签: 直接读 PE 的证书表 (第 5 个数据目录), size > 0 即带签名。
# ⚠️ 它只证明「证书表非空」, 证明不了签名有效、也看不出有无时间戳 —— 那两样只有编译机
#    上的 signtool verify /pa /v 能给 (见 remote_verify_timestamps)。两者互补, 别互相替代。
pe_has_signature() {
    python3 - "$1" <<'PY' 2>/dev/null
import struct, sys
b = open(sys.argv[1], "rb").read()
opt = struct.unpack_from("<I", b, 0x3C)[0] + 24
magic = struct.unpack_from("<H", b, opt)[0]
dd = opt + (112 if magic == 0x20b else 96)   # PE32+ 与 PE32 的数据目录起点不同
_rva, size = struct.unpack_from("<II", b, dd + 4 * 8)
sys.exit(0 if size > 0 else 1)
PY
}

# 解包便携包, 逐个 PE 查证书表。
# ⚠️ 这是必须做的一步: verify-sign 只扫 dist\ 顶层、验不到 zip 内部 —— 而 Authenticode
#    只认 PE, zip 本身签不了, 所以「Setup.exe 验过了」推不出「便携包里那 5 个也签了」。
verify_portable_contents() {
    local zip="$1" tmp n=0 bad=0 f
    command -v unzip >/dev/null 2>&1 || { warn "  ⚠️ 没有 unzip, 跳过便携包内部验签"; return 0; }
    tmp="$(mktemp -d)"
    if ! unzip -q "$zip" -d "$tmp"; then
        err "  ✗ 解包便携包失败: $zip"; rm -rf "$tmp"; return 1
    fi
    while IFS= read -r -d '' f; do
        n=$((n + 1))
        if pe_has_signature "$f"; then
            gray "      ✓ ${f#"$tmp"/}"
        else
            err  "      ✗ 证书表为空 (裸的): ${f#"$tmp"/}"; bad=$((bad + 1))
        fi
    done < <(find "$tmp" -type f \( -iname '*.exe' -o -iname '*.dll' \) -print0)
    rm -rf "$tmp"
    if [ "$n" = 0 ]; then err "  ✗ 便携包里一个 PE 都没有 —— 包是空的?"; return 1; fi
    [ "$bad" != 0 ] && { err "  ✗ 便携包内有 $bad 个 PE 没有签名"; return 1; }
    say "  ✓ 便携包内 $n 个 PE 均带签名"
    return 0
}

# 编译机: 把 dist\ 里同名的旧包挪走 (只移不删)。
# ⚠️ 这不是可选的清洁工作, 是硬前提 —— ① 若之前在编译机上本地编过同版本号, dist\ 里会
#    躺着【同名】的 Setup/Portable, 和 CI 版混在一起签完根本分不清签的是哪一份;
#    ② verify-sign 扫 dist\ 顶层且不递归, 历史遗留的未签名旧包会把它的退出码拖成 1。
remote_archive_old_dist() {
    local ps
    ps='$d = "@@ROOT@@/dist"
if (-not (Test-Path $d)) { New-Item -ItemType Directory $d -Force | Out-Null }
$a = Join-Path $d ("_archive/pre-release-" + (Get-Date -Format "MMdd-HHmm"))
$old = @(Get-ChildItem $d -File -EA SilentlyContinue |
         Where-Object { $_.Name -match "^WindInput(Dev)?-(Setup|Portable|Stage)-" })
if ($old.Count -eq 0) { "NONE"; exit 0 }
New-Item -ItemType Directory $a -Force | Out-Null
$old | Move-Item -Destination $a -Force
"MOVED " + $old.Count'
    ps="${ps//@@ROOT@@/$WIND_BUILD_ROOT}"
    rbuild_ps "$ps" 2>&1 | tr -d '\r'
}

# 编译机: 还原中转产物 → 签名 → 重新打包 → 验签。
#
# ⚠️ 为什么必须【重新打包】而不能对 CI 的成品补签外壳: 签名夹在打包中间 —— PE 要在封进
#    压缩块之前签。补签只签得到外壳, 包内 5 个 PE 仍是全裸的, 而 signtool verify 验
#    Setup.exe 照样通过, 从外面完全看不出来。
remote_sign() {
    local v="$1" ps
    ps='$ErrorActionPreference = "Stop"
Set-Location "@@ROOT@@"
.\scripts\dev.ps1 unstage ".\dist\WindInput-Stage-@@V@@.zip"; if ($LASTEXITCODE) { exit 11 }
.\scripts\dev.ps1 sign 8s;                                    if ($LASTEXITCODE) { exit 12 }
.\scripts\dev.ps1 sign 9s;                                    if ($LASTEXITCODE) { exit 13 }
.\scripts\dev.ps1 verify-sign;                                if ($LASTEXITCODE) { exit 14 }
exit 0'
    ps="${ps//@@ROOT@@/$WIND_BUILD_ROOT}"
    ps="${ps//@@V@@/$v}"
    rbuild_ps "$ps" 2>&1 | tr -d '\r'
    return "${PIPESTATUS[0]}"
}

# 编译机: 验便携包内每个 PE 的【时间戳】。
# ★ 时间戳不可省 —— 没有它, 签名会在证书到期当天集体失效, 连早已发出去、用户机器上装着
#   的包也会一起变成「未知发布者」。而这一项 verify-sign 验不到 (它不看 zip 内部)。
remote_verify_timestamps() {
    local v="$1" ps
    ps='$ErrorActionPreference = "Stop"
$st = (Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter signtool.exe -EA SilentlyContinue |
        Where-Object { $_.FullName -match "\\x64\\" } | Sort-Object FullName -Descending)[0].FullName
if (-not $st) { Write-Output "没找到 signtool.exe"; exit 21 }
$tmp = Join-Path $env:TEMP "wind-verify-portable"
if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
Expand-Archive "@@ROOT@@/dist/WindInput-Portable-@@V@@.zip" $tmp -Force
$bad = 0; $n = 0
Get-ChildItem $tmp -Recurse -File | Where-Object { $_.Extension -in @(".exe", ".dll") } | ForEach-Object {
    $n++
    $o  = & $st verify /pa /v $_.FullName 2>&1
    $ts = ($o | Where-Object { $_ -match "timestamped" }) -replace ".*timestamped:\s*", ""
    if ($ts) { "{0,-24} 时间戳 {1}" -f $_.Name, $ts.Trim() }
    else     { $bad++; "{0,-24} ★无时间戳★" -f $_.Name }
}
Remove-Item $tmp -Recurse -Force
if ($n -eq 0)   { Write-Output "便携包里没有 PE"; exit 22 }
if ($bad -gt 0) { Write-Output ("有 " + $bad + " 个 PE 没有时间戳"); exit 23 }
exit 0'
    ps="${ps//@@ROOT@@/$WIND_BUILD_ROOT}"
    ps="${ps//@@V@@/$v}"
    rbuild_ps "$ps" 2>&1 | tr -d '\r'
    return "${PIPESTATUS[0]}"
}

# 本地 dist/ 里本版本的 4 个待传资产
release_assets() {
    local v="$1"
    printf '%s\n' \
        "$DIST_DIR/WindInput-Setup-$v.exe" \
        "$DIST_DIR/WindInput-Setup-$v.exe.sha256" \
        "$DIST_DIR/WindInput-Portable-$v.zip" \
        "$DIST_DIR/WindInput-Portable-$v.zip.sha256"
}

# 只覆盖草稿 Release。覆盖【已发布】的会让已下载用户的 sha256 对不上, 且
# release-published.yml 早已按旧文件同步到 R2、那边的 latest.json 也指向旧 hash。
# release.ps1 对此默认拒绝, 这边照做。
require_draft_release() {
    local tag="$1" isdraft
    isdraft="$(gh release view "$tag" -R "$GH_REPO" --json isDraft -q .isDraft 2>/dev/null)"
    case "$isdraft" in
        true)  return 0 ;;
        false) err "  ✗ $tag 已经【发布】, 不是草稿。"
               err "     覆盖已发布的 Release 会让已下载用户的 sha256 对不上, 且 R2 上的"
               err "     latest.json 仍指向旧 hash。要强行继续请自行 gh release upload --clobber。"
               return 1 ;;
        *)     err "  ✗ 读不到 $tag 的 Release (还没创建? CI 的 publish job 没跑?)"
               return 1 ;;
    esac
}

do_sign_draft() {
    local v="$1" tag="v$1" run tmp stagezip a rc
    require_gh || return 1
    rbuild_require_ready "sign-draft" || return 1

    cyan "\n══ 签名段  $tag ══"
    require_draft_release "$tag" || return 1

    # 签名产物已经在本地? 那就别重签 —— 重跑一遍要白扣 7 次云签名配额。
    if [ -f "$DIST_DIR/WindInput-Setup-$v.exe" ] && [ -f "$DIST_DIR/WindInput-Portable-$v.zip" ]; then
        warn "  ⚠️ dist/ 里已经有 $v 的 Setup 与便携包。"
        # ★ 挂机模式下不问、也不重签: 重跑签名段要白扣 7 次云签名配额, 而这时 dist/ 里
        #   躺着的多半就是上一轮签好、只是上传失败的那份 —— 直接转上传才是本意。真要重签
        #   请手动跑 sign-draft。
        if [ "$AUTO_YES" = 1 ]; then
            warn "     挂机模式: 跳过签名直接走上传 (重签会白扣 7 次配额)。"
            do_upload "$v"
            return $?
        fi
        gray "     若上一轮只是上传失败, 直接跑 ./scripts/release.sh upload $v (不重签, 不扣配额)。"
        confirm "仍要重新走一遍签名?" n || { gray "已取消。"; return 0; }
    fi

    # ---------- 1. 定位 CI run 并拉中转产物 ----------
    cyan "\n[1/7] 定位 CI 构建并拉取中转产物"
    run="$(ci_find_run "$tag" success)"
    [ -n "$run" ] || { err "  ✗ $tag 没有成功的 release.yml 构建"; return 1; }
    gray "  run $run"

    tmp="$(mktemp -d)"
    if ! gh run download "$run" --name stage-windows --dir "$tmp" -R "$GH_REPO"; then
        err "  ✗ 下载 stage-windows 失败。artifact 保留期 14 天, 过期需重跑 CI。"
        rm -rf "$tmp"; return 1
    fi
    stagezip="$(find "$tmp" -maxdepth 1 -name 'WindInput-Stage-*.zip' | head -1)"
    [ -n "$stagezip" ] || { err "  ✗ 下载结果里没有中转产物包"; rm -rf "$tmp"; return 1; }
    say "  ✓ $(basename "$stagezip")  ($(fsize "$stagezip"))"

    # 整个「传上去 → 签 → 传回来」期间持有编译机锁, 免得并发的 dev.sh 1 把 build\ 冲掉。
    # 锁文件与 remote-build.ps1 共用, 所以 Windows 那侧的构建也会一起排队。
    rbuild_trap_on
    if ! rbuild_lock; then rbuild_trap_off; rm -rf "$tmp"; return 1; fi

    _sign_body "$v" "$stagezip"; rc=$?

    rbuild_cleanup
    rbuild_trap_off
    rm -rf "$tmp"
    [ "$rc" = 0 ] || return "$rc"

    # ---------- 上传 ----------
    do_upload "$v"
}

_sign_body() {
    local v="$1" stagezip="$2" out rc

    # ---------- 2. 清场 ----------
    cyan "\n[2/7] 归档编译机 dist\\ 里的同名旧包"
    out="$(remote_archive_old_dist)"
    case "$out" in
        *NONE*)  gray "  dist\\ 里没有同名旧包" ;;
        *MOVED*) say  "  ✓ 已移入 dist\\_archive\\ : ${out##*MOVED }" ;;
        *)       warn "  ⚠️ 归档步骤的输出不符合预期, 原样贴出:"; printf '%s\n' "$out" ;;
    esac

    # ---------- 3. 传中转产物 + 对齐版本号 ----------
    cyan "\n[3/7] 上传中转产物并对齐编译机的 docs/VERSION"
    if ! rbuild_scp "$stagezip" "$WIND_BUILD_REMOTE:$WIND_BUILD_ROOT/dist/" "中转产物"; then
        err "  ✗ 上传中转产物失败"; return 1
    fi
    # unstage 有版本硬校验 (stage.json 的版本 vs 编译机的 docs/VERSION)。tag 是版本真源,
    # 而仓库里的 docs/VERSION 是开发占位, 所以签名前必须把编译机那份对齐到 tag 版本。
    # ⚠️ 必须用 scp 传文件, 别拼 PowerShell 写文件 —— 文档里实测连踩两次转义坑。
    local vf; vf="$(mktemp)"; printf '%s\n' "$v" > "$vf"
    if ! rbuild_scp "$vf" "$WIND_BUILD_REMOTE:$WIND_BUILD_ROOT/docs/VERSION" "docs/VERSION"; then
        err "  ✗ 同步 docs/VERSION 失败"; rm -f "$vf"; return 1
    fi
    rm -f "$vf"
    say "  ✓ 中转产物已就位, 编译机 docs/VERSION = $v"
    gray "  ⚠️ 编译机的 docs/VERSION 被改成了 $v; 下次 dev.sh 构建同步回本机占位版本时,"
    gray "     dev.ps1 的 Sync-VersionStamp 会触发一次 cargo clean + 全量重编 (约 9 分钟)。"

    # ---------- 4. 签名 ----------
    cyan "\n[4/7] 编译机: unstage → sign 8s → sign 9s → verify-sign"
    gray "  下面是编译机的原样输出。★ 逐条看, 脚本报「完成」不算数:"
    gray "    · unstage 要打印【两行】→ : build\\ 和 target\\release (安装器三件套)。"
    gray "      只有第一行说明三件套没还原, pack.ps1 会现编 —— 那就不是 CI 那份二进制了。"
    gray "    · sign 8s 应出现【三段】签名: 5 个文件 → 1 个(uninstall.exe) → 1 个(Setup.exe),"
    gray "      共 7 次配额, 不是 6 次。"
    gray "    · sign 9s 应报「0 个已签, 5 个跳过」—— 按签名者指纹识别为已签, 不重复扣配额。"
    printf '%b  %s%b\n' "$C_GRAY" "$(printf '─%.0s' $(seq 1 60))" "$C_RESET"
    remote_sign "$v"; rc=$?
    printf '%b  %s%b\n' "$C_GRAY" "$(printf '─%.0s' $(seq 1 60))" "$C_RESET"
    case "$rc" in
        0)  say "  ✓ 签名与验签通过" ;;
        11) err "  ✗ unstage 失败 (多半是版本硬校验没过: 中转产物的版本与 $v 不符)"; return 1 ;;
        12) err "  ✗ sign 8s 失败 —— 会话过期? 去编译机桌面重新登录后重跑本步"; return 1 ;;
        13) err "  ✗ sign 9s 失败"; return 1 ;;
        14) err "  ✗ verify-sign 未通过。⚠️ 它扫 dist\\ 顶层且不递归 —— 历史遗留的未签名旧包"
            err "     会把退出码拖成 1, 先确认报错指的是本次产物再下结论。"; return 1 ;;
        *)  err "  ✗ 编译机返回 $rc"; return 1 ;;
    esac

    # ---------- 5. 时间戳 ----------
    cyan "\n[5/7] 编译机: 验便携包内每个 PE 的时间戳"
    remote_verify_timestamps "$v"; rc=$?
    if [ "$rc" != 0 ]; then
        err "  ✗ 时间戳校验未通过 (退出码 $rc)"
        err "     没有时间戳的签名会在证书到期当天集体失效 —— 连已发出去的包一起。不能发。"
        return 1
    fi
    say "  ✓ 便携包内每个 PE 都有时间戳"

    # ---------- 6. 回传 ----------
    cyan "\n[6/7] 回传 4 个签名资产"
    local pat ok=1
    for pat in "WindInput-Setup-$v.exe" "WindInput-Setup-$v.exe.sha256" \
               "WindInput-Portable-$v.zip" "WindInput-Portable-$v.zip.sha256"; do
        if rbuild_scp "$WIND_BUILD_REMOTE:$WIND_BUILD_ROOT/dist/$pat" "$DIST_DIR/" "$pat"; then
            gray "  ✓ $pat  ($(fsize "$DIST_DIR/$pat"))"
        else
            err "  ✗ 回传失败: $pat"; ok=0
        fi
    done
    [ "$ok" = 1 ] || { err "  资产不齐, 停在这里 (签名产物还在编译机 dist\\, 不必重签)"; return 1; }

    # ---------- 7. Linux 侧独立验签 ----------
    cyan "\n[7/7] 本机独立验签 (读 PE 证书表, 不依赖 signtool)"
    if pe_has_signature "$DIST_DIR/WindInput-Setup-$v.exe"; then
        say "  ✓ Setup.exe 带签名"
    else
        err "  ✗ Setup.exe 的证书表是空的 —— 拿回来的是未签名版, 不能上传"; return 1
    fi
    verify_portable_contents "$DIST_DIR/WindInput-Portable-$v.zip" || return 1
    return 0
}

# ============================================================================
# upload —— 第 8 节: 上传 Release、摘横幅、端到端校验
# ============================================================================
# 单独成一个子命令, 因为「签名产物已出、上传失败」时【绝不能重跑签名段】: 那会重新拉产物、
# 重签一遍, 白扣 7 次云签名配额。产物就在 dist/, 直接从这里接着跑。
do_upload() {
    local v="$1" tag="v$1" a missing=0
    require_gh || return 1

    cyan "\n══ 上传  $tag ══"
    require_draft_release "$tag" || return 1

    while IFS= read -r a; do
        if [ -f "$a" ]; then gray "  $(basename "$a")  ($(fsize "$a"))"
        else err "  ✗ 不存在: $a"; missing=1; fi
    done < <(release_assets "$v")
    [ "$missing" = 0 ] || { err "  资产不齐, 无法上传。"; return 1; }

    printf '\n'
    confirm "覆盖 $tag 的这 4 个资产?" y || { gray "已取消 (签名产物留在 dist/)。"; return 0; }

    local args=()
    while IFS= read -r a; do args+=("$a"); done < <(release_assets "$v")
    if ! gh release upload "$tag" "${args[@]}" --clobber -R "$GH_REPO"; then
        err "\n  ✗ 上传失败。签名产物已在 dist/, 修好网络后重跑本命令即可 —— 别回去重签。"
        return 1
    fi
    say "  ✓ 4 个资产已上传"

    # ---------- 摘掉未签名横幅 ----------
    cyan "\n摘掉正文里的未签名横幅"
    local body clean
    body="$(mktemp)"; clean="$(mktemp)"
    if ! gh release view "$tag" -R "$GH_REPO" --json body -q .body > "$body"; then
        warn "  ⚠️ 读取 Release 正文失败, 请手动删掉未签名横幅后再发布。"
        rm -f "$body" "$clean"
    else
        if strip_unsigned_banner "$body" > "$clean"; then
            if gh release edit "$tag" --notes-file "$clean" -R "$GH_REPO" >/dev/null; then
                say "  ✓ 已摘除"
            else
                warn "  ⚠️ 写回正文失败, 请手动删。"
            fi
        else
            gray "  正文里没有未签名横幅 (已删过, 或本次 CI 判定为已签名)"
        fi
        rm -f "$body" "$clean"
    fi

    # ---------- 资产齐全 + 端到端校验 ----------
    cyan "\n清点 Release 资产"
    local names
    names="$(gh release view "$tag" -R "$GH_REPO" --json assets -q '.assets[].name' 2>/dev/null)"
    printf '%s\n' "$names" | sed 's/^/  /'
    # ⚠️ 少组件是静默的, 数一遍。macOS 的 .pkg 由 CI 直传, 不经本流程 —— 但它必须在。
    if ! printf '%s\n' "$names" | grep -q '\.pkg$'; then
        err "  ✗ 没有 macOS 的 .pkg —— 发版是 Windows + macOS 两套产物, 缺一不可。"
        return 1
    fi
    say "  ✓ macOS .pkg 在"

    cyan "\n端到端校验: 把文件真下回来比对 sha256"
    gray "  上传成功不等于挂上去的就是签名版。"
    local d f r l bad=0
    d="$(mktemp -d)"
    for f in "WindInput-Setup-$v.exe" "WindInput-Portable-$v.zip"; do
        if ! gh release download "$tag" --pattern "$f" --dir "$d" -R "$GH_REPO" >/dev/null 2>&1; then
            err "  ✗ 下载失败: $f"; bad=1; continue
        fi
        r="$(sha256sum "$d/$f" | cut -d' ' -f1)"
        l="$(sha256sum "$DIST_DIR/$f" | cut -d' ' -f1)"
        if [ "$r" = "$l" ]; then say "  ✓ $f"; else err "  ✗ $f 与本地签名版不一致!"; bad=1; fi
    done
    rm -rf "$d"
    [ "$bad" = 0 ] || { err "\n端到端校验未通过 —— 别发布这个 Release。"; return 1; }

    printf '\n'
    say "签名产物已上传并校验通过。"
    gray "  最后一步由人来做: 去 GitHub 确认草稿正文, 填好「更新说明」后点发布。"
    gray "  $(gh release view "$tag" -R "$GH_REPO" --json url -q .url 2>/dev/null)"
    return 0
}

# 从 Release 正文里摘掉未签名横幅。stdin → stdout; 没找到时返回 1 (stdout 无输出)。
#
# ★ 判定必须认「未经代码签名」这个特征串, 不能认「第一个 [!WARNING] 块」: 正文后面还有
#   两条给用户看的提示 (SmartScreen 信誉积累、macOS 未公证的打开方式), 认错了就会把它们
#   一起删掉。release.ps1 的 Remove-UnsignedBanner 就是这么防的。
# ⚠️ 正文从【文件参数】进, 不走 stdin: `python3 - <<'PY'` 里的 stdin 已经被 heredoc
#    (也就是脚本自己) 占住了, 此时 sys.stdin.read() 读到的是空串 —— 于是横幅永远识别不出来,
#    而函数照常返回。实测踩过: 用真实正文跑出来 0 行输出、横幅原封不动留在 Release 上。
strip_unsigned_banner() {
    python3 - "$1" <<'PY'
import re, sys
lines = re.split(r'\r?\n', open(sys.argv[1], encoding="utf-8").read())
start = next((i for i, l in enumerate(lines) if re.match(r'^>\s*\[!WARNING\]', l)), -1)
if start < 0:
    sys.exit(1)
end = start
while end + 1 < len(lines) and re.match(r'^>', lines[end + 1]):
    end += 1
if '未经代码签名' not in "\n".join(lines[start:end + 1]):
    sys.exit(1)          # 特征串不匹配, 拒绝删除
while end + 1 < len(lines) and lines[end + 1].strip() == "":
    end += 1
kept = (lines[:start] if start else []) + (lines[end + 1:] if end + 1 < len(lines) else [])
sys.stdout.write("\n".join(kept))
PY
}

# ============================================================================
# auto-sign —— 等 CI → 自动接签名上传
# ============================================================================
# 挂机用: 推完 tag 就可以走开, 回来时草稿 Release 上已经是签名版。对齐 release.ps1 的
# auto-sign 子命令。
#
# ★ 前提是签名会话【已经建立】。会话只活 2 小时而 CI 约 20 分钟 —— 正确顺序是先在编译机
#   桌面登录会话, 再推 tag; 等 CI 的这 20 分钟里会话一直在倒计时, 不是等完了再去登录。
#
# 中止 (Ctrl+C) 不会留下半成品: tag 与草稿 Release 都还在, 事后 sign-draft 可原样接上。
do_auto_sign() {
    local v="$1" rc
    cyan "\n══ 等 CI 并自动签名上传  v$v ══"
    gray "  随时可 Ctrl+C 中止 —— tag 与草稿 Release 都不会丢, 事后 sign-draft 接着跑即可。"
    do_wait "$v" || return 1
    AUTO_YES=1
    do_sign_draft "$v"; rc=$?
    AUTO_YES=0
    return "$rc"
}

# ============================================================================
# 交互菜单
# ============================================================================
# 无参数直接跑进这里 (对齐 dev.sh 与 release.ps1 的习惯); 子命令仍可单独调, CI/脚本用那个。
pause() { printf '\n'; read -e -r -p "按回车继续..." _; }

# 菜单数据带缓存: menu_refresh 要跑 git ls-remote (联网, 1~2 秒), 每次操作完回到菜单都
# 重查一遍会把「看一眼状态」变成等待。只有 push 会改变这几个值, 所以只在它之后置脏;
# 想手动重查按 r。
MENU_DIRTY=1
MENU_BASE=""; MENU_LATEST=""; MENU_FILEVER=""; MENU_AHEAD=""; MENU_BRANCH=""
menu_refresh() {
    printf '%b正在查询远端 tag ...%b\r' "$C_GRAY" "$C_RESET"
    MENU_BRANCH="$(manifest_branch)"
    MENU_LATEST="$(latest_remote_version)"
    MENU_FILEVER="$(tr -d '[:space:]' < "$VERSION_FILE" 2>/dev/null)"
    MENU_BASE="$(bump_base)"
    # 不 fetch: 菜单要快, 这里只是给个量级提示。真正的把关在 [1] 预检里 (那边会 fetch)。
    MENU_AHEAD="$(git -C "$PRODUCT_ROOT" rev-list --count "origin/$MENU_BRANCH..HEAD" 2>/dev/null || echo '?')"
    printf '%*s\r' 30 ''
}

show_menu() {
    local sep
    sep="$(printf '=%.0s' $(seq 1 64))"
    clear 2>/dev/null || true
    printf '%b%s%b\n' "$C_CYAN" "$sep" "$C_RESET"
    printf '%b  WindInput 发版  (Linux; 签名委托编译机 %s)%b\n' "$C_CYAN" "${WIND_BUILD_REMOTE:-未配置 build.local}" "$C_RESET"
    printf '%b%s%b\n' "$C_CYAN" "$sep" "$C_RESET"
    printf '  最新已发布 tag : '
    if [ -n "$MENU_LATEST" ]; then say "v$MENU_LATEST"; else warn "(无)"; fi
    printf '  docs/VERSION   : %s' "${MENU_FILEVER:-?}"
    gray "   (本地构建占位, 非版本真源)"
    printf '  主仓待推提交   : %s 个' "$MENU_AHEAD"
    gray "   (分支 $MENU_BRANCH; 未 fetch, 仅供参考)"
    printf '%b%s%b\n\n' "$C_CYAN" "$sep" "$C_RESET"

    printf '%b  发布 (按顺序走):%b\n' "$C_YELLOW" "$C_RESET"
    printf '    1  预检        '; gray "五仓 / gh / 编译机 / 签名会话; 不做任何改动"
    printf '    2  发布 Patch  '; say "v$MENU_BASE  →  v$(bump_version "$MENU_BASE" patch)"
    printf '    3  发布 Minor  '; say "v$MENU_BASE  →  v$(bump_version "$MENU_BASE" minor)"
    printf '    4  发布当前版  '
    if [ -n "$MENU_LATEST" ] && [ "$MENU_BASE" = "$MENU_LATEST" ]; then
        printf 'v%s' "$MENU_BASE"; warn "   ⚠️ 远端已有此 tag —— 选它会 force 重发 (仅限草稿)"
    else
        printf 'v%s' "$MENU_BASE"; gray "   (docs/VERSION 的版本; 远端尚无此 tag)"
    fi
    printf '    5  指定版本    '; gray "自己输入 x.y.z"
    printf '\n'
    printf '%b  发布之后 (下面三项操作 %s):%b\n' "$C_YELLOW" "$(if [ -n "$MENU_LATEST" ]; then echo "v$MENU_LATEST"; else echo "最新 tag"; fi)" "$C_RESET"
    printf '    6  等 CI       '; gray "守着 release.yml 跑完 (约 20 分钟; windows 与 macos 两个 job)"
    printf '    7  签名+上传   '; gray "拉 CI 产物 → 编译机签名 → 回传 → 覆盖草稿 Release → 端到端校验"
    printf '    8  只上传      '; gray "签名已出而上传失败时的恢复路径 (不重签, 不扣配额)"
    printf '    9  等CI+签名   '; gray "守着 CI 跑完再自动接 [7]; 推完 tag 就能走开 (需签名会话已建立)"
    printf '\n'
    printf '%b  其它:%b\n' "$C_YELLOW" "$C_RESET"
    printf '    s  状态        '; gray "五仓分支 / 待推 / 脏文件 (不联网)"
    printf '    r  刷新        '; gray "重查远端 tag (上面几个值是进菜单时取的, 不会自己变)"
    printf '    h  帮助        '; gray "子命令用法"
    printf '    q  退出\n'
    printf '%b%s%b\n' "$C_CYAN" "$sep" "$C_RESET"
    gray "  ★ 时序: 先在编译机桌面登录签名会话 (2 小时时限) 再发布, CI 跑完时会话才还有效。"
}

# 6/7/8 处理的版本: 刚 push 完时, 远端最新 tag 就是本次要处理的那个。
menu_target_version() {
    [ -n "$MENU_LATEST" ] && { printf '%s\n' "$MENU_LATEST"; return 0; }
    err "远端没有 v* tag, 没有可处理的版本。"
    return 1
}

menu_loop() {
    local choice v rc
    # 菜单要交互式终端; 管道 / CI 里请用子命令。
    if [ ! -t 0 ]; then
        err "当前不是交互式终端, 无法显示菜单。"
        gray "  请改用子命令: release.sh check|status|push|wait|sign-draft|upload"
        return 1
    fi
    while :; do
        # 每轮开头复位: do_auto_sign 正常走完会自己复位, 但它中途失败返回时若哪天加了
        # 提前 return, 残留的 AUTO_YES=1 会让后面手动选的操作全部静默自动确认。
        AUTO_YES=0
        if [ "$MENU_DIRTY" = 1 ]; then menu_refresh; MENU_DIRTY=0; fi
        show_menu
        printf '\n'
        read -e -r -p "请选择: " choice
        choice="$(printf '%s' "$choice" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
        rc=0
        case "$choice" in
            "")  continue ;;
            q)   gray "已退出。"; return 0 ;;
            1)   do_check; rc=$? ;;
            2)   v="$(bump_version "$MENU_BASE" patch)"; do_push "$v"; rc=$?; MENU_DIRTY=1 ;;
            3)   v="$(bump_version "$MENU_BASE" minor)"; do_push "$v"; rc=$?; MENU_DIRTY=1 ;;
            4)   if [ -n "$MENU_LATEST" ] && [ "$MENU_BASE" = "$MENU_LATEST" ]; then
                     printf '\n'
                     warn "远端已有 v$MENU_BASE —— 重发会用 --force 覆盖该 tag。"
                     gray "  只有该版本的 Release 还是草稿时才允许, 脚本会先查一遍。"
                     if confirm "确认 force 重发 v$MENU_BASE ?" n; then
                         do_push "$MENU_BASE" 1; rc=$?
                     else
                         gray "已取消。"; rc=0
                     fi
                 else
                     do_push "$MENU_BASE"; rc=$?
                 fi
                 MENU_DIRTY=1 ;;
            5)   printf '\n'; read -e -r -p "版本号 (x.y.z, 不带 v): " v
                 v="$(printf '%s' "$v" | tr -d '[:space:]')"
                 if valid_version "$v"; then do_push "$v"; rc=$?; MENU_DIRTY=1
                 else err "版本号格式应为 x.y.z (不带 v): $v"; rc=1; fi ;;
            6)   if v="$(menu_target_version)"; then do_wait "$v"; rc=$?; else rc=1; fi ;;
            7)   if v="$(menu_target_version)"; then do_sign_draft "$v"; rc=$?; else rc=1; fi ;;
            8)   if v="$(menu_target_version)"; then do_upload "$v"; rc=$?; else rc=1; fi ;;
            9)   if v="$(menu_target_version)"; then do_auto_sign "$v"; rc=$?; else rc=1; fi ;;
            r)   MENU_DIRTY=1; continue ;;
            s)   do_status; rc=$? ;;
            h)   usage; rc=0 ;;
            *)   err "无效选项: $choice"; sleep 1; continue ;;
        esac
        [ "$rc" -ne 0 ] && err "\n(退出码 $rc)"
        pause
    done
}

# ============================================================================
usage() {
    cat <<'USAGE'
WindInput 发版编排 (Linux 侧)

  ./scripts/release.sh status                  五仓状态一览 (只读)
  ./scripts/release.sh check                   预检: 五仓 / gh / 编译机 / 签名会话
  ./scripts/release.sh push <版本|patch|minor> [--force]
                                               五仓按序打 tag 并推送 (主仓最后);
                                               --force 覆盖已存在的同名 tag, 仅限草稿
  ./scripts/release.sh wait [版本]             等 release.yml 跑完
  ./scripts/release.sh sign-draft [版本]       拉 CI 产物 → 编译机签名 → 回传 → 上传
  ./scripts/release.sh auto-sign [版本]        等 CI 跑完再自动接 sign-draft (挂机)
  ./scripts/release.sh upload [版本]           只上传 (签名产物已在 dist/ 时的恢复路径)

版本号留空时取远端最新的 v* tag。完整流程与每步的检查点见
docs/design/release-from-linux.md。

★ 时序: 先在编译机桌面登录云签名会话 (2 小时时限), 再 push 触发 CI (约 20 分钟),
  这样 CI 跑完时会话仍然有效。
USAGE
}

main() {
    local cmd="${1:-}" v
    case "$cmd" in
        status)     do_status ;;
        check)      do_check ;;
        push)
            [ -n "${2:-}" ] || { err "push 需要版本号: <x.y.z> | patch | minor"; return 1; }
            v="$(resolve_version "$2")" || return 1
            local f=0
            case "${3:-}" in
                --force|-f) f=1 ;;
                "")         ;;
                *)          err "push 的第三个参数只能是 --force: ${3}"; return 1 ;;
            esac
            do_push "$v" "$f" ;;
        wait)
            v="$(resolve_version "${2:-}")" || return 1
            [ -n "$v" ] || { err "取不到版本号"; return 1; }
            do_wait "$v" ;;
        auto-sign|autosign)
            v="$(resolve_version "${2:-}")" || return 1
            [ -n "$v" ] || { err "取不到版本号"; return 1; }
            do_auto_sign "$v" ;;
        sign-draft|sign)
            v="$(resolve_version "${2:-}")" || return 1
            [ -n "$v" ] || { err "取不到版本号"; return 1; }
            do_sign_draft "$v" ;;
        upload)
            v="$(resolve_version "${2:-}")" || return 1
            [ -n "$v" ] || { err "取不到版本号"; return 1; }
            do_upload "$v" ;;
        ""|menu)    menu_loop ;;
        -h|--help|help) usage ;;
        *)          err "未知命令: $cmd"; printf '\n'; usage; return 1 ;;
    esac
}

main "$@"

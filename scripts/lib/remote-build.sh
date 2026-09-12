# shellcheck shell=bash
# ============================================================================
# remote-build.sh — 把构建放到 Windows 编译机上跑, 产物回传本机 build[_dev]/
# ----------------------------------------------------------------------------
# 由 scripts/dev.sh 载入。协议与 scripts/remote-build.ps1 (Windows→Windows 的那条路)
# 同构: 同步 → 执行 → 回传, 全程 SSH; 连编译机上的互斥锁都用【同一个锁文件】, 两条路
# 因此互相排队而不是互相踩踏。
#
# ── 为什么 Linux 侧必须把编译推出去 ──────────────────────────────────────────
# clang / cargo-xwin 交叉编出的 wind_tsf.dll 在带安全加固的宿主 (企业微信 / TIM / QQ /
# UU浏览器) 里 COM 激活失败, 同 commit 的原生 MSVC 版正常 —— A/B 实测把唯一变量锁定在
# 工具链上 (6dbc8595)。实测两份 DLL 的 PE 安全元数据 (DllCharacteristics/SafeSEH) 逐位
# 相同, 差异落在【代码生成层】, 换 cargo-xwin 的用法或版本都无效。
# 由此 Linux 上的 cargo-xwin / clang 只剩 check/clippy 一个正当用途 —— 那两个不链接、
# 不产出任何交付物。凡是会落进 build[_dev]/ 的东西, 一律推到编译机上用 cl.exe 编。
#
# ── 与 remote-build.ps1 的差异 ──────────────────────────────────────────────
#  ① 未配置时【硬失败】而不是回落本机。那边回落是对的 (Windows 本机自己就有 MSVC);
#     这边回落只会安静地产出一堆按上述理由不能用的东西。
#  ② 8/9 (打包) 也转发, 并额外回传 dist/ —— remote-build.ps1 刻意不转发它们, 因为
#     它的本机就能签名+打包; Linux 既不能签名也不能打包 (pack-installer.sh 会交叉编
#     installer stub, 又是被判死的那条链)。
#     ⚠️ 因此这边回传的 Setup.exe / 便携 zip 【未签名】, 只能自测安装流程, 不能发版。
#        发版链路是: dev.sh 1 → dev.sh stage → scp → 本机 dev.ps1 unstage → sign 8s/9s。
# ============================================================================

# ── ★ 为什么本文件的一切都带 rbuild_ 前缀 ────────────────────────────────────
# dev.sh 里【已经有】一个 remote_ps —— 那是推送到**实测靶机**用的 (走 $WIND_REMOTE),
# 与编译机是两台不同的机器。本文件最初也叫 remote_ps, 而 dev.sh 对它的定义在 source
# 本文件【之后】, 于是后定义的覆盖先定义的: 构建请求被发往靶机的地址。
# ⚠️ 这个撞车不会报「函数重复」, 只会在 $WIND_REMOTE 未配置时得到一句
#    `ssh: Could not resolve hostname :` —— 空主机名。若靶机恰好配了地址, 它会安静地
#    把编译命令发到靶机上去跑, 那才是真正难查的形态。
# ⇒ 本文件的函数与变量一律 rbuild_ / RBUILD_ 前缀, 与 dev.sh 的 remote_* (靶机) 分开。
#    加新函数时先 grep dev.sh 确认不重名。

# ---------- 配置 ----------
# scripts/build.local (gitignore; 模板见 build.local.example) 或环境变量。
WIND_BUILD_REMOTE="${WIND_BUILD_REMOTE:-}"     # user@host
WIND_BUILD_ROOT="${WIND_BUILD_ROOT:-}"         # 编译机上的仓库路径, 如 C:/build/WindInput
# ★ 去掉尾部斜杠后再用。prune 靠 '<根>'.Length+1 把绝对路径切成相对路径去比对清单,
#   多一个尾斜杠就会把每个相对路径多切掉一个字符 ⇒ 清单里一个都对不上 ⇒ 整棵树被判成
#   残留并【逐个删除】。这是本文件唯一会造成不可逆后果的地方, 故在入口就规范化。
WIND_BUILD_ROOT="${WIND_BUILD_ROOT%/}"
WIND_BUILD_ROOT="${WIND_BUILD_ROOT%\\}"
WIND_BUILD_PS="${WIND_BUILD_PS:-pwsh}"         # 编译机上跑 dev.ps1 的 PowerShell
# 需要一并同步的伴生仓 (与产品仓平级的目录名), 空格分隔。
# ⚠️ wind-setting 必须连 wind-ui-rust 一起 —— 它是 path 依赖 (windui = { path = "../wind-ui-rust" }),
#    仓不在就直接「系统找不到指定的路径 (os error 3)」。
WIND_BUILD_SIBLINGS="${WIND_BUILD_SIBLINGS:-wind-setting wind-ui-rust wind-portable wind-installer}"

# ---------- worktree 槽位 ----------
# 每个 linked worktree 在编译机上占一份【自己的父目录】, 否则多个 worktree 全映射到同一个
# WIND_BUILD_ROOT: 同步是镜像, 于是每次切 worktree 都要把对方的树 prune 掉再铺自己的,
# 而 docs/VERSION 一有差异 dev.ps1 的 Sync-VersionStamp 还会 cargo clean + 删 tsf-cmake
# ⇒ 每换一次 worktree 就是一次 ~9 分钟全量重编。锁只保证不并发踩踏, 挡不住这个。
#
# ★★ 换的是【父目录】而不是主仓目录名: 伴生仓与主仓平级, 而 wind-setting/Cargo.toml 里
#    写死了三条相对 path 依赖 (../WindInput/wind_input/crates/...)。只改主仓名, wind-setting
#    仍会去 ../WindInput 取那三个 crate —— 取到的是【主树】代码, 编译照样成功, 错得毫无提示。
#        默认     C:/build/{WindInput, wind-setting, wind-ui-rust, ...}
#        槽位 fx  C:/build-fx/{WindInput, wind-setting, wind-ui-rust, ...}
#
# 判据与 remote-build.ps1 一致: git-dir != git-common-dir 即 linked worktree。主树两者相同
# (本仓虽是 repo 工具布局, 但在仓内 rev-parse 返回的都是相对 .git —— 已实测), 派生不出槽位,
# 行为与从前逐字不变。「忘了设 → 静默互相覆盖」这坑踩过太多次, 默认值必须是安全的那个。
# ⚠️ 每个槽位各带一份 target/ (几十 GB), 用完记得删掉编译机上的整个槽位目录。
rbuild_apply_slot() {
    local slot="${WIND_BUILD_SLOT:-}" gd cd
    case "$slot" in
        0|none|off) return 0 ;;                       # 显式关掉: 与主树共用(串台风险自负)
        "") gd="$(git -C "$PRODUCT_ROOT" rev-parse --git-dir 2>/dev/null)"
            cd="$(git -C "$PRODUCT_ROOT" rev-parse --git-common-dir 2>/dev/null)"
            # 没装 git / 不是仓库 ⇒ 两者都空 ⇒ 当作主树, 不派生。
            [ -n "$gd" ] && [ -n "$cd" ] && [ "$gd" != "$cd" ] && slot="$(basename "$PRODUCT_ROOT")"
            ;;
    esac
    [ -n "$slot" ] || return 0
    # 分支名里的 / : 之类会把路径打断, 非安全字符一律折成 '-'
    slot="$(printf '%s' "$slot" | tr -c 'A-Za-z0-9._-' '-')"
    slot="${slot#-}"; slot="${slot%-}"
    [ -n "$slot" ] || return 0
    WIND_BUILD_ROOT="$(dirname "$WIND_BUILD_ROOT")-$slot/$(basename "$WIND_BUILD_ROOT")"
    WIND_BUILD_SLOT_ACTIVE="$slot"
}
WIND_BUILD_SLOT_ACTIVE=""
[ -n "$WIND_BUILD_ROOT" ] && rbuild_apply_slot

# SSH 选项: 关掉交互, 避免脚本里卡在 known_hosts 询问。
# ★ ServerAlive* 与 remote-build.ps1 的 $SshOpts 对齐, 不是可省的: 远端 tar 打包整个
#   build/(含 data/ 22MB) 与 cargo 的长链接阶段都会有【数分钟一个字节都不往回吐】,
#   NAT/防火墙的空闲回收会把连接掐掉 —— 表现为间歇性「远程打包失败」/「退出码 255」,
#   而手动逐条跑都正常, 与下面 rbuild_scp 退避重试那段是同一类判据。
RBUILD_SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=15
                 -o ServerAliveInterval=30 -o ServerAliveCountMax=40)

# ---------- 命令分类 ----------
# 走远程的命令 = 凡会写进 build[_dev]/ 或 dist/ 的。
# ⛔ 不含 k/check、l/clippy、t/test、f/fmt、ci: 它们不产出交付物。check/clippy 在 Linux
#    上用 cargo-xwin 跑得更快 (CI 的 ubuntu job 正是这么跑的), 而 test 走的是【原生
#    cargo test】, 与交叉编译根本无关 —— 推到编译机上只会更慢。
# ⛔ 不含 stage: 它只是把已有的 build/ 打成中转包, 不编译任何东西。
rbuild_is_forwarded() {
    # ── 逃生口: WIND_BUILD_LOCAL=1 就地用 cargo-xwin/clang 编 ──────────────────
    # 给「agent 想在本机直接编出二进制看看链接过不过」这类快速验证留的口子。⚠️ 产物
    # 只能自己看, 不能部署也不能发版 —— 正是 6dbc8595 判死的那种。stage 的
    # check_native_msvc 闸门会在出口把它拦下, 所以这个口子漏不到发版链上去;
    # 但 p1/pd1 部署【不经过】那道闸门, 别拿它推到靶机上做「表现是否正常」的判断。
    # 纯粹只想知道编不编得过, 用 k/check、l/clippy 更快, 不必开这个。
    [ "${WIND_BUILD_LOCAL:-}" = 1 ] && return 1
    case "$1" in
        1|release|d1|dev|m1|dm1|m2|dm2|m3|dm3|m4|dm4|gd|gen-data) return 0 ;;
        8|installer|pack|8s|installer-skip|9|portable-zip|9s|portable-zip-skip) return 0 ;;
        *) return 1 ;;
    esac
}

# dev.sh 的别名 → dev.ps1 认识的规范名。
# ⚠️ 两边的命令集不完全一样: dev.sh 有 installer-skip / portable-zip-skip 这些长别名,
#    dev.ps1 只认 8s / 9s。不规范化就会在编译机上得到「未知命令」而整条链白跑。
rbuild_canon_cmd() {
    case "$1" in
        release)            echo 1  ;;
        dev)                echo d1 ;;
        installer|pack)     echo 8  ;;
        installer-skip)     echo 8s ;;
        portable-zip)       echo 9  ;;
        portable-zip-skip)  echo 9s ;;
        gen-data)           echo gd ;;
        *)                  echo "$1" ;;
    esac
}

# 命令 → 产物落在 build 还是 build_dev。
#
# ⚠️ 判据必须与 dev.ps1 的【实际写入目录】对齐, 不能与命令的拼写规律对齐。gd/gen-data
#    不以 d 开头, 但 Do-GenData 默认就往 build_dev\ 写 —— 照搬「d 开头 = dev」的规律曾让
#    远程 gen-data 静默失效: 去编译机的 build\ 取产物, 把上一次全构建留下的【旧】build\
#    整个盖回本机, 全程 exit 0 且日志照打「完成」。
rbuild_outdir_name() {
    case "$1" in
        d*|gd) echo build_dev ;;
        *)     echo build ;;
    esac
}

# 命令 → 只回传这几个文件 (空 = 整目录回传)。
# dm2 只需 1 个 exe (20 MB), 没必要连 data/ 22 MB 一起拉。
rbuild_artifacts_for() {
    case "$1" in
        m1)     echo "wind_tsf.dll wind_tsf_x86.dll" ;;
        dm1)    echo "wind_tsf_dev.dll wind_tsf_x86_dev.dll" ;;
        m2)     echo "wind_input.exe wind_cli.bat" ;;
        dm2)    echo "wind_input_dev.exe wind_cli.bat" ;;
        m3)     echo "wind_setting.exe" ;;
        dm3)    echo "wind_setting_dev.exe" ;;
        m4|dm4) echo "wind_portable.exe" ;;
        *)      echo "" ;;
    esac
}

# 命令 → 整目录回传时只取这个子目录 (空 = 整个 build[_dev]/)。
# gen-data 只产出 data/; 照全构建那样整目录回传会把编译机上的 exe 一并盖到本机, 那些
# 可能比本机刚编的还旧 —— 正是「改了没生效」这类问题最难查的成因, 且没有任何提示。
rbuild_subdir_for() {
    case "$1" in
        gd) echo data ;;
        *)  echo "" ;;
    esac
}

# 命令 → 除 build[_dev]/ 外还要回传的目录。
# 8/9 的成品落 dist/ 而不是 build/ —— 不取它等于「远程打的包留在编译机上」, 本机一无所获。
rbuild_extra_dirs_for() {
    case "$1" in
        8|8s|9|9s) echo "dist" ;;
        *)         echo "" ;;
    esac
}

# 命令 → 真正需要的伴生仓。dm1/dm2 (最常用) 完全用不到, 每次都同步四个仓纯属浪费。
rbuild_siblings_for() {
    local need=""
    case "$1" in
        1|d1)     need="wind-setting wind-ui-rust wind-portable" ;;
        m3|dm3)   need="wind-setting wind-ui-rust" ;;
        m4|dm4)   need="wind-portable" ;;
        8|8s)     need="wind-setting wind-ui-rust wind-portable wind-installer" ;;
        9|9s)     need="wind-setting wind-ui-rust wind-portable" ;;
        *)        need="" ;;
    esac
    # 与配置清单取交集 —— 用户可能只列了其中一部分。
    local out="" n c
    for n in $need; do
        for c in $WIND_BUILD_SIBLINGS; do
            [ "$n" = "$c" ] && { out="$out $n"; break; }
        done
    done
    printf '%s\n' "${out# }"
}

# ---------- 就绪判据 ----------
# ⚠️ 硬失败而非回落本机 —— 理由见文件头「与 remote-build.ps1 的差异 ①」。
rbuild_require_ready() {
    local cmd="$1"
    if [ -n "${WIND_NO_REMOTE:-}" ]; then
        err "WIND_NO_REMOTE 已设, 但 '$cmd' 必须在编译机上跑 —— Linux 交叉编的产物不能用。"
        err "  只想验证代码编不编得过, 用 k/check、l/clippy、t/test —— 那三个本来就在本机跑,"
        err "  也正是 cargo-xwin 在 Linux 上仅存的正当用途。要本机出二进制见 WIND_BUILD_LOCAL。"
        return 1
    fi
    if [ -z "$WIND_BUILD_REMOTE" ] || [ -z "$WIND_BUILD_ROOT" ]; then
        err "'$cmd' 需要 Windows 编译机, 但未配置。"
        err "  cp scripts/build.local.example scripts/build.local  然后填 WIND_BUILD_REMOTE / WIND_BUILD_ROOT"
        err "  为什么不能在 Linux 上编: clang/cargo-xwin 出的 wind_tsf.dll 在加固宿主 COM 激活失败"
        err "  (6dbc8595, 根因在工具链代码生成层)。Linux 侧只剩 check/clippy 一个正当用途。"
        return 1
    fi
    command -v iconv >/dev/null 2>&1 || { err "需要 iconv (编码远端 PowerShell 脚本)"; return 1; }
    return 0
}

# ---------- 远端执行 ----------
# 脚本经 UTF-16LE+base64 传入, 彻底避开 bash/ssh/cmd 多层引号。
# ⚠️ 编译机 sshd 的默认 shell 是 cmd.exe (实测 `echo A; hostname` 整串被当字面量回显),
#    别指望它是 PowerShell。
# ⚠️ 必须在远端把控制台编码设成 UTF-8: 中文 Windows 的默认代码页是 936(GBK), dev.ps1
#    的中文输出经 SSH 传回来在 Linux 终端上全是乱码, 连报错都读不了。
# ⚠️ -OutputFormat Text 不能省: stdout 被 SSH 重定向时, PowerShell 会把 information 流
#    (Write-Host 在 PS5+ 走的就是它, dev.ps1 的 Say/Warn/Gray 全是 Write-Host) 再序列化
#    成 CLIXML 吐到 stderr —— 于是每一行中文输出旁边都跟着一大坨
#    `#< CLIXML <Objs Version="1.1.0.1"...>`, 真正的报错会被冲得无影无踪。实测加上它
#    之后输出干净, 且远端退出码照常透传 (验过 exit 7 → $? = 7)。
rbuild_ps() {
    local script="$1" b64
    # $ProgressPreference 是给 WIND_BUILD_PS="powershell" (5.1) 备的: 那个版本即使有
    # -OutputFormat Text 也照样把 progress 记录序列化成 CLIXML 吐 stderr (在靶机上实测过)。
    # pwsh 7 不需要, 但配置模板里明写了可以改用 5.1, 故在此一并钉死。
    script="\$ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding=[Text.Encoding]::UTF8; \$OutputEncoding=[Text.Encoding]::UTF8; $script"
    b64="$(printf '%s' "$script" | iconv -f UTF-8 -t UTF-16LE | base64 | tr -d '\n')"
    ssh "${RBUILD_SSH_OPTS[@]}" "$WIND_BUILD_REMOTE" \
        "$WIND_BUILD_PS -NoProfile -NonInteractive -OutputFormat Text -EncodedCommand $b64"
}

# scp 带退避重试。
# Windows OpenSSH 不支持 ControlMaster 连接复用, 一次远程构建要连开 4~6 条 SSH 会话
# (上传/解压/执行/回传), 短时间密集建连会偶发被 sshd 拒。判据是「手动逐条跑必成功、
# 脚本里却间歇失败」—— 那指向密集建连而非配置错误, 故退避重试而不是改 sshd。
rbuild_scp() {
    local from="$1" to="$2" what="$3" try
    for try in 1 2 3; do
        scp "${RBUILD_SSH_OPTS[@]}" -q "$from" "$to" && return 0
        [ "$try" -lt 3 ] && { warn "  scp $what 第 $try 次失败, 重试..."; sleep "$try"; }
    done
    return 1
}

# ---------- 编译机互斥锁 ----------
# ★ 锁文件路径与 remote-build.ps1 的 $LockFile 【必须一致】($RRoot.buildlock) —— 否则
#   Linux 这条路和 Windows 那条路各锁各的, 两边同时构建会在同一个 target\ 上打架。
# 锁放在仓库目录【旁边】而不是仓库内, 因此既不会被同步覆盖, 也不会被 prune 清掉。
RBUILD_LOCK_OWNED=0
RBUILD_TMPFILES=()

# 中断时也要收尾 —— remote-build.ps1 那侧是 try/finally, bash 这侧没有对应物, 只能靠 trap。
# 一次 9 分钟全构建中途按 Ctrl+C 很常见; 不收尾的话锁要卡到 30 分钟陈旧判据生效, 而且因为
# 两条路【共用同一个锁文件】, Windows 那侧也跟着一起被堵住。
rbuild_cleanup() {
    rbuild_unlock
    [ "${#RBUILD_TMPFILES[@]}" -gt 0 ] && rm -f "${RBUILD_TMPFILES[@]}" 2>/dev/null
    RBUILD_TMPFILES=()
    return 0
}
# INT 里先收尾再把默认行为还给自己, 这样 Ctrl+C 的退出状态仍是「被信号中断」而不是 0。
rbuild_trap_on() {
    trap 'rbuild_cleanup; trap - INT; kill -INT $$' INT
    trap 'rbuild_cleanup' TERM
}
rbuild_trap_off() { trap - INT TERM; }

rbuild_lock() {
    local lock="${WIND_BUILD_ROOT}.buildlock" holder="linux/$(whoami)/pid$$" try r stale=0
    for try in $(seq 1 120); do
        # CreateNew 是原子的: 文件已存在就抛异常, 抢锁因此不需要额外的比较交换。
        r="$(rbuild_ps "try { \
  \$fs=[IO.File]::Open('$lock','CreateNew','Write','None'); \
  \$sw=New-Object IO.StreamWriter(\$fs); \$sw.Write('$holder'); \$sw.Dispose(); 'OK' \
} catch { \
  \$age=((Get-Date)-(Get-Item '$lock' -EA SilentlyContinue).LastWriteTime).TotalMinutes; \
  if (\$age -gt 30) { Remove-Item '$lock' -Force -EA SilentlyContinue; 'STALE' } \
  else { 'BUSY:'+((Get-Content '$lock' -Raw -EA SilentlyContinue) -replace '\s','') } \
}" 2>/dev/null | tr -d '\r' | tail -1)"
        case "$r" in
            OK)     RBUILD_LOCK_OWNED=1; return 0 ;;
            STALE)  stale=$((stale + 1))        # 陈旧锁已清除, 立刻重试抢占(不 sleep)
                    # 但连续 STALE 说明 Remove-Item 一直失败(权限/占用), 再转下去就是
                    # 120 次无间隔建连 —— 正好撞上 sshd 对密集建连的拒绝。
                    [ "$stale" -ge 3 ] && { err "  [锁] 陈旧锁清不掉 (连续 $stale 次), 放弃。"; return 1; }
                    continue ;;
            BUSY:*) [ "$try" = 1 ] && warn "  [锁] 编译机正被占用 (${r#BUSY:}), 等待..." ;;
            "")     return 0 ;;                     # 连不上时不因为锁卡死, 后续步骤自会报错
        esac
        sleep 10
    done
    err "  [锁] 等待编译机超过 20 分钟仍未空闲, 已放弃。"
    err "  确认无人在用后手动清: ssh $WIND_BUILD_REMOTE \"del ${WIND_BUILD_ROOT//\//\\\\}.buildlock\""
    return 1
}
rbuild_unlock() {
    [ "$RBUILD_LOCK_OWNED" = 1 ] || return 0
    RBUILD_LOCK_OWNED=0
    rbuild_ps "Remove-Item '${WIND_BUILD_ROOT}.buildlock' -Force -EA SilentlyContinue" >/dev/null 2>&1 || true
}

# ---------- 源码同步 ----------
# 排除清单的理由:
#   target/      50 GB / 7.8 万文件 —— 同步它等于把远程构建的收益全部倒赔进去
#   build*/      产物目录, 回传方向相反; 传过去会用本机旧产物覆盖编译机刚产出的
#   .git/        编译机只要工作树, 不要历史 (故编译机上那份【不是】git 仓)
#   .cache/      词库下载缓存, 编译机自行下载即可 (它已有一份)
#   .claude/ 等  AI 工具的会话历史与运行时状态 —— 与编译无关, 却占几千文件
#   *.local*     两机配置不同, 传过去会让编译机拿本机的部署目标办事
RBUILD_EXCLUDE_DIRS="target build build_dev build_mac build_debug dist .git node_modules .cache .claude .remember .omc .omx .vscode .idea"
RBUILD_EXCLUDE_FILES="*.log *.pdb *.local *.local.ps1"

# 同步一棵树到编译机。$1=本地路径 $2=远端路径 $3=标签
rbuild_sync_tree() {
    local src="$1" rdir="$2" label="$3"
    [ -d "$src" ] || { gray "  - 跳过 $label (本机不存在)"; return 0; }

    local rnd tgz rtgz ex=() d f
    rnd="$(tr -dc 'a-z0-9' < /dev/urandom 2>/dev/null | head -c 6)"; rnd="${rnd:-$$}"
    tgz="$(mktemp -t "wi-src-$rnd-XXXX.tar.gz")"; RBUILD_TMPFILES+=("$tgz")
    rtgz="C:/Windows/Temp/wi-src-$rnd.tar.gz"

    # --exclude 按归档内路径做 fnmatch。以 -C <root> . 打包时路径形如 ./wind_input/...,
    # 故顶层与嵌套两种模式都要给 —— 只写 --exclude=target 不保险。
    for d in $RBUILD_EXCLUDE_DIRS;  do ex+=("--exclude=./$d" "--exclude=*/$d"); done
    for f in $RBUILD_EXCLUDE_FILES; do ex+=("--exclude=$f"); done

    tar -czf "$tgz" --format=gnu "${ex[@]}" -C "$src" . 2>/dev/null \
        || { err "  打包 $label 失败"; rm -f "$tgz"; return 1; }
    local mb; mb="$(fsize "$tgz")"

    if ! rbuild_scp "$tgz" "$WIND_BUILD_REMOTE:$rtgz" "上传 $label"; then
        err "  scp 上传 $label 失败 (已重试 3 次)"; rm -f "$tgz"; return 1
    fi
    rm -f "$tgz"

    # ---- 解压 + 清理残留 (prune) ----
    # tar 解压是【叠加】不是镜像: 只创建和覆盖, 从不删除。本机删掉的文件会永远留在编译机上。
    # 对 src/ 多半无害, 但 cargo 会把 tests/ benches/ examples/ 下的每个 .rs【自动发现】
    # 为独立编译目标 —— 不需要任何引用就参与构建。于是早已删除的测试文件带着对已删字段的
    # 引用一起炸, 而报错指向一个 git 和工作区里都找不到的文件。
    #
    # 做法: 拿刚传上去的那个包自身的清单当「应当存在的集合」, 剪枝遍历远程目录删掉集合外的。
    # ★ 清单直接来自那个包 (tar -tzf), 与实际传输内容【必然】一致, 不引入第二份要人工同步的规则。
    # ★ 清理作用域 == 同步作用域 (同一份排除清单): 我们从不同步的东西, 也就不管它的生死。
    # ★ 只删文件不删目录: 空目录无害, 且天然堵死「误删整个 target」这类不可逆后果。
    local edLit efLit
    edLit="$(for d in $RBUILD_EXCLUDE_DIRS;  do printf "'%s'," "$d"; done)"; edLit="${edLit%,}"
    efLit="$(for f in $RBUILD_EXCLUDE_FILES; do printf "'%s'," "$f"; done)"; efLit="${efLit%,}"

    # ⚠️ 解压必须走【管道】喂 tar 的 stdin, 不能 `tar -xzf <具名文件>` —— 后者会稳定复现
    #    "<某个源文件>: Refusing to overwrite archive: No error"。机制 (libarchive 的
    #    S_ISREG 分支): bsdtar 打开具名归档时 stat() 它自己, 把 dev+ino 记成 skip_file;
    #    解压时任何目标路径的 dev+ino 撞上它就报这个错。C:\Windows\Temp 文件 churn 高,
    #    NTFS 的 MFT 记录被快速复用, 偶尔就会撞上 —— 与文件名/内容无关。
    #    stdin 是管道 (S_ISFIFO) 不是具名文件, skip_file 的前置条件就不成立。
    # ⚠️ cmd 内置的 type 不认正斜杠, 必须转反斜杠。且 type 失败时管道另一端的 tar 拿到空
    #    stdin 照样 exit 0, 而 $LASTEXITCODE 只反映最后一个命令 —— 故解压后必须验目录非空。
    local rtgzBS="${rtgz//\//\\}"
    local ps="New-Item -ItemType Directory -Force -Path '$rdir' | Out-Null; "
    ps="$ps cmd /c type '$rtgzBS' | tar -xz -C '$rdir'; \$c=\$LASTEXITCODE; "
    # ★ 解压后的实证校验 —— 上面那段注释说的「type 失败时 tar 拿到空 stdin 照样 exit 0」
    #   光靠 \$c 是判不出来的。抽查清单里的前若干条是否真的落地: 若 scp 成功而 type 空转,
    #   \$k 仍会被 tar -tzf 正常填满(它直接读归档文件, 与解压成败无关), prune 也照跑不误,
    #   最后 exit 0 打印「已同步 (9.0M)」—— 而编译机上的源码停在上一次, 随后编出旧代码。
    ps="$ps if (\$c -eq 0) { "
    ps="$ps   \$probe=@(); tar -tzf '$rtgz' | Select-Object -First 40 | ForEach-Object { "
    ps="$ps     \$q=\$_ -replace '^\./','' -replace '/','\'; "
    ps="$ps     if (\$q -and -not \$q.EndsWith('\')) { \$probe += \$q } }; "
    ps="$ps   \$miss=@(\$probe | Where-Object { -not (Test-Path -LiteralPath (Join-Path '$rdir' \$_)) }); "
    ps="$ps   if (\$probe.Count -gt 0 -and \$miss.Count -gt 0) { "
    ps="$ps     [Console]::Error.WriteLine(\"解压未生效: 抽查 \$(\$probe.Count) 条, \$(\$miss.Count) 条不存在 (例: \$(\$miss[0]))\"); "
    ps="$ps     Remove-Item '$rtgz' -Force -EA SilentlyContinue; exit 3 } }; "
    ps="$ps if (\$c -eq 0) { "
    ps="$ps   \$k=[Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase); "
    ps="$ps   tar -tzf '$rtgz' | ForEach-Object { \$p=\$_ -replace '^\./','' -replace '/','\'; "
    ps="$ps     if (\$p -and -not \$p.EndsWith('\')) { [void]\$k.Add(\$p) } }; "
    ps="$ps   if (\$k.Count -gt 0) { "
    # TrimEnd 必须同时给两种斜杠: \$rdir 用的是正斜杠风格 (scp/ssh 路径惯例), 而
    # remote-build.ps1 那边是反斜杠, 照抄 TrimEnd('\') 对这边【不起作用】。bash 侧已在
    # WIND_BUILD_ROOT 入口规范化过, 这里是第二道 —— 切错一个字符就是整棵树被当残留删掉。
    ps="$ps     \$ed=@($edLit); \$ef=@($efLit); \$rl='$rdir'.TrimEnd('\','/').Length+1; "
    ps="$ps     \$pr=0; \$st=[Collections.Stack]::new(); \$st.Push('$rdir'); "
    ps="$ps     while (\$st.Count) { \$d=\$st.Pop(); "
    ps="$ps       foreach (\$e in (Get-ChildItem -LiteralPath \$d -Force -EA SilentlyContinue)) { "
    ps="$ps         if (\$e.PSIsContainer) { if (\$ed -notcontains \$e.Name) { [void]\$st.Push(\$e.FullName) } } "
    ps="$ps         else { \$sk=\$false; foreach (\$m in \$ef) { if (\$e.Name -like \$m) { \$sk=\$true; break } }; "
    ps="$ps           if (-not \$sk) { \$r=\$e.FullName.Substring(\$rl); "
    ps="$ps             if (-not \$k.Contains(\$r)) { Remove-Item -LiteralPath \$e.FullName -Force -EA SilentlyContinue; \$pr++ } } } } }; "
    ps="$ps     if (\$pr) { \"    [$label] 清理残留 \$pr 个\" } } }; "
    ps="$ps Remove-Item '$rtgz' -Force -EA SilentlyContinue; exit \$c"

    rbuild_ps "$ps" || { err "  远程解压 $label 失败"; return 1; }
    gray "  - $label 已同步 ($mb)"
    return 0
}

# ---------- 产物回传 ----------
# 远端整目录打包 → scp → 本地解压到产品根。
# $3=mirror: 解压前先清掉本机同名目录。
# ★ 上行 prune 花了十行论证「tar 解压是叠加不是镜像」, 下行【同样成立】却一直没人管:
#   本机 build/ 里的陈旧产物(上次 WIND_BUILD_LOCAL=1 交叉编的、或已从构建里删掉的模块)
#   会原地存活, 随后被 p1 原样推到靶机 —— 上行的「报错指向一个远端不存在的文件」在这边
#   变成「部署了一个远端根本没编的文件」。build[_dev]/ 完全由构建产出, 清空是安全的。
# ⛔ dist/ 不镜像: 那里攒的是历次打包成品, 不是本次构建的产出。
# ★ 清空放在【下载成功之后】, 失败时本机产物原样保留, 不会两头空。
rbuild_fetch_dir() {
    local packPath="$1" label="$2" mirror="${3:-}" rnd tgz rtgz
    rnd="$(tr -dc 'a-z0-9' < /dev/urandom 2>/dev/null | head -c 6)"; rnd="${rnd:-$$}"
    rtgz="C:/Windows/Temp/wi-out-$rnd.tar.gz"
    tgz="$(mktemp -t "wi-out-$rnd-XXXX.tar.gz")"; RBUILD_TMPFILES+=("$tgz")

    # 远端目录不存在时 tar 会失败 —— 对 dist/ 这类「可能没产出」的目录要能容忍, 由调用方判读。
    rbuild_ps "if (-not (Test-Path '$WIND_BUILD_ROOT/$packPath')) { exit 9 }; \
tar -czf '$rtgz' -C '$WIND_BUILD_ROOT' '$packPath'; exit \$LASTEXITCODE" >/dev/null 2>&1
    local rc=$?
    [ "$rc" = 9 ] && { rm -f "$tgz"; return 9; }
    [ "$rc" = 0 ] || { err "  远程打包 $label 失败"; rm -f "$tgz"; return 1; }

    if ! rbuild_scp "$WIND_BUILD_REMOTE:$rtgz" "$tgz" "回传 $label"; then
        err "  scp 下载 $label 失败"; rm -f "$tgz"; return 1
    fi
    local mb; mb="$(fsize "$tgz")"
    if [ "$mirror" = mirror ] && [ -d "$PRODUCT_ROOT/$packPath" ]; then
        rm -rf "${PRODUCT_ROOT:?}/$packPath" || { err "  清理本机 $packPath/ 失败"; rm -f "$tgz"; return 1; }
    fi
    tar -xzf "$tgz" -C "$PRODUCT_ROOT" || { err "  本地解压 $label 失败"; rm -f "$tgz"; return 1; }
    rm -f "$tgz"
    rbuild_ps "Remove-Item '$rtgz' -Force -EA SilentlyContinue" >/dev/null 2>&1 || true
    gray "  - $label 已回传 ($mb)"
    return 0
}

# 按命令回传该模块的产物。
rbuild_fetch_artifacts() {
    local cmd="$1" outName="$2" localOut="$3"
    local files; files="$(rbuild_artifacts_for "$cmd")"

    if [ -n "$files" ]; then
        mkdir -p "$localOut"
        local got=() missed=() f
        for f in $files; do
            if rbuild_scp "$WIND_BUILD_REMOTE:$WIND_BUILD_ROOT/$outName/$f" "$localOut/$f" "回传 $f"; then
                got+=("$f")
            else
                missed+=("$f")
            fi
        done
        [ "${#got[@]}" -eq 0 ] && { err "  未取回任何产物 —— 编译机 $outName/ 里没有 $files"; return 1; }
        gray "  - ${got[*]}"
        # 部分缺失必须说出来: 静默跳过会让本机留着【旧】产物, 而你以为部署的是刚编的那份。
        [ "${#missed[@]}" -gt 0 ] && warn "  ! 未取回: ${missed[*]} —— 本机这些文件仍是上一次的版本"
        return 0
    fi

    local sub; sub="$(rbuild_subdir_for "$cmd")"
    local packPath="$outName" label="整目录 $outName/"
    [ -n "$sub" ] && { packPath="$outName/$sub"; label="$outName/$sub/"; }
    rbuild_fetch_dir "$packPath" "$label" mirror || return 1

    # 8/9 的成品在 dist/ —— 不取它等于远程打的包留在编译机上。
    local extra; extra="$(rbuild_extra_dirs_for "$cmd")"
    local e rc
    for e in $extra; do
        rbuild_fetch_dir "$e" "$e/"; rc=$?
        [ "$rc" = 9 ] && warn "  ! 编译机上没有 $e/ —— 打包步骤可能未产出任何东西"
        [ "$rc" = 1 ] && return 1
    done
    return 0
}

# ---------- 主流程 ----------
# dev.sh 的 dispatch 在命中转发清单时调用这里, 整条命令交给编译机的 dev.ps1 执行。
rbuild_run() {
    local raw="$1" cmd t0 rc
    cmd="$(rbuild_canon_cmd "$raw")"
    rbuild_require_ready "$raw" || return 1
    t0=$SECONDS

    if [ -n "$WIND_BUILD_SLOT_ACTIVE" ]; then
        say "\n========== 远程构建 '$cmd' @ $WIND_BUILD_REMOTE [槽位 $WIND_BUILD_SLOT_ACTIVE] =========="
        gray "  → $WIND_BUILD_ROOT (worktree 专用; 各带一份 target/, 用完记得删)"
    else
        say "\n========== 远程构建 '$cmd' @ $WIND_BUILD_REMOTE =========="
    fi
    rbuild_trap_on
    rbuild_lock || { rbuild_trap_off; return 1; }
    # 锁的获取与释放收口在这一层, 主体的任何一条失败路径都不会把锁留在编译机上 ——
    # 否则一次中断就把它锁到 30 分钟陈旧判据生效为止, 另一条路 (Windows 本机) 也跟着卡。
    _rbuild_run_body "$cmd"; rc=$?
    rbuild_cleanup
    rbuild_trap_off

    if [ "$rc" -eq 0 ]; then
        say "\n远程构建完成 ($((SECONDS - t0))s)"
        case "$cmd" in
            8|8s|9|9s)
                warn "  ⚠️ 回传的安装包/便携包【未签名】, 只能自测安装流程, 不能发版。"
                gray "     发版: dev.sh 1 → dev.sh stage → scp 到本机 → dev.ps1 unstage → sign 8s / 9s" ;;
        esac
    fi
    return "$rc"
}

_rbuild_run_body() {
    local cmd="$1" outName localOut sib s rc
    outName="$(rbuild_outdir_name "$cmd")"
    localOut="$PRODUCT_ROOT/$outName"

    gray "[1/3] 同步源码 → $WIND_BUILD_ROOT"
    rbuild_sync_tree "$PRODUCT_ROOT" "$WIND_BUILD_ROOT" "WindInput" || return 1
    sib="$(rbuild_siblings_for "$cmd")"
    local sibRoot; sibRoot="$(dirname "$WIND_BUILD_ROOT")"
    for s in $sib; do
        rbuild_sync_tree "$(dirname "$PRODUCT_ROOT")/$s" "$sibRoot/$s" "$s" || return 1
    done

    gray "[2/3] 执行 dev.ps1 $cmd"
    # ⚠️ WIND_NO_REMOTE=1 是防二次转发的哨兵: 编译机上若存在 build.local.ps1, dev.ps1 会
    #    把命令再转发到第三台机器去。当前编译机没有那个文件, 但这属于「不设就不知道哪天
    #    会中招」的那类, 故显式钉死。
    # ★ 前置条件要显式判死, 不能靠 $LASTEXITCODE 兜底: Set-Location 失败与 & 找不到脚本
    #   都是【非终止错误】—— PowerShell 写完 stderr 继续往下走, 而本次会话没跑过任何 native
    #   命令时 $LASTEXITCODE 是 $null, `exit $null` 退 0。于是「dev.ps1 压根没被调用」会被
    #   报成构建成功, 随后把编译机上【上一次】的 build/ 整个盖回本机。
    rbuild_ps "\$env:WIND_NO_REMOTE='1'; \$LASTEXITCODE=0; \
if (-not (Test-Path '$WIND_BUILD_ROOT/scripts/dev.ps1')) { [Console]::Error.WriteLine('编译机上找不到 scripts/dev.ps1 —— 源码同步没生效?'); exit 66 }; \
Set-Location -LiteralPath '$WIND_BUILD_ROOT' -EA Stop; \
& '$WIND_BUILD_ROOT/scripts/dev.ps1' $cmd; exit \$LASTEXITCODE"
    rc=$?
    [ "$rc" -eq 66 ] && { err "\n编译机上没有 $WIND_BUILD_ROOT/scripts/dev.ps1"; return 1; }
    [ "$rc" -eq 0 ] || { err "\n编译机上 'dev.ps1 $cmd' 失败 (退出码 $rc)"; return "$rc"; }

    gray "[3/3] 回传产物 → $localOut"
    rbuild_fetch_artifacts "$cmd" "$outName" "$localOut" || return 1
    return 0
}

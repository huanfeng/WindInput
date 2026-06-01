#!/usr/bin/env bash
# setup_signing.sh — 命令行创建自签 Code Signing 证书并 import 到 login keychain.
# 用 openssl + security cli, 完全跳过 Keychain Access GUI.
#
# 输出: 一个名为 "WindInput Dev" 的可用于 codesign 的本机证书.
# 用法: scripts_mac/deploy/setup_signing.sh        # 创建
#       scripts_mac/deploy/setup_signing.sh check  # 仅检查现状
#       scripts_mac/deploy/setup_signing.sh remove  # 删掉证书
set -uo pipefail

CERT_NAME="WindInput Dev"
WORK_DIR="${TMPDIR:-/tmp}/wind_input_cert"
CFG_FILE="$WORK_DIR/openssl.cnf"
KEY_FILE="$WORK_DIR/cert.key"
CRT_FILE="$WORK_DIR/cert.crt"
P12_FILE="$WORK_DIR/cert.p12"
P12_PASS="windinput-dev"

bold() { printf "\n\033[1m==> %s\033[0m\n" "$*"; }
info() { printf "  %s\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

# purge_cert — 删除所有同名证书, 带次数上限防死循环。
# 关键: 残留可能在 System keychain (如某次 add-trusted-cert 部分成功留下 cert),
# 普通 delete-certificate 删不掉受保护的 System keychain 条目 → 原先 while 会死循环。
# 这里 login + System (sudo) 都试, 且 20 次封顶。
purge_cert() {
    local i=0
    while security find-certificate -c "$CERT_NAME" >/dev/null 2>&1; do
        security delete-certificate -c "$CERT_NAME" >/dev/null 2>&1
        sudo security delete-certificate -c "$CERT_NAME" /Library/Keychains/System.keychain >/dev/null 2>&1
        i=$((i + 1))
        if [[ $i -ge 20 ]]; then
            err "清理 \"$CERT_NAME\" 超过 20 次仍残留, 放弃 (手动检查 login/System keychain)"
            break
        fi
    done
}

CMD="${1:-create}"

# ---------------- check ----------------
if [[ "$CMD" == "check" ]]; then
    bold "查询当前 codesigning identity"
    security find-identity -v -p codesigning
    exit 0
fi

# ---------------- grant ----------------
# 授权 codesign 非交互访问私钥 (set-key-partition-list)。import -A 仍不足以让
# codesign 在无 GUI 授权上下文 (如 ssh 部署会话) 访问私钥 → errSecInternalComponent;
# 设 partition-list 后 apple/codesign 工具可免授权使用, 无头 ssh 部署才能用证书签名。
if [[ "$CMD" == "grant" ]]; then
    KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
    bold "授权 codesign 非交互访问 \"$CERT_NAME\" 私钥"
    printf "  输入此 Mac 的登录密码 (解锁 login keychain): "
    read -rs PW; echo
    if ! security unlock-keychain -p "$PW" "$KEYCHAIN"; then
        err "解锁 login keychain 失败 (密码错?)"; unset PW; exit 1
    fi
    if security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PW" "$KEYCHAIN" >/dev/null 2>&1; then
        bold "成功: codesign 现可非交互访问私钥 (ssh 无头部署可用此证书签名)"
    else
        err "set-key-partition-list 失败"
    fi
    unset PW
    exit 0
fi

# ---------------- remove ----------------
if [[ "$CMD" == "remove" ]]; then
    bold "删 \"$CERT_NAME\" 证书 (所有同名条目, 含 System keychain)"
    purge_cert
    # 删 trust 设置 (admin trust 与 user trust)
    sudo security remove-trusted-cert -d -p codeSign 2>/dev/null || true
    bold "remove 完成"
    exit 0
fi

# ---------------- create ----------------
command -v openssl  >/dev/null || { err "openssl 未安装"; exit 1; }
command -v security >/dev/null || { err "security cli 未安装"; exit 1; }

# 清理已有同名证书 (踩过的坑: 失败的 import 也会留条目, 重复后 codesign ambiguous;
# 残留可能在 System keychain 需 sudo 删, 普通 delete 删不掉会死循环, 见 purge_cert)
if security find-certificate -c "$CERT_NAME" >/dev/null 2>&1; then
    bold "发现已有 \"$CERT_NAME\" 证书, 清掉重建"
    purge_cert
fi

mkdir -p "$WORK_DIR"
chmod 700 "$WORK_DIR"

bold "1. 生成 openssl 配置 (X509 extensions for code signing)"
cat > "$CFG_FILE" <<EOF
[ req ]
distinguished_name = req_distinguished_name
prompt             = no
x509_extensions    = v3_self

[ req_distinguished_name ]
CN = $CERT_NAME
O  = WindInput Local
C  = CN

[ v3_self ]
basicConstraints       = critical, CA:false
keyUsage               = critical, digitalSignature
extendedKeyUsage       = critical, codeSigning
subjectKeyIdentifier   = hash
EOF
info "$CFG_FILE"

bold "2. 生成 RSA 2048 私钥 + 自签 X509 证书 (有效期 10 年)"
openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$KEY_FILE" -out "$CRT_FILE" \
    -days 3650 -config "$CFG_FILE" -sha256 2>&1 | tail -3 | sed 's/^/  /'
[[ -f "$CRT_FILE" ]] || { err "openssl 生成失败"; exit 1; }

bold "3. 打成 PKCS12 (.p12, legacy 格式) 以便 security import"
# OpenSSL 3.x 默认 PBES2 (PBKDF2 + AES) macOS security import 不识别, 必须 -legacy
# 回退老的 PKCS12 RC2-40 + SHA-1 (本地用安全够了)。
# 但 macOS 自带 LibreSSL 不认识 -legacy 标志 (会报错且不生成 p12), 且其默认就是老格式 →
# 仅对 OpenSSL 3.x 加 -legacy。不加引号: 空时不产生空参数 (兼容 bash 3.2 + set -u)。
P12_LEGACY=""
if openssl version 2>/dev/null | grep -qi "^OpenSSL 3"; then
    P12_LEGACY="-legacy"
fi
openssl pkcs12 -export $P12_LEGACY -inkey "$KEY_FILE" -in "$CRT_FILE" \
    -out "$P12_FILE" -name "$CERT_NAME" -passout pass:"$P12_PASS" 2>&1 | tail -3 | sed 's/^/  /'
[[ -f "$P12_FILE" ]] || { err "pkcs12 生成失败 (openssl 版本/参数不兼容?), 终止"; exit 1; }

bold "4a. unlock login keychain (会弹一次密码框)"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
security unlock-keychain "$KEYCHAIN" || {
    err "解锁失败. 请手动跑: security unlock-keychain ~/Library/Keychains/login.keychain-db"
    exit 1
}

bold "4b. import 到 login keychain (允许 codesign 直接用)"
# -T /usr/bin/codesign: 把 codesign 加入私钥 ACL, 后续 codesign 不再弹框
# -A: 允许所有应用使用此私钥 (开发期方便, 否则每次 codesign 都要点 Always Allow)
security import "$P12_FILE" -k "$KEYCHAIN" \
    -P "$P12_PASS" -A 2>&1 | sed 's/^/  /'

bold "5. 把证书加为 trusted code-signing root (这一步要 sudo)"
# 没有 trust, codesign 用上后系统仍判 CSSMERR_TP_NOT_TRUSTED 等同 ad-hoc, IME 注册照样拒
# -d: 加到 admin trust domain (System keychain)
# -r trustRoot: 当 root CA trust
# -p codeSign: 仅信任此 cert 的 code signing 用途, 不开成全能 root
sudo security add-trusted-cert -d -r trustRoot -p codeSign \
    -k "/Library/Keychains/System.keychain" "$CRT_FILE" 2>&1 | sed 's/^/  /'

bold "6. 验证 identity 可用 (Valid identities only 段应出现 \"$CERT_NAME\")"
security find-identity -v -p codesigning | sed 's/^/  /'

if security find-identity -v -p codesigning | grep -q "\"$CERT_NAME\""; then
    bold "成功"
    info "现在跑:"
    info "  SIGN_IDENTITY=\"$CERT_NAME\" scripts_mac/build/app.sh"
    info "  sudo scripts_mac/deploy/install_app.sh --uninstall"
    info "  sudo SIGN_IDENTITY=\"$CERT_NAME\" scripts_mac/deploy/install_app.sh"
    info "  swift scripts_mac/test/list_input_sources.swift"
else
    err "证书仍未 valid. 看上面 add-trusted-cert 输出"
    exit 1
fi

rm -rf "$WORK_DIR"

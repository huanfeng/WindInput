#!/usr/bin/env python3
"""检查 PE 文件是否带 Authenticode 签名 —— 不需要 signtool，可在 Linux 上跑。

用途是 CI 的防误发门禁：release.yml 的 publish job 在 ubuntu 上汇总两平台产物，
那里没有 signtool，但仍需如实报出「这批 Windows 产物签没签」，免得未签名的包被
当成正式版发出去。

判据是 IMAGE_DIRECTORY_ENTRY_SECURITY（数据目录索引 4）非零，也就是「有没有证书表」。
刻意【不】验证签名有效性 —— 那要走证书链，Linux 上做不到，而且这道门禁要防的是
「根本没签」，不是「签了但证书有问题」。后者由本机的 `dev.ps1 verify-sign` 负责。

  用法: check-signed.py <文件或目录> ...
  退出: 0 = 全部已签名; 1 = 有未签名的; 2 = 参数错误

⚠️ zip 不是 PE，Authenticode 签不了它。便携版 zip 会被跳过并单独提示 —— 它的可信度
   靠包内每个 exe/dll 各自的签名，这个脚本看不到包内。
"""

import struct
import sys
from pathlib import Path

# CI 跑在 ubuntu（UTF-8）上，但本机 Git Bash / cmd 的控制台默认是 GBK，中文提示会变乱码。
# 3.7+ 可以直接重配流编码，失败也不影响判定结果，故不做硬要求。
for _s in (sys.stdout, sys.stderr):
    try:
        _s.reconfigure(encoding="utf-8")
    except (AttributeError, OSError):
        pass

# Optional Header 里数据目录的起点：PE32 在可选头 +96，PE32+ 在 +112
DD_OFFSET = {0x10B: 96, 0x20B: 112}
SECURITY_INDEX = 4


def cert_table(path: Path):
    """返回 (offset, size)；不是 PE 或解析不下去则返回 None。"""
    try:
        data = path.read_bytes()
    except OSError:
        return None
    if len(data) < 0x40 or data[:2] != b"MZ":
        return None

    (pe_off,) = struct.unpack_from("<I", data, 0x3C)
    if pe_off + 24 + 112 > len(data) or data[pe_off : pe_off + 4] != b"PE\0\0":
        return None

    opt_off = pe_off + 24
    (magic,) = struct.unpack_from("<H", data, opt_off)
    if magic not in DD_OFFSET:
        return None
    dd_off = opt_off + DD_OFFSET[magic]

    # NumberOfRvaAndSizes 紧邻数据目录之前，必须 > 4 才有 Security 项
    (num_rva,) = struct.unpack_from("<I", data, dd_off - 4)
    if num_rva <= SECURITY_INDEX:
        return None

    return struct.unpack_from("<II", data, dd_off + SECURITY_INDEX * 8)


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2

    targets = []
    for arg in argv[1:]:
        p = Path(arg)
        if p.is_dir():
            targets += sorted(q for q in p.iterdir() if q.is_file())
        else:
            targets.append(p)

    signed, unsigned, skipped = [], [], []
    for f in targets:
        if f.suffix.lower() in (".sha256", ".json", ".toml", ".md", ".txt"):
            continue
        ct = cert_table(f)
        if ct is None:
            skipped.append(f.name)
        elif ct[0] and ct[1]:
            signed.append(f.name)
        else:
            unsigned.append(f.name)

    for n in signed:
        print(f"  已签名   {n}")
    for n in unsigned:
        print(f"  未签名   {n}")
    for n in skipped:
        print(f"  非 PE    {n}  (Authenticode 只认 PE; zip 内文件看不到)")

    if unsigned:
        print(f"\n{len(unsigned)} 个 PE 未签名")
        return 1
    if not signed:
        print("\n没有找到任何 PE 文件")
        return 1
    print(f"\n全部 {len(signed)} 个 PE 均带签名")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

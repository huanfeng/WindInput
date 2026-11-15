# Linux rpm 发布包（Fedora / openSUSE）

目前只发 `.deb`（见 [linux-port.md](linux-port.md) §9）。本文是 rpm 包的设计与落地记录。

## 1. 结论

**二进制不重新编译，只把 deb 同一棵安装树重新打成 rpm，再在 rpm 系发行版里实测。**

依据（2026-11 实测）：

| 发行版 | glibc | fcitx5 | 结果 |
| --- | --- | --- | --- |
| Rocky / Alma 9（+EPEL、CRB） | 2.34 | **仓库里没有** | 不是目标：用户装不了 fcitx5 |
| Fedora 41 | 2.40 | 5.1.10 / 5.1.13 | 用 Ubuntu 22.04 编出的 deb 二进制，三个 ELF 无缺失动态库，rpm 装 / 卸干净 |
| openSUSE Leap 15.6 | 2.38 | 5.1.1 | 同上 |

- 若在 Fedora / Leap 里独立构建，基线 glibc 反而更高（Fedora 当前在维护版本 ≥ 2.42），拿到 Leap 上跑不了；
  Ubuntu 22.04（2.35）已是最低基线，二进制本来就能在所有 rpm 系目标上运行。
- addon 按 fcitx5 5.0.14 头文件编译，5.1.x 上能加载（Deepin 25、Fedora 41、Leap 15.6 实测），
  所以不需要为 rpm 重编 addon。
- 这不是 `alien` 式的格式转换：rpm 的依赖、scriptlet、文件清单都是按 rpm 的规则写的，
  只是复用已经核对过的安装树（同一份 ELF 同时出现在 deb 与 rpm 里，反而少一个「两个包内容不一致」的变量）。

## 2. 结构

```
stage-linux.sh      构建服务 + 设置程序 + addon，组装安装树（deb / rpm 共用，Ubuntu 22.04 基线）
 ├─ build-deb.sh    + DEBIAN/ 元数据 → .deb
 └─ build-rpm.sh    + pkg/windinput.spec → .rpm（rpm 系容器里跑 rpmbuild）
verify-rpm.sh       发布前硬门禁（与 verify-deb.sh 同口径）
smoke-rpm.sh        干净发行版里真装一次（dnf / zypper 自动识别）
package-rpm.sh      本机入口：package-deb.sh 之后，用其安装树出 rpm 并在 Fedora + Leap 容器里核对
```

## 3. 包内容与 deb 的差异

- **addon 位置**：deb 在 `/usr/lib/<multiarch>/fcitx5`，rpm 系是 `%{_libdir}/fcitx5`（x86_64 / aarch64 都是 `lib64`）。
  spec 的 `%install` 把它挪过去。服务、设置程序、词库仍在 `/usr/lib/windinput/`（addon 里的路径写死，与 deb / Nix 一致）。
- **许可证**：rpm 带 `LICENSE`、`NOTICE.md`（`/usr/share/licenses/windinput/`），deb 目前没有。
- **版本**：`Version` 把预发布后缀的 `-` 换成 `~`（同 deb），`Release: 1`。
- **依赖**：链接期依赖（`libc.so.6(GLIBC_x)`、`libFcitx5Core.so.7`、`libstdc++`、`libxcb*`…）由 rpm 按 ELF 自动算；
  手写的只有 `fcitx5 >= 5.0.14`、`fontconfig`（运行时 `dlopen`）与 scriptlet 用的 `/usr/bin/pkill`
  （用文件依赖，绕开 Fedora `procps-ng` / openSUSE `procps` 的包名差异）。
  字体、剪贴板、portal、zenity、fcitx5 前端等写 `Recommends`，两家的包名都列，不存在的被忽略。
- **scriptlet**：`%post` 结束旧服务（规则同 deb 的 postinst）；`%postun` 仅在 `$1 = 0`（真卸载）时结束服务。
  mime / desktop / icon 数据库：Fedora 靠 file trigger，openSUSE 没有——**运行时**判断命令是否存在再刷新，
  不用 `%if 0%{?suse_version}`（宏在构建机上就定死，同一个 rpm 装到另一家时已无法分支）。
- **压缩**：`w6.xzdio`（词库占大头；Leap 15.x 的 rpm 4.14 也认）。不 strip、不抽 debuginfo、不跑 brp 脚本。

## 4. 门禁

- `verify-rpm.sh`：Arch、`Requires` 含 `fcitx5 >= 5.0.14` / `libFcitx5Core.so.7` 等、关键文件齐全（addon 必须在 `lib64`）、
  无 `DEBIAN/` 与 Debian 多架构目录残留、每个 ELF 机器类型与 glibc ≤ 2.35、词库体积、`compat.toml` 零规则、
  无 group/other 可写（按 rpm 记录的模式查，不看解包后受 umask 影响的结果）、`%post` / `%postun` 语法。
- `smoke-rpm.sh`：`dnf` / `zypper` 真装（依赖可解析）、`ldd` 无缺失、`rpm -V` 通过、卸载后 `/usr/lib/windinput` 不残留。
  `ldd` 看不到运行时 `dlopen` 的库，真正的验证仍是干净发行版上装一次、在 fcitx5 里添加输入法。

## 5. 不做（本期）

- 不签名、不建 yum / zypper 仓库：与 apt 现状一致，签名密钥与托管另立项。`dnf install ./x.rpm` 本地文件默认不校验签名；
  zypper 需 `--no-gpg-checks`（`smoke-rpm.sh` 已带）。
- RHEL / Rocky（无 fcitx5）、SUSE 的 fcitx5 前端包名差异不专门处理。
- 在线升级（设置程序目前只会 `pkexec apt-get`）：需要按 `/etc/os-release` 或 `rpm -q windinput` 判断包格式，
  改走 `pkexec dnf install -y <rpm>` / zypper，且 `latest-linux.json` 要加 `rpms.<arch>`。在 wind-setting 仓单独排期，
  首版 rpm 用户手动更新。

<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-05-29 | Updated: 2026-05-29 -->

# scripts_mac/ - macOS 构建/部署/诊断工具链

## Purpose

WindInput 在 macOS 上的全部辅助脚本：Go 服务与 IMKit `.app` 的构建、签名、本机/VM 安装部署、TIS 注册诊断。不参与主构建流程，供开发者手动调用。

按职责分四个子目录：

| 子目录 | 角色 |
|--------|------|
| `build/` | 构建产物（Go 服务、`.app` bundle、Wails 设置端） |
| `deploy/` | 本机安装/卸载、重部署、代码签名设置 |
| `test/` | TIS 注册状态诊断 |
| `vm/` | host→tart VM 一键远程部署 |

仓库根 `dev_mac.sh`（macOS 开发菜单）调用本目录的 `build/build.sh`、`build/app.sh`、`build/setting.sh`、`deploy/install_service.sh`、`deploy/install_app.sh`、`deploy/install_setting.sh`、`deploy/redeploy.sh`、`vm/deploy.sh`、`test/list_input_sources.swift`。「构建全部」(`1`)/「本机安装全部」(`i`)/「本机卸载全部」(`u`) 均含 `wind_setting`；`m <service|app|setting…>` 为单模块构建+安装（对位 Windows `dev.ps1` 的 `m[N]`）。

## Key Files

### build/

| File | Description |
|------|-------------|
| `build.sh` | macOS 端 Go 服务构建（对位 `build_all.ps1` 的可裁剪子集）：下载 rime-frost / rime-wubi / OpenCC 词库源到 `.cache/`，跑 `gen_unigram` + `dictgen` + `gen_opencc_dict`，产出 `build/{wind_input, data/}`（debug variant 产出 `build_debug/{wind_input_debug, data/}`）。子命令：`all` / `service` / `data` / `clean`，开关：`--debug` / `--universal`（arm64+x86_64，`CGO_ENABLED=0` 双架构交叉编译 + `lipo` 合并；亦可 `WIND_MAC_UNIVERSAL=1` 环境变量统一开关） |
| `app.sh` | macOS IMKit `.app` 打包：`swift build --product wind-input-app` + 拼 `Contents/{MacOS,Resources/lproj,Info.plist,PkgInfo}` + 拷 `AppIcon.icns`（应用图标，plist `CFBundleIconFile`）+ `codesign --options runtime`（默认 ad-hoc；`SIGN_IDENTITY="cert name"` 切真证书）。**版本贯通**：若仓库根有 `VERSION` 文件则注入 `CFBundleShortVersionString`/`CFBundleVersion`（CI 由 tag 写入，pkg.sh 据此取版本）。开关：`--debug` / `--no-sign` / `--universal`（`--arch arm64 --arch x86_64`，产物落 `.build/apple/Products/<Config>/`；或 `WIND_MAC_UNIVERSAL=1`）。输出 `wind_macos/build/WindInput.app` |
| `setting.sh` | 在 macOS 上构建 `wind_setting.app`（Wails+Vue 设置界面）：容忍 pnpm 11 ignored-builds 退出码、用 `vite build` 跳过 vue-tsc 门禁、把 `build/data` 拷进 `.app`。需 `build/build.sh data` 先产出词库。开关：`--universal`（`wails build -platform darwin/universal`，默认 `darwin/arm64`；或 `WIND_MAC_UNIVERSAL=1`）。输出 `wind_setting/build/bin/wind_setting.app` |
| `pkg.sh` | 把**三件套**（IME + Go 服务 + 设置 app）打成单个 **`.pkg` 安装器**（面向终端用户分发）：payload 把三件套 + 三个 `deploy/install_*.sh` 铺到系统暂存 `/Library/Application Support/WindInputInstaller/`，`pkgbuild --scripts` 挂 `pkg_resources/postinstall`，输出 `wind_macos/dist/WindInput-<版本>-macOS.pkg`（`-macOS` 后缀与 Windows `-Setup.exe`/`-Portable.zip` 在同一 Release 区分）。`--build` 先构建全部组件，`--universal`（`export WIND_MAC_UNIVERSAL=1` 下传子脚本 + distribution.xml `hostArchitectures="arm64,x86_64"`，与产物架构一致）。**公证预留**：配齐 `MACOS_DEVELOPER_ID_INSTALLER` + `MACOS_NOTARY_{APPLE_ID,PASSWORD,TEAM_ID}` 环境变量则 productbuild 后自动 `productsign` + `notarytool submit --wait` + `stapler staple`，否则保持 ad-hoc。安装 `sudo installer -pkg ... -target /` 或双击。**未公证版**首启需绕过 Gatekeeper，Tahoe 系统设置 UI 硬墙需公证才解 |
| `pkg_resources/postinstall` | `pkg.sh` 的 `pkgbuild --scripts` postinstall：installer 以 root 跑，但 IME/TIS/LaunchAgent 必须以登录 GUI 用户身份在其会话里装 → 用 `launchctl asuser $UID sudo -H -u $USER` 双重下沉，依次调 `install_service.sh --from`/`install_setting.sh --from`/`install_app.sh --from`；再把脚本拷到 `~/Library/Application Support/WindInput/installer/` 并在 `~/Applications/` 放「卸载清风输入法.command」；最后清系统暂存。需登录 console 用户（headless `installer` 亦然） |

### deploy/

| File | Description |
|------|-------------|
| `install_app.sh` | 把 `WindInput.app` 装到 per-user `~/Library/Input Methods/`（用户域，**不要 sudo**）：cp + 对 ad-hoc 产物原地去 hardened-runtime 重签（`codesign --force -s -` → 纯 ad-hoc `flags=0x2`，与 Fcitx5 一致；真证书产物则保留不降级）+ `lsregister -f -R` 刷 LS DB + `WindInput --register-input-source` 让 IME 自身进程调 TIS API。`--uninstall` 完整清理（删 .app + 清 HIToolbox plist + 缓存 + SIGTERM 重启 input agents），`--build` 先 build。**不再 `spctl --add`**（Tahoe 已移除该能力且证伪无必要）。能否进系统设置「可添加列表」取决于 `Info.plist` **不带** `tsInputModeDefaultStateKey`（带则 mode 注册即「已启用」却不落盘 → 被「+」列表过滤掉看不见，见 `wind_macos` Info.plist 说明） |
| `install_service.sh` | 把 Go 服务（`wind_input` + `data/`）装到 per-user `~/Library/Application Support/WindInput/service/`，生成并装 LaunchAgent `to.feng.windinput.service.plist`（`RunAtLoad`+`KeepAlive` 开机自启），`launchctl bootstrap/enable/kickstart` 启动，末尾验证 push socket + err.log。普通用户运行（不要 sudo）。开关：`--debug` / `--from <dir>`（指定源目录，部署脚本远程用）/ `--uninstall`。装时 cp 后 `codesign --force -s -` 原地 ad-hoc 重签（跨机同路径 redeploy 触发 amfi cdhash 缓存失配 → `OS_REASON_CODESIGNING`）+ `pkill -f "$INSTALL_ROOT/wind_input"` 清孤儿进程（bootout 只杀托管实例）。服务用 `exeDir/data` 定位词库，故二进制与 `data/` 必须同目录 |
| `install_setting.sh` | 把 `wind_setting.app`（Wails 设置界面）装到 per-user `~/Applications/`，去 quarantine + ad-hoc 重签 + `lsregister -f -R` 刷 LS DB，让 IME 菜单 "设置…" 经 `NSWorkspace` 按 bundleID `com.wails.wind_setting` 启动。普通用户运行（不要 sudo，非输入法不进 `/Library/Input Methods`）。开关：`--build`（先跑 `build/setting.sh`）/ `--from <dir>` / `--uninstall`。**绝不**对已删路径跑 `lsregister -u`（见 install_app.sh 血泪教训），卸载仅 rm + kill |
| `redeploy.sh` | 一键 build + uninstall + install + 自动验证签名链 + 跑 TIS list 看是否被收录。必须以普通用户跑（codesign 要 user login keychain），内部对需 root 步骤显式 invoke sudo。强制用 `SIGN_IDENTITY` 真证书签名（默认 "WindInput Dev"）。开发期 `.app` 主入口 |
| `setup_signing.sh` | 命令行创建本机 self-signed Code Signing 证书：openssl 生成 RSA + X509（codeSigning EKU）→ PKCS12（`-legacy`）→ import 到 login keychain → `add-trusted-cert` 加 trust。子命令: 默认创建 / `check` / `remove` |
| `setup_signing.md` | self-signed 证书的手动（Keychain Access GUI）创建步骤备份；macOS 26 Tahoe 上路径菜单与老版本不同时可作参考 |

### test/

| File | Description |
|------|-------------|
| `list_input_sources.swift` | 调 Apple `TISCreateInputSourceList` API 列系统已注册的输入源；诊断 IME 是否真被 macOS 收录到 TIS 数据库。默认只列非 `com.apple.*`（第三方），`--all` 列全部，`<keyword>` 模糊搜 |

### vm/

| File | Description |
|------|-------------|
| `deploy.sh` | host→VM 一键部署：宿主机构建产物 rsync 到目标 macOS 机后远程调 `deploy/install_service.sh`/`deploy/install_app.sh`。目标解析顺序：位置参数 > `SSH_TARGET` 环境变量 > 默认 `admin@$(tart ip ime-dev)`。远端 staging（`~/wind_deploy`）镜像仓库结构（`scripts_mac/{deploy,test}/`+`build/`+`wind_macos/build/`）让 install 脚本 `REPO_DIR`（`$SCRIPT_DIR/../..`）自对齐到 staging 根。开关：`--build` / `--debug` / `--service-only` / `--app-only` / `--setting`（加部署 `wind_setting.app`）/ `--setting-only`（只部署设置应用）/ `--uninstall`（远程卸载服务+.app+设置 并验证清除：LaunchAgent bootout、plist/service 目录、`.app`、`~/Applications/wind_setting.app`、TIS 残留条目；不需本地产物，可叠加 `--service-only`/`--app-only`/`--setting-only` 限定范围）。设置应用默认不部署（opt-in），远程调 `install_setting.sh`（普通用户） |

## Usage

### build/build.sh

```bash
scripts_mac/build/build.sh             # 全量: 下载词库 + 构建服务 + 准备 data
scripts_mac/build/build.sh service     # 仅构建 Go 服务
scripts_mac/build/build.sh data        # 仅词库下载 + 准备 data
scripts_mac/build/build.sh --debug     # debug variant (build_debug/)
scripts_mac/build/build.sh clean       # 清 build/ 与 build_debug/
```

输出：release `build/{wind_input, data/}`；debug `build_debug/{wind_input_debug, data/}`。

### `.app` 工具链 (build/app.sh + deploy/*)

```bash
# 一次性: 建本机自签证书 (会弹 2 次密码框: keychain 解锁 + sudo add-trusted-cert)
scripts_mac/deploy/setup_signing.sh

# 之后每次改了 wind_macos/ 代码:
SIGN_IDENTITY="WindInput Dev" scripts_mac/deploy/redeploy.sh
```

`redeploy.sh` 流程: 验证 identity → 解锁 keychain + partition list → `build/app.sh` → `deploy/install_app.sh --uninstall` → `deploy/install_app.sh`（用户域 ~/Library，无 sudo；cp + lsregister + IME 自身 `--register-input-source`）→ 验证签名 + `test/list_input_sources.swift` → ✓/✗ 总结。

单独跑各步：

```bash
SIGN_IDENTITY="WindInput Dev" scripts_mac/build/app.sh        # 仅 build
scripts_mac/deploy/install_app.sh                            # 装到 ~/Library (用户域, 无 sudo)
scripts_mac/deploy/install_app.sh --uninstall

swift scripts_mac/test/list_input_sources.swift              # 第三方 IME
swift scripts_mac/test/list_input_sources.swift --all        # 全部
swift scripts_mac/test/list_input_sources.swift wind         # 模糊搜
```

**已知限制 (macOS 26 Tahoe)**: self-signed cert + Personal Team Apple Development 都过不了 macOS 26 IME 注册校验，`TISRegisterInputSource` 返回 `OSStatus=0` 但 silent 不入库。需要 Apple Developer Distribution + Notarization 才能让 IME 出现在 系统设置 → 键盘 → 输入法 列表里。完整踩坑过程见 `../docs/design/macos-imkit-plan.md` "踩坑记录"。

### deploy/install_service.sh

```bash
scripts_mac/deploy/install_service.sh             # 装 build/ 的 release 产物
scripts_mac/deploy/install_service.sh --debug     # 装 build_debug/ 的 debug 产物
scripts_mac/deploy/install_service.sh --from <dir># 从指定目录装 (内含 wind_input + data/)
scripts_mac/deploy/install_service.sh --uninstall # 卸载服务 (保留用户数据)
```

### vm/deploy.sh

```bash
scripts_mac/vm/deploy.sh                       # rsync + 远程装 (服务 + .app)
scripts_mac/vm/deploy.sh --build               # 先在宿主机 build 再部署
scripts_mac/vm/deploy.sh --service-only        # 只部署 Go 服务
scripts_mac/vm/deploy.sh --app-only            # 只部署 .app
scripts_mac/vm/deploy.sh admin@192.168.64.9    # 显式指定目标
SSH_TARGET=ime-vm scripts_mac/vm/deploy.sh     # 用 ssh 别名
scripts_mac/vm/deploy.sh --uninstall           # 远程卸载 (服务+.app) 并验证清除
scripts_mac/vm/deploy.sh --uninstall --service-only  # 只卸 Go 服务
```

## For AI Agents

### Working In This Directory

- 所有 bash 脚本以 `set -euo pipefail` 开头；调用 `go run` 前 `cd "$REPO_DIR/wind_input"`（仓库根本身不是 Go module）
- 脚本下沉两级（`scripts_mac/<子目录>/`），`REPO_DIR` 必须用 `$(cd "$SCRIPT_DIR/../.." && pwd)` 解析；跨子目录调用兄弟脚本走 `$REPO_DIR/scripts_mac/<子目录>/<名>`（不要用 `$SCRIPT_DIR/<名>`）
- 新增脚本按职责放进 `build/` / `deploy/` / `test/` / `vm/`，并在本表登记
- `build/build.sh` 词库下载到 `.cache/`（已被 `.gitignore` 排除），dictgen / unigram 输出到 `build/data/schemas/`

## Dependencies

### Internal
- `build/build.sh` 依赖 `wind_input/cmd/{gen_unigram,gen_opencc_dict}`、`wind_input/tools/dictgen`、仓库根 `data/`、`VERSION`
- `build/app.sh` / `deploy/install_app.sh` / `deploy/redeploy.sh` 依赖 `wind_macos/Package.swift`（含 `wind-input-app` target）+ `wind_macos/Sources/WindInputApp/Resources/{Info.plist, WindInput.entitlements, *.lproj/}`
- `build/setting.sh` 依赖 `wind_setting/`（Wails 工程）+ `build/data`
- `deploy/install_service.sh` 依赖 `build/build.sh` 的产物（`build/{wind_input, data/}`）；服务的词库定位逻辑见 `wind_input/pkg/config/paths.go`（`GetDataDir` = `exeDir/data`）
- `vm/deploy.sh` 依赖 `deploy/install_service.sh` + `deploy/install_app.sh` + `test/list_input_sources.swift`（rsync 到远端 staging 后调用）+ 构建产物（`build/`、`wind_macos/build/WindInput.app`）；默认目标解析需要宿主机装 `tart`
- `deploy/setup_signing.sh` 操作 `~/Library/Keychains/login.keychain-db` 与 `/Library/Keychains/System.keychain`（后者需 sudo）
- `test/list_input_sources.swift` 用 Apple Carbon `TextInputSources` API，无外部依赖

### External
- `build/build.sh` 需要 macOS + Go 1.24+ + `curl`
- `build/app.sh` / `deploy/install_app.sh` / `deploy/redeploy.sh` / `test/list_input_sources.swift` 需要 Xcode（含 `swift 5.9+`、`codesign`、`/usr/libexec/PlistBuddy`）
- `build/setting.sh` 需要 wails CLI（`go install github.com/wailsapp/wails/v2/cmd/wails`）+ pnpm
- `deploy/setup_signing.sh` 需要 `openssl 3+`（系统 libressl 不支持 `-legacy`，建议 `brew install openssl`）+ 用户 login keychain 已解锁

<!-- MANUAL: -->

# Windows 代码签名设计与接线

> 状态：**已接线，真证书全链路验证通过**（2026-09-07）。`dev.ps1 sign d1 d8s` 实跑：
> 远程编译 → 回传 → 本机用真证书签 5 个 PE → 本机打包 → 签安装包 → manifest 按签名后的
> 文件算 hash。6 个产物 `signtool verify /pa` 全部通过、Status=Valid、带 RFC3161 时间戳；
> 签名后的安装包可正确解包（85 个条目）；`latest-dev.json` 的 sha256/size 与实际文件逐位一致。
> **`signtool` 未要求交互输入 PIN**，非交互会话下直接签成，无需在客户端勾「记住 PIN」。
> 关联：`scripts/sign.ps1`、`scripts/sign.local.ps1.example`、`scripts/check-signed.py`、
> `scripts/dev.ps1`（五个接线点）、跨仓 `wind-installer/src/archive/reader.rs`、
> `.github/workflows/release.yml`。

> ⚠️ **本文与仓库中的所有脚本刻意不提任何签名服务商的名字。** 签名平台是可替换的外部
> 依赖，把它焊进公开仓库既没必要（换一家就要全仓改名），也不合适（对外暴露供应链细节）。
> 服务商相关的一切都落在 `scripts/sign.local.ps1` 里，而那个文件不入库。

## 0. 目标与非目标

**目标**：发布的 Windows 产物（5 个 PE + 安装包）带有效 Authenticode 签名，消除
SmartScreen 的「未知发布者」，并让 TSF DLL 在加固宿主里少一条被拒的理由。

**非目标**：不做 CI 无人值守签名 —— 第 3 节说明这在云签名下不成立，不是没做，是做不了。
也不做 EV 证书 / SmartScreen 信誉加速那一套。

## 1. 证书形态：云签名，不涉及任何物理硬件

私钥在服务商的 **HSM** 里，不可导出，也搬不进 Azure Key Vault / Google Cloud KMS 之类的
第三方 HSM。**没有 USB token，没有实体智能卡。** 本机能签名的全部条件只有一条：

> 证书出现在 `Cert:\CurrentUser\My`，且私钥可用。

本地客户端登录、建立会话后，证书就会被投射进 Windows 证书存储，`signtool` 按指纹取用它。
至于服务商用什么机制投射（多数是在 PC/SC 层注册一个**软件**读卡器，让系统走 Base Smart
Card Crypto Provider 访问远端密钥 —— 是软件模拟，不是硬件），对我们的脚本完全透明：
`scripts/sign.ps1` 不认识任何服务商，只认指纹。换一家服务商不需要改脚本一行。

两条硬约束由此而来，它们才是真正影响设计的部分：

1. **会话有时限**（本项目所用服务商是 2 小时），到期需重新做一次二次验证。
2. **需要一个本地客户端进程在跑**，它是证书投射的宿主。

部分服务商还提供 PKCS#11 库，可供 jsign / osslsigncode 走非 signtool 路线。本项目不用 ——
它同样依赖已建立的会话，换不来无人值守，只多一层间接。

## 2. 什么是秘密，什么不是

这一节是「不能泄露」这个要求的落点，也是整套方案里最容易做错的地方。

**不是秘密**（可以进仓库、进 CI 变量、贴进 issue）：

- **证书指纹 thumbprint** —— 它就是证书公钥部分的 SHA-1。每个签过名的文件里都带着它，
  用 `signtool verify /v` 就能从任何已发布的安装包里读出来。知道指纹只能指定
  「用哪张证书」，**不能拿它签任何东西**。
- 时间戳服务器地址、摘要算法。

**是秘密**（只存在于你本机与二次验证设备上）：

- 签名服务的账号 / 密码 / PIN
- **二次验证的 TOTP 种子** —— 这一条最要命。种子不是「一次性密码」而是「密码生成器」，
  拿到它等于永久接管这张证书。社区里那些「CI 自动签名」的方案（用 `SendKeys` 往客户端
  登录框灌验证码）无一例外要把种子存进 CI secrets，那等于把证书交出去。

因此 `scripts/sign.ps1` **只读 thumbprint 一项配置**，不接受也不存储任何口令。
配置文件 `scripts/sign.local.ps1` 仍然 gitignore，理由是「一机一份、随证书更换而变，
且承载服务商细节」，而不是「里面有口令」。

## 3. 为什么 CI 不能自动签名

不是取舍问题，是技术上不成立：

| 障碍 | 说明 |
|---|---|
| 需要人 | 建立会话要做二次验证，GitHub 托管 runner 上没人能完成 |
| 会话时限 | 即便手动开一次，2 小时也覆盖不了「随时可能触发的 tag 构建」 |
| 需要本地客户端 | 那是个桌面应用，headless runner 上跑不起来 |
| 没有 API | 服务商无 REST 签名接口（官方口径「计划中，无日期」） |
| 密钥不可迁移 | 私钥不能导出到 Azure Key Vault 等 CI 原生方案 |

社区的两类绕法都要一台**长期在线、有人定期开会话**的机器：Linux 侧用 Xvnc 容器跑客户端
再把 p11-kit socket 暴露出去；Windows 侧直接用自托管 runner。两者都只是把手动步骤从
「签名」挪到「续会话」，还多一台机器要维护 —— 而 2 小时的窗口让这个手动步骤比签名本身
还频繁。

**结论**：签名只在开发机上发生，但**编译不在**。发布产物由 CI 构建（构建环境只有一套，
本机与 CI 的工具链差异不会漏进发布包），开发机只做「签名 + 打包」这一段。第 6 节是完整
流程与配套的防误发门禁。

## 4. 签名默认关闭：`sign` 开关与配额

签名次数按月计费且有限。一次全构建要签 5 个 PE，加打包是 6 次 —— 若做成「配了证书就
自动签」，日常 `d1` 几天就能把一个月的额度烧光。所以**默认一次也不签**，必须在命令里
显式写 `sign`：

```
dev.ps1 1          全构建, 不签            0 次
dev.ps1 sign 1     全构建并签 5 个 PE      5 次
dev.ps1 8          出安装包, 不签          0 次
dev.ps1 sign 8     出安装包, 签 PE + 外壳  6 次
dev.ps1 sign 9s    紧接着出便携包          0 次（PE 上一步已签，按指纹识别后跳过）
```

`8`/`9` 在未签名时会打印一行「本次【未签名】(要签名: dev.ps1 sign 8)」。发版忘写 `sign`
是会一路走到用户手里的那种错误，值得每次都说一句。

**为什么是命令关键字而不是 `-Sign` 参数**：`dev.ps1` 的 `$Commands` 用了
`ValueFromRemainingArguments`，它会把 `-Sign` 一并吃进命令列表，而 switch 恒为 `$false`
—— **不报错、静默失效**。实测：

```
.\dev.ps1 1 -Sign   →   Commands = [1, -Sign]   Sign = False
```

**`sign` 是位置无关的开关**，扫描后从命令列表里摘掉，不参与按序执行，所以
`dev.ps1 sign 8` 与 `dev.ps1 8 sign` 完全等价。位置无关是刻意的：若把它当成「按序执行
的一条命令」，`dev.ps1 8 sign` 就会变成「先出未签名的包（`latest.json` 已按未签名的
hash 算好），再去签 `build\`」—— 签了个寂寞，而 manifest 里的 sha256 与实际发布的文件
对不上。摘成开关就不存在这种顺序陷阱。

**省配额靠幂等**：已签过的文件按**签名者指纹**识别并跳过（不是按「有没有签名」，理由见
第 4.1 节末）。所以 `sign 8` 之后再 `sign 9s`，便携包里的 PE 一次也不会重签。

## 4.1 五个接线点，与它们各自的顺序约束

`scripts/sign.ps1` 是实现，`dev.ps1` 在五处调用它（`Invoke-SignArtifacts`；没写 `sign`
时这五处整段不走）。每一处的位置都不是随意的，都由一个「会静默出坏包」的约束定死：

| 接线点 | 位置 | 早了 / 晚了会怎样 |
|---|---|---|
| `build[_dev]\` 下的 PE | `Do-Full` 末尾，`Verify-DistData` 之后 | **晚了**就被封进 Setup 的压缩块 / zip，再也签不到 |
| 同上（skip 模式补签） | `Do-Installer` 里，`pack.ps1` **之前** | 漏了则 `8s`/`d8s` 会打出「外壳签了、里面 5 个 PE 全裸」的包，且验签 Setup.exe 照样通过 —— 从外面完全看不出来。**实测踩过** |
| `uninstall.exe` | `Do-Installer` 里，`pack.ps1 -PrepOnly` 之后、`pack.ps1 -SkipPrep` **之前** | **早了**签的是随后会被 prep 改写的裸 stub；**晚了**它已在压缩块里。窗口只有这一格，见 4.2 |
| `Setup.exe` | `Do-Installer` 里，`pack.ps1` 之后、`New-UpdateManifest` **之前** | **晚了**则 `latest.json` 的 `sha256`/`size` 是签名前的旧值，在线升级校验全体失败 |
| 便携 zip 内容 | `Do-PortableZip` 里，`Compress-Archive` 之前 | zip 本身签不了，包内 PE 必须先签好 |

另外两点：

- **签名收口在 `Do-Full` 末尾，不散在各 `Build-*` 里**。`Invoke-BuildStagesParallel`
  会同时跑四路构建，四路各自签名就是四个 `signtool` 并发访问同一个签名会话，
  而云端会话不是并发安全的 —— 失败会是随机的。
### 编译在远程、签名在本机

这条路是**通的**，签名不必把编译拉回本机：

```
dev.ps1 sign 1     远程全构建 → 回传 build\ → 本机签 5 个 PE
dev.ps1 sign 8s    本机打包 + 签外壳（PE 已签，跳过）
```

（两步也可以连写成 `dev.ps1 sign 1 8s`。）

机制是：**转发出去的命令里不含 `sign`** —— 它在入口就被摘成开关了，编译机收到的是光秃秃的
`1`/`d1`/`m*`，于是只编译、不签名。这正是想要的，证书会话不该散到第二台机器上，编译机也
建立不了会话。产物回传到本机 `build[_dev]\` 之后，`Dispatch` 在转发返回处补签 —— 时机
等价于本机 `Do-Full` 末尾那一次（都是「产物齐全、尚未打包」）。

只有**打包类**（`8`/`9` 系列）在签名时强制本机：

- 远程打包会把**未签名的 PE 封进压缩块**，回传后本机再补签也够不着包内文件 —— 拿到手的是
  一个签名完好、内容全裸的安装包，且没有任何提示。
- 何况 `remote-build.ps1` 的回传只取 `build[_dev]\`，**`dist\` 根本不回传**，远程打的包
  留在编译机上（这与签名无关，是既有行为）。

判据是「本次是否真要签」而不是「有没有配过签名」：没写 `sign` 的日常打包照样走远程。

**幂等判据是签名者指纹，不是 `Status -eq 'Valid'`**。`Status` 问的是「这个签名可信吗」，
取决于证书链、CRL 能否联网、根是否受信任 —— 全是与「签没签过」无关的外部条件，离线一次
就会让所有文件被判成未签名而全部重签（在按次计费下这是直接的损失）。按指纹比对还顺带
管了换证书的情况：旧证书签的文件会被正确地重签，而不是被当成「已签好」留在包里。

## 4.2 卸载器：把 overlay 前移到打包期才签得了

`uninstall.exe` 一度是**签不了**的，原因不在签名，在它自己的数据流：品牌清单（manifest
+ logo）此前由**安装期在用户机器上**追加到卸载器尾部（`append_manifest_overlay`）。
Authenticode 要求证书表必须是文件的最后一段（`offset + size == 文件长度`），尾部多一个
字节这个等式就破了 —— 实测追加后 `signtool verify` 直接报 "No signature found"。只要装机
端还会改这个文件，构建机上签什么都白搭。

**解法是让那次修改不再发生在装机端**：overlay 的内容全部来自 `app.toml`，没有一个字节
依赖安装期（安装期真正产生的信息 —— 装了什么、装到哪 —— 早就落在注册表的 `Receipt` 与
ARP 的 `InstallLocation`，与 overlay 无关）。既然是静态数据，就该在构建机上烤进二进制：

```
prep-uninstaller           写版本信息 + 图标 + 追加 overlay  → 卸载器成为终态
  ↓  签名                  ← 窗口只有这一格
pack                       封进压缩块
  ↓
装机端                     只解压，逐字节还原签名后的文件
```

这正是 NSIS「两遍构建」和 Inno Setup `SignedUninstaller` 的同一条路子：**签名的 PE 离开
构建机之后就是只读的**。（另一派是 Inno 的 `unins000.dat` —— 把数据外置成独立文件。这里
用不上：外置的价值在于容纳会变的数据，而这里没有会变的数据，白白引入「文件丢了就卸不了」
的新失败模式。）

落地在三处，每处都带一个不能省的判据：

| 位置 | 做什么 |
|---|---|
| `wind-packer prep-uninstaller` | 新子命令。**幂等**：已带 overlay 就整段跳过 —— 判据必须挡在「写版本信息」和「追加 overlay」**两件事之前**，因为 `set_pe_version_info` 用 editpe 重写资源节，同样会毁签名 |
| `pack.ps1 -PrepOnly` / `-SkipPrep` | 拆成两次调用，只为给中间那次签名腾位置。`-PrepOnly` 有意把 `uninstall.exe` 留在源目录不清理 |
| `installer/steps.rs` 的 `AppendUninstallerOverlay` | 改为「自身已含 overlay 则跳过」。正常情况下这一步现在什么都不做，只为**老版打包器产出的旧包**保底（那里的卸载器仍是裸 stub，不补就读不到清单、启动即失败） |

⚠️ `pack.ps1` 每轮都从 `target\` 重新复制**未加工的** stub 覆盖上一轮的产物。加工不可逆
（写完版本信息又追加了 overlay），拿加工过的再加工一次会撞上幂等判据而静默沿用旧版本号。

判据实现是 `archive::has_manifest_overlay()`，走 `ArchiveReader::open` —— 它自带证书表
偏移解析（第 5 节），对已签名的文件同样判得准。回归测试 `signed_uninstaller_overlay_stays_readable`
遍历 8 种对齐余数，同时断言「读得回清单」与「判据仍为真」：前者失守则卸载器启动即失败，
后者失守则安装期会重复追加、把刚签的名毁掉，两条都是从外面看不出来的。

## 5. wind-installer：签名会打废自解压包（已修）

这是接线过程中挖出来的、优先级最高的一个坑。

安装包布局是 `[stub][压缩块][Header][Footer 16B]`，`ArchiveReader::open` 原本用
`SeekFrom::End(-16)` 从**物理末尾**读 Footer 校验 magic。而 Authenticode 把证书表
**追加在 PE 末尾**：

```
签名前: size=1195024  末 16 字节 = ...57494e44454e4400   ("WINDEND\0")  ✓
签名后: size=1196440  末 16 字节 = 39dbc8839121f206...   (证书表数据)   ✗
```

于是「安装包一签名就报 Invalid footer magic」。

修复是让 reader 解析 PE 的 `IMAGE_DIRECTORY_ENTRY_SECURITY`（数据目录索引 4）拿到证书表
的起始偏移，用它当作归档的逻辑末尾。两个必须记住的细节：

1. **Security 是全部 16 个数据目录项里唯一一个 `VirtualAddress` 存文件偏移而非 RVA 的**，
   可以直接 seek，不需要按节表换算。
2. **Footer 未必紧挨着证书表**。证书表要求 8 字节对齐，signtool 会先把原文件补 0..=7 字节
   再追加。真实的 Setup.exe 就补了 7 字节（21997457 % 8 == 1），Footer 的 magic 因此落在
   `cert_off-16` 往后偏 1 字节的位置。第一版修复漏了这一点，构造的测试全绿、真包照样打不开
   —— 只有「原大小恰为 8 的倍数」的包能开，其余七分之六随构建产物大小随机复现。

回归测试 `signed_installer_still_extractable` **遍历 8 种对齐余数**，正是为了不让这个坑
以七分之一的概率蒙混过关。

## 6. CI 侧：不签，但要拦住误发

`release.yml` 的 Windows job 产出的仍是未签名产物。为避免它被当成正式版发出去，
`publish` job 在建草稿 Release 前会**逐个检查 PE 的 Security 目录是否非空**
（`scripts/check-signed.py`，ubuntu 上纯 python 解析，不需要 signtool），并把结论如实
写进 Release 正文。

判据是「Security 目录项非零」而不是「签名有效」—— 后者需要验证证书链，在 ubuntu 上做不了，
而前者足以区分「签过」与「根本没签」，这正是这道门禁要防的。

正式发版流程因此是：

```
1. .\scripts\release.ps1 patch        五仓同步打 tag，CI 开始构建
2. 等 CI 跑完 —— 草稿 Release 就绪，Windows 产物未签名，正文顶着未签名横幅
3. 本机建立签名会话（客户端登录 + 二次验证）
4. .\scripts\release.ps1 sign-draft   拉 CI 产物 → 本机签名打包 → 覆盖草稿资产
5. 人工过目 Release Notes，点 Publish
```

第 4 步内部就是从前手工做的那几件事，区别只在于**编译不再发生于本机**：

```
拉 stage-windows artifact（build\ 散件 + 安装器三件套）
  → dev.ps1 unstage        还原；硬校验中转产物版本 == docs\VERSION
  → dev.ps1 sign 8s        签 5 个 PE → 打包 → 签外壳 → 出 manifest   消耗 6 次
  → dev.ps1 sign 9s        打便携包（包内 PE 已签，按指纹跳过）        消耗 0 次
  → dev.ps1 verify-sign    硬校验通过才上传
  → gh release upload --clobber，再摘掉正文的未签名横幅
```

### 6.1 中转的为什么是「散件」而不是成品

CI 已经产出了 Setup.exe 和 Portable.zip，看起来本机只要对它们补签就行 —— **不行**。

签名夹在打包**中间**（第 4.1 节的五个接线点）：PE 必须在被封进压缩块之前签。对成品补签
只签得到外壳，包内 5 个 PE 仍是全裸的，而 `signtool verify` 验 Setup.exe **照样通过** ——
从外面完全看不出来。这正是第 5 节记的那个实测缺陷。

所以 CI 额外产出一个**中转产物** `WindInput-Stage-<版本>.zip`（`dev.ps1 stage`），装的是
打包之前的散件：

| 内容 | 用途 |
|---|---|
| `build\` | 全构建产物；内容 == 安装内容，是安装包与便携包的共同上游 |
| `installer\` | wind-installer 的 stub / packer / uninstaller |
| `stage.json` | 版本号等清单，`unstage` 时硬校验 |

带上 `installer\` 是为了让本机**一行代码都不编译** —— `Do-Installer` 检测到三件套已在，
就给 `pack.ps1` 透传 `-SkipBuild`。

它单列一个 artifact 而不并进 `dist-*`：`publish` job 按 `pattern: dist-*` 汇总并原样上传到
Release 页面，中转产物混进去就会出现在用户看到的下载列表里。

⚠️ **版本号是这条流程唯一会静默出坏包的地方**。打包函数用 `$Version`（读 `docs\VERSION`）
拼产物文件名，而中转产物里的二进制版本号是 CI 按 tag 编进去的 —— 两者不一致就会打出
「文件名写 A、里面是 B」的包，全程无任何报错。故 `unstage` 在还原前硬校验，不一致即中断；
`sign-draft` 则先按 tag 对齐 `docs\VERSION` 再调它（对齐后那道校验依然有效：拉错 run 时
中转产物版本仍会与 tag 不符而被拦下）。

## 7. 已知取舍与未做的部分

**真证书已全链路验过**（见文首状态）。当初担心的 PIN 交互没有发生：非交互会话下
`signtool` 直接签成，不需要在客户端里勾「记住 PIN」。

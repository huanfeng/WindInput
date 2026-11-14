# 用户数据存储重设计：beta 发版规划

状态：已裁定（2026-10-16）：D1 选 C、D2 选 A、D3 选 §3.1 全部（第 2 条随 D1 取消）。日期：2026-10-15。
依据：`docs/design/user-data-storage-redesign.md`（下称「设计」）与 2026-10-15 的目标达成度审查。

## 1 现状

- main 已合入 P0–P5 并 push，随 `v0.127.0-beta.1` 打 tag（草稿，未发布）。核心目标「只复制用户目录就带走全部数据」已达成：用户词、临时词、草稿、词频、联想、短语、Shadow、快捷格式、常用字、字符集、补全、统计的真源都在 `<userdata>/data/`，运行期零 redb 读写，删掉 `state/` 可由 `data/` 重建（有守护测试，靶机实测）。
- **已发布的 v0.126.0 没有 §7.1 降级保护**（不认识 `meta.toml`）。下一版会让护栏、P1–P5 迁移、旧库改名同时进场，违反设计 §7「护栏先于破坏性变更」。
- 现有发版流水线对预发布的处理不完整：winget / Scoop 按版本号后缀跳过预发布，文档仓按后缀分通道；但 **`release-published.yml` 的 R2 同步不区分预发布**——发一个 beta 会改写 `latest.json`，全体正式版用户经应用内升级被推到 beta，且 R2 上一版正式安装包会被清理。`release.yml` 产出的草稿也不会自动勾选 pre-release。（两处均已修，见 §6。）

## 2 待裁定

### D1 回退安全（发布次序）

beta 用户回退到 0.126.0 时：旧版照常打开 `userdata.redb`；若它已被改名 `.legacy`，旧版新建空库，用户看到「数据全没了」；之后再升级，`meta.toml` 已是 5、不再重迁，回退期间在旧版写的数据丢失。

| 方案 | 做法 | 回退时用户看到 |
|---|---|---|
| **A（建议）** | 先发 0.126.1 正式版，只含降级保护（`meta.toml` 读到更高版本进只读）；beta 期间**不改名** redb，改名推迟到 0.127.0 正式版之后的版本 | 回到 0.126.1：只读模式，数据是升级前的；不分叉 |
| B | 不发 0.126.1；beta 不改名 redb | 回到 0.126.0：数据是升级前的，可继续写，但再升级时这段写入丢失 |
| C | 照现状 | 回到 0.126.0：空词库，需手工把 `.legacy` 改回 |

**裁定：C。** 回退步骤写进 beta 发布公告（§5.1）。

### D2 beta 的分发与升级

| 方案 | 做法 |
|---|---|
| **A（建议）** | 首个 beta 只走 GitHub 预发布 + 论坛帖，测试者手动下载安装；不进应用内升级、不进 winget / Scoop；beta 之间也手动更新。0.127.0 正式版发出后，beta 用户经 `latest.json` 自动收到 |
| B | 新增 beta 通道：R2 发 `latest-beta.json`，wind-setting 加「参与测试版」开关（新 `Channel::Beta`），beta 之间自动更新 |

两种都必须先修 R2 同步：预发布不写 `latest.json`、不清理上一版。

**裁定：A。**

### D3 beta 前修复范围

建议「必修」= 下面 §3.1 的全部；§3.2 进 beta 期间。

## 3 修复清单

### 3.1 beta 前必修

1. CI：`sync-to-r2` 对预发布（Release 勾了 pre-release 或版本号带 `-`）跳过 `latest*.json` 与旧版清理，只上传安装包；`release.yml` 草稿按版本号后缀自动勾 `prerelease`。
2. ~~按 D1：beta 构建不调用 `retire_legacy_db`~~（D1 裁定为 C，取消）。
3. 单写者锁失败（`device.rs` 的 `TryLockError::Error`，网络家目录上 `flock` 报错）退化为只读，而不是整个用户数据不可用。
4. 退出路径补折回：服务停止 / 注销 / SIGTERM 时调 `flush_user_data_before_exit`，并让它一并刷统计收集器。
5. 日志写失败（磁盘满、目录只读）：设计 §8 的「内存保留模式」未实现。beta 前至少做到：写失败登记到 `storageStatus.write_error`、设置页提示，并在设计 §8 改写为实际行为。
6. `system.storageStatus.format_version` 报 `meta.toml` 实际值。
7. 文档：
   - 主仓：`wind-store/AGENTS.md`（`user/`、redb、`*.json` 等大面积过期）、`wind-userdata/AGENTS.md`（「Incr/Touch 不进文本」）、模块头与日志里的「redb」措辞、设计 §3.3/§3.5/§3.6「运行时层」收口与 §10 P5 行补记差异（§3.9 索引、§3.2 时间上限、§3.5 降优先级、草稿不进备份）。
   - 文档仓 WindInputDocs：`reference/faq.mdx`、`start/concepts.mdx`、`settings/index.mdx`、`guides/config/index.mdx`、`settings/dict/phrases.mdx`、`settings/dict/index.mdx`（字符集一段现已反转）、`settings/stats.mdx`——把「`userdata.redb` 用户数据库」改为「`data/` 下可手改的文本，备份 = 复制用户目录（服务运行中先在设置页备份或 `dict.flush`）」，并说明 `userdata.redb.legacy` 是升级前的旧库。
8. beta 发版说明（见 §5）。

### 3.2 beta 期间

- 设计 §3.9 索引产物（`.wtxi` / `.wasi`）：现仍按代次轮询整表重建，性能项。
- 日志保留「7 天时间上限」、折回线程降优先级、`.icloud` 占位与「已删除」的区分。
- 备份不含草稿（裁定不备份并在 §10 记一笔即可）。
- mac / Android 的 `machine_fp` 近乎常量；Android 派生模式下 `device.toml` / 日志在用户目录内——P6 同步前处理。
- Linux 状态目录用 `data_local_dir()`，设计写的是 `XDG_STATE_HOME`——统一一处。
- 延后 minor：隔离时学到的数据导入报错（按设计保留）。

## 4 版本与流程

- 版本号：`0.127.0-beta.1`、`-beta.2`… → `0.127.0`。
- 发版走现有 `scripts/release.sh`（`check → push → wait → sign-draft`）；beta 的 Release 必须勾 pre-release（§3.1 第 1 条落地后自动）。
- 每个 beta 的 Release 正文人工填写区写明本版变化与已知问题；文档仓按后缀进 beta 通道。

## 5 beta 测试

### 5.1 测试者须知（写进发版说明）

- 升级前先在设置页「备份」，或整个复制用户目录（`%APPDATA%\WindInput`、便携版 `userdata\`）。
- 升级后用户数据在用户目录的 `data/` 下，都是可手改的文本；旧数据库改名为 `userdata.redb.legacy` 留作兜底。
- 回退到 0.126.0：卸载 beta、装回 0.126.0，**首次启动前**把用户目录里的 `userdata.redb.legacy` 改名回 `userdata.redb`（若已启动过、生成了新的 `userdata.redb`，先删掉它再改名）。回退后看到的是升级那一刻的数据，beta 期间新学到 / 新加的内容不在旧版里。之后再升级，以 `data/` 为准，回退期间在旧版里的改动不会带回来。
- 数据文件有问题时，设置 → 词库页顶部会提示是哪个文件、坏了几行。
- 反馈附：日志目录、`meta.toml`、`data/` 的文件清单（不必附内容）。

### 5.2 维护者验证矩阵

| 场景 | Windows 安装版 | Windows 便携版 | 自定义数据目录 | Linux deb | macOS pkg |
|---|---|---|---|---|---|
| 0.126 真实数据升级（词数、短语、统计、补全逐项比对） | ✓ | ✓ | ✓ | ✓ | ✓ |
| 全新安装 | ✓ | ✓ | | ✓ | ✓ |
| 回退到正式版再升级（D1 的预期行为） | ✓ | | | | |
| 复制用户目录到另一台机器 / 删 `state/` | ✓ | ✓ | | ✓ | |
| 手改各类文件（坏行、整份坏、修好后合入） | ✓ | | | | |
| 备份 → 清空 → 还原 | ✓ | | | ✓ | ✓ |
| 卸载「删除用户数据」 | ✓ | | ✓ | | |
| 按键延迟（与 0.126 基线对比，日志分段） | ✓ | | | | |

Windows 已在靶机 Dev 实例上验过升级、删 `state/`、手改往返；Linux / macOS 尚未实机验证。

### 5.3 进入正式版的判据

- beta 至少两周，期间无数据丢失报告；所有「数据文件有问题」的反馈已定位。
- §5.2 矩阵全部打勾。
- §3.1 全部完成，§3.2 中影响数据的项已有结论。
- 正式版发布公告沿用 §5.1 的回退说明。

## 6 进度（2026-10-16）

§3.1 已完成：1（CI 预发布不进 R2、草稿自动勾 pre-release）、3（锁失败退化为只读）、4（注销 / 关机 / SIGTERM 先落盘，含统计）、5（写失败原因经 `storageStatus.write_error` 提示，设计 §8 改写）、6（`format_version` 报实际值，锁导致的只读也报）、7（主仓文档；文档仓另提交）、8（下面的公告草稿）。第 2 条按 D1 取消。

2026-10-16 补：预发布版本号全链路审计后修正——`release.sh` 拒收 `x.y.z-beta.N`、两个发版脚本在 beta
之后 bump 结果不一致（现均「转正」为 x.y.z）、文档仓会把 beta 写进 `releases.json` / `latest.json`
（下载直链 404，现同步脚本与定时兜底均按版本号后缀跳过，主仓 notify-docs 同步加闸）、安装器 PE 版本
被拆成 0.127.0.1、macOS 的 CFBundleVersion 非纯数字、正式版发布说明的上一 tag 会取到 beta。构建期注入
（service / setting build.rs）、deb 版本（`~beta`）、应用内升级比较（semver）本就正确。

版本号显示的统一规则：给人看的字段（各 exe / DLL / 安装包的 FileVersion、ProductVersion 字符串，macOS
的 CFBundleShortVersionString）一律保留 `-beta.N`；给系统比较的数字字段（PE 数字版本、CFBundleVersion、
pkg 版本）只取 x.y.z。macOS pkg 关闭 BundleIsVersionChecked，覆盖安装不受显示版本的后缀影响。

`v0.127.0-beta.1` 已打 tag，签名并上传为草稿（待发布）。文档仓的预发布闸（CI 那笔）已推（WindInputDocs `79da863`）。

待做：靶机验证会话结束落盘（需人工注销一次看日志）；Linux / macOS 按 §5.2 实机验证；macOS 正式版覆盖安装
beta 的真机确认。

发版操作注意：
- 发 beta：`release.sh push 0.127.0-beta.1`（或菜单 5）；之后的 `wait` / `sign-draft` 不带版本号时（含菜单
  6–9）取远端版本号最大的 tag。beta 在外期间给旧线发版，要用子命令并带上版本号。
- 签名段会在编译机重新打包：签名前确认编译机的 wind-installer 已含版本资源那两笔修正。
- macOS：正式版覆盖安装 beta 需真机确认一次。

## 7 beta 发布公告草稿（Release 正文人工填写区）

### 清风输入法 0.127.0 测试版：用户数据改为可直接复制、可手改的文本文件

这是测试版，**升级前请先在设置页做一次备份，或复制整个用户目录**（安装版 `%APPDATA%\WindInput`，便携版 `userdata\`）。

**变化**

- 用户词、临时词、草稿、词频、联想历史、命令短语、候选调整、快捷格式、常用字、字符集、补全历史、输入统计，全部保存在用户目录的 `data\` 下，都是可用文本编辑器打开的文件。**复制用户目录就能把全部数据带到另一台电脑。**
- 首次启动会自动从旧的 `userdata.redb` 迁移，迁完旧库改名为 `userdata.redb.legacy` 保留，确认无误后可删。程序保留的上一版与损坏文件副本集中在 `data\.backup\`。
- 设置 → 词库改为三栏：左边选方案或数据类别，中间选类别与分库，右边是内容。用户词与分库、临时词、词频、短语、候选调整、快捷格式、常用字、字符分类都能「编辑文件」：在你的编辑器里打开对应文件，改完点「完成并加载」即生效。
- 用户词库可以有分库（如成语、行业词）：在词库页新建或从文件导入，中栏逐个勾选开关；词条能在主词库与分库之间移动。
- 直接手改这些文件后，在词库页点「刷新」或重启输入法服务即生效。写坏的行会被跳过并原样保留；整个文件写坏时按上一次正常的内容继续工作、不会覆盖你的文件。设置 → 词库页顶部会提示哪个文件有问题。
- 学到的数据停手 30 秒内写回文本（连续输入时最多 5 分钟）；注销、关机时会先写回。

**回退到 0.126.0**

旧版本读不了新格式。卸载测试版、装回 0.126.0 后，**首次启动前**把用户目录里的 `userdata.redb.legacy` 改名回 `userdata.redb`（若已启动过、生成了新的 `userdata.redb`，先删掉它再改名）。回退后看到的是升级那一刻的数据；测试版期间新增的内容不在旧版里。之后再升级测试版或正式版，以 `data\` 为准，回退期间在旧版里的改动不会带回来。

**反馈**

请附：日志目录（安装版 `%LOCALAPPDATA%\WindInput\logs`，便携版 `localdata\logs`）、用户目录里的 `meta.toml`、`data\` 的文件清单（不必附文件内容）。

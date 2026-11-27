# 数据同步（设计）

状态：已定稿，待实现。日期：2026-11-24。

> 2026-11-25：同步范围、设置按设备组同步、设备号与设置页的修改见 [sync-device-groups.md](sync-device-groups.md)，冲突处以它为准。

## 1. 目标与非目标

**目标**：把用户目录里的数据在多台设备间同步，后端为用户自备的 WebDAV 或 S3 兼容存储
（坚果云、群晖、Nextcloud、R2、MinIO、阿里云 OSS 等），以后可接官方同步服务。

**原则**：

1. 核心不联网。网络、加密、大块 IO 都在独立进程 `wind_input_sync` 里，core 的二进制不引入任何网络/加密依赖。
2. 端到端加密。服务端只看到密文和不可读的对象名；同步密钥不离开设备。
3. 增量、可重试、幂等。断网、中途被杀、多设备同时写都不丢数据、不损坏远端。
4. 输入流程零影响。同步进程崩溃、卡死、慢，打字照常；core 只做拉起监护和少量重载入口。
5. 不留系统残留。不写自启动、服务单元、计划任务，由 core 按需拉起（与扩展宿主一致）。

**已定（2026-11-24）**：

- 分两期：**一期文件级**（配置、方案、主题、文本词库与短语）先交付；**二期操作级**（学习数据日志）另行设计。
- 同步进程由 core 拉起并监护。
- 监护逻辑这次就抽成共享库，扩展宿主与同步进程共用。
- 一期 WebDAV 与 S3 同时实现。

**非目标（一期）**：学习数据（临时词、词频、联想、草稿、统计）；官方同步服务；Android（见 §10）；
实时协同；同步历史浏览/回滚界面。

## 2. 现状（设计依据）

- **监护**：core 拉起扩展宿主的逻辑在 `apps/service/src/extensions.rs`（拉起、200ms 节拍、卡死判定、
  Job Object、stderr 日志轮转、宽限停止）；退避熔断 `Backoff` 在 `wind-hook/src/backoff.rs`；
  宿主监护扩展又在 `apps/extension_host/src/lib.rs` 复制了一份（含 `pre_exec` 设 PDEATHSIG）。
  core 侧监护流程无测试。macOS 无 Job/PDEATHSIG，只靠 stdin EOF。
- **RPC 与事件**：ctrl 管道与 events 管道的 SDDL 放行 Everyone/AppContainer（TSF 宿主需要），
  Unix socket 也不校验对端。**同步进程持有同步密钥与凭据，不能开任何具名端点**；与 core 通信沿用扩展的做法——继承匿名管道。
- **数据层**（`docs/design/user-data-storage-redesign.md`）：
  - 文本真源白名单 `wind-store/src/user_words.rs` 的 `TextPath`/`classify_text_path`。
  - 运行时写入先进 `.wjl` 再后台写回文本；`data.beginEdit`/`data.endEdit` 外部编辑会话会先写回待处理操作、
    暂停写回，结束时按文件重载再恢复——**正好是同步写入需要的语义**，同步直接复用。
  - `dict.reload(paths)`、`config.reload` 已有；**compat.toml、schemas/、themes/ 的文件内容被外部改了没有重载入口**，要补。
  - `dict.changed` 事件只有定义、全仓无人发送。core 没有任何文件监听。
  - `device_id`、日志在 `local/state/runtime/<dir_id>/`，不在用户目录，天然不进同步范围。
  - `wind-transfer` 的 Merge 只有类型骨架；`wind-userdata/src/text/patch.rs` 有按规范化键的行补丁可复用。
- **依赖**：workspace 没有 HTTP、TLS、加密、async、文件监听库。设置端有 `ureq 2`、`sha2 0.10`。
- **凭据存储**：全项目没有 DPAPI/Keychain/libsecret 用法。
- **Android**：UniFFI 进程内调 `wind-mobile`，无任何后台调度。

## 3. 架构

### 3.1 进程与 crate

```
设置端 ──ctrl RPC(sync.*)──▶ core(service) ──匿名管道──▶ wind_input_sync
                              │  拉起/监护（wind-supervise）     │ wind-sync（纯逻辑）
                              │  重载入口、编辑会话              │ 后端：WebDAV / S3
                              ▼                                 ▼
                          用户目录 ◀──────── 读写文本文件 ───────┘
```

| 位置 | 内容 |
|---|---|
| `crates/wind-supervise`（新） | 从 `extensions.rs` + `wind-hook/backoff.rs` 抽出：`Backoff`、`Job`、日志轮转、`Supervisor`（期望态、拉起、退出检测、退避重启、熔断、宽限停止、卡死判定）、子进程侧 `die_with_parent()`（PDEATHSIG / Job 由父进程负责）。不含帧与协议。 |
| `crates/wind-sync`（新） | 纯逻辑，无 IO 运行时假设：后端 trait、加密、清单、合并、待传队列、状态机。全部可用内存后端测试。 |
| `apps/sync`（新，产物 `wind_input_sync`） | 进程外壳：匿名管道协议、文件监听、调度、调用 `wind-sync`。 |
| `apps/service` | 新增 `sync.rs`：期望态由同步设置决定，复用 `Supervisor`；转发 `sync.*` RPC；执行同步进程发来的少量受限请求。 |

扩展宿主与 `extension_host` 改用 `wind-supervise`，行为不变；借此补上 core 侧监护的测试。

### 3.2 core ↔ 同步进程协议

帧沿用 4 字节大端长度前缀 + JSON。

- **core → sync**：`Hello{suffix, userdata_dir, state_dir, settings}`、`Settings{...}`（变更时）、
  `Command{id, op: sync_now|test_connection|create_key|import_key|export_key|reset_remote...}`、`Shutdown`。
- **sync → core**：
  - `Status{phase, last_ok, last_error, pending, conflicts}`（core 缓存，供 `sync.status` 查询并在 events 广播 `sync.statusChanged`，不带任何内容或路径以外的数据）；
  - `Request{id, method, params}`，**白名单**：`data.beginEdit`、`data.endEdit`、`dict.reload`、`config.reload`、
    `compat.reload`、`schema.reloadFiles`、`theme.reloadFiles`（后三个新增，§6）。core 用现有 dispatcher 执行后回 `Reply`。

同步进程不连 ctrl/events 管道。

### 3.3 同步设置与凭据

同步设置**不进 config.toml**：config.toml 本身要同步，把「是否同步、同步到哪」放进被同步的文件会自指，且凭据会被上传。

- 设置：`<local>/state/sync/settings.toml`（设备本地）：`enabled`、`backend`（`webdav{url, user}` / `s3{endpoint, region, bucket, prefix, access_key_id, path_style}`）、
  `scope`（四类开关：配置 / 方案 / 主题 / 词库与短语）、`interval_min`（默认 15）。
- 机密：WebDAV 密码、S3 secret、加密数据密钥（DEK）合存一份 `<local>/state/sync/secrets.bin`：
  - Windows：DPAPI（当前用户）封装，系统 API，不加依赖。
  - macOS：DEK 与凭据存 Keychain，文件里只留条目名。
  - Linux：0600 文件，不接 libsecret（依赖 DBus，无图形会话常不可用）。
  只在同步进程里读写；core 只转发设置端的**写入**，没有任何读取机密的 RPC（查看同步密钥见 §5，走 `sync.command`，结果只回给发起的设置端连接）。
- 依据：思源把 S3/WebDAV 凭据与仓库密钥明文存在配置里；rclone 只做可逆混淆；Joplin 后来才在 Win/mac 改用钥匙串、
  Linux 未跟进。本设计在 Win/mac 用系统保护、Linux 与它们持平。
- 设置端 RPC：`sync.getSettings`（不含机密，只回 `has_password` 之类布尔）、`sync.setSettings`、`sync.setSecret`、
  `sync.status`、`sync.command`。不新增 config.toml 键，因此不动能力快照与 mockdata 契约。

## 4. 远端格式

根为用户给定的 URL/前缀下的 `windinput-sync/v1/`：

```
meta.json                      明文：格式版本（2）、同步密钥校验值（不含密钥本身）
devices/<dev>/manifest.<gen>   密文：该设备的清单，gen 单调递增；保留最近 2 代
devices/<dev>/info             密文（devinfo 子密钥）：设备名册条目 {name, platform, app_version, caps, last_seen}
blobs/<id>                     密文：文件内容，id = HMAC(files 子密钥, 明文 SHA-256) 的 base32
```

- **每设备只写自己的目录**（`<dev>` 为同步专用的随机设备号，存在同步状态里，与日志的 device_id 无关）。
  WebDAV 和 S3 都没有可靠的原子比较交换，按设备分写就不需要锁。
- **清单条目**：`path → {hash, base, deleted, lamport, device}`。`base` 是本设备上次同步时该文件的 hash，
  用来判断「谁在谁之后」以及三方合并的公共祖先。路径与内容都在密文里，服务端只看到设备号、代数和 blob 名。
- **blob 内容寻址**：同内容同名，上传天然幂等；先传 blob、后写清单，清单写成功才算提交。中途失败只会留下无主 blob。
- **回收**：任一设备最新两代清单都不引用、且超过 7 天的 blob 由下次同步删除；只扫 `blobs/`，不碰 `devices/*/info`。

## 5. 加密

> DEK 不直接加密对象：按用途经 HKDF 派生子密钥（文件、设备信息、剪贴板、传输、局域网），见 `sync-messaging.md` §3.1。下文「用 DEK 加密」均指文件子密钥。

- **单一同步密钥**：DEK 就是用户看到的「同步密钥」，没有口令这一层。首台设备首次启用时生成随机 32 字节 DEK，
  展示为 base32 小写无填充（52 字符），每 4 字符一组用 `-` 连接，如 `k7m2-x3pq-…`；粘贴时忽略大小写、空白与 `-`。
  密钥只存本机机密存储与用户自己的备份里，**不上传**；远端 `meta.json` 只有格式版本（`format: 2`）与 HMAC 校验值，
  用来在粘贴时核对「密钥对不对」。（思源同做法；没有口令封装，所以也没有 KDF 与盐。）
- 新设备加入：粘贴同步密钥，校验通过后存入本机机密存储，再进入身份确认（见 `sync-device-groups.md` §4.5）。
- **丢失后无法恢复**：没有恢复密钥，也没有口令可找回。所有设备都丢了密钥时只能重置远端（清空后生成新密钥，以本机为准重新上传），其他设备用新密钥重新加入。
  重置前应先暂停其他设备的同步：它们若还在用旧密钥写入，残留的旧密钥对象会挡住回收，直到被移除。
- 每个对象独立随机 nonce，XChaCha20-Poly1305，附加数据绑定对象类型与 id，防止对象被调换。
- 旧的口令格式 `meta.json`（含 `kdf_alg`）不再识别，报「远端同步库格式不受支持，请重置远端」。

## 6. core 要补的

1. **`wind-supervise` 迁移**（§3.1），扩展行为回归不变。
2. **重载入口**：`compat.reload`（现在改了 compat.toml 只能重启服务）、`schema.reloadFiles(ids)`、`theme.reloadFiles(ids)`。
   设置端也能用，顺手解决手改这些文件不生效的问题。
3. **`sync.*` 转发与 `sync.statusChanged` 事件**。
4. 不加「core 写了哪些文件」的变化信号：一期靠同步进程监听文件（`notify` + 每 `interval_min` 全量扫描兜底），
   回声抑制靠「上次已同步 hash」。`dict.changed` 这个死事件维持现状，另行清理。

## 7. 同步流程（一期）

一次同步 = 扫描 → 拉取 → 合并 → 写入本地 → 上传：

1. **扫描本地**：按范围列出文件，算 hash，与本地状态库（`<local>/state/sync/state.redb` 或 toml，记每个文件上次同步的 hash）比，得出本地改动。
2. **拉取**：列 `devices/*/`，取各设备最新清单（按 gen 缓存，未变的不下载）。
3. **逐文件裁决**（L = 本地当前，B = 上次同步，R_i = 各远端设备的最新）：
   - 只有一方相对 B 变了 → 取变的那方。
   - 多方都变且内容不同 → 冲突，按文件类型合并（§8）。
4. **写入本地**：涉及 `data/` 文本的，先 `data.beginEdit(paths)`，原子写入（同目录临时文件 + rename），再 `data.endEdit`；
   config.toml → `config.reload`；compat/方案/主题 → 对应重载入口。一批写入合并成一次重载。
5. **上传**：新 blob → 新清单（gen+1）。更新本地状态库。

触发：本地文件变化去抖 10 秒；定时 `interval_min`；启动后 30 秒；设置端「立即同步」。
失败：待传队列持久化在状态库，指数退避（30 秒起，上限 30 分钟）；认证失败、同步密钥不对、远端格式不认识则停在错误态等用户处理，不重试。

## 8. 合并规则

| 文件 | 规则 |
|---|---|
| `data/**/*.dict.yaml`、`phrases.dict.yaml` 等逐行表 | 三方合并：以 B 为祖先，按规范化键（编码+词，复用 `text/patch.rs` 的键）逐行合并；同键双方都改取权重较大者；一方删一方改保留修改。头部（yaml 元数据）不同时按 TOML/YAML 规则或取较新。 |
| `config.toml`、`compat.toml`、`dicts.toml` 等 TOML | 三方合并到键级；同键冲突取 lamport 较大者，另一值写入冲突副本。 |
| 主题、方案文件及其他 | 不合并：取 lamport 较大者，另一版保存为 `<name>.conflict-<设备名>-<日期>.<ext>`，设置端提示。 |

**说明**：TOML 内容级合并后重新序列化，**会丢失注释与排版**（仅当一方已包含另一方全部修改时才保留原字节）；yaml 头部双方都改且不同时取本地并计入冲突提示。

**同步范围（一期）**：`config.toml`、`compat.toml`、`schemas/`、`themes/`、`data/` 下的
`phrases.dict.yaml`、`quick_format.toml`、`common_chars.yaml`、`charsets/`、`<schema>/user.dict.yaml`、
`<schema>/shadow.dict.yaml`、`<schema>/dicts/`、`<schema>/dicts.toml`。

**不同步**：`meta.toml`（dir_id）、`data/.backup/`、`phrases.sys_hash`、`local/`（state、cache、日志、device_id）、
以及二期才处理的 `temp`/`freq`/`assoc`/`draft`/`stats`/`completion`。

## 9. 依赖（只进 `wind-sync` 与 `apps/sync`）

`ureq`（同步 HTTP + rustls，与 core 的同步线程模型一致，不引入 tokio；版本与设置端对齐）、
`rusty-s3`（不带 IO 的 S3 SigV4 签名，配 ureq 用）、WebDAV 自己实现（PROPFIND/GET/PUT/DELETE/MKCOL，量很小）、
`chacha20poly1305`、`sha2`、`hmac`、`notify`、Windows `windows` crate 的 DPAPI、macOS `security-framework`。

证书：默认按内置根证书校验。同步设置里的 `skip_cert_verify` 打开时换一个不校验服务器证书的 rustls 验证器
（握手签名照常校验），给自签名证书的自建服务用，不算换存储、不丢已存密码。证书校验失败报 `Fatal` 并提示这个开关，
不当作临时故障反复重试。

## 10. 分阶段

| 阶段 | 内容 | 验收 |
|---|---|---|
| S0 | `wind-supervise` 抽库，扩展宿主两层迁移 | 现有 extension_host 测试全过；补 core 侧监护测试 |
| S1 | `wind-sync`：加密、清单、裁决、三类合并、待传队列，内存后端 | 单测覆盖多设备并发、中途失败重放、冲突、重置远端换密钥 |
| S2 | WebDAV、S3 后端 | 本机起 WebDAV（如 `dufs`/`rclone serve webdav`）与 MinIO 的集成测试；坚果云、R2 人工验 |
| S3 | core：拉起监护、匿名管道协议、重载入口、`sync.*` 转发 | 用桩同步进程测协议；重载入口单测 |
| S4 | `wind_input_sync` 外壳：监听、调度、机密存储 | Linux 本机两个用户目录对拍；Windows 靶机验 DPAPI |
| S5 | 设置端「同步」页：后端、同步密钥（生成/查看/粘贴）、范围、状态、立即同步、冲突提示 | 设置端测试 + 靶机真机 |
| S6 | 文档站；打包带上 `wind_input_sync`（Windows/Linux/mac） | 安装包内容检查 |

Android 放在一期交付之后：`wind-mobile` 新开 `sync_once()` 接口 + WorkManager，复用 `wind-sync`。

**二期（另写设计）**：学习数据按设备日志对象同步，core 新增「导入外部设备日志」入口按 (device_id, seq) 去重，补删除标记。

## 11. 已定的取舍

1. 同步设置与机密放设备本地、不进 config.toml（§3.3）。
2. 机密：Windows DPAPI、macOS Keychain、Linux 0600 文件（§3.3）。
3. 不设口令与恢复密钥，只有一个同步密钥（生成/查看/粘贴，§5）。
4. 不可合并文件的冲突：自动取较新 + 保存冲突副本（§8）。
5. Android 排在一期之后（§10）。

## 12. 空间占用

设置端存储卡显示同步库在远端占了多少空间，点开看明细。

**口径**（单位字节，只算 `windinput-sync/v1/` 之内，列一次同步根求和）：

| 字段 | 含义 |
|---|---|
| `total` | 合计 |
| `blobs` | `blobs/`：文件数据，各设备共用、按内容去重 |
| `blobs_unreferenced` | 其中任一设备最新两代清单都不再引用的旧版本（与 §4 回收同一引用口径）。「未被引用」不等于「将被删除」：回收另要求对象超过 7 天，期间若又被新清单引用就不删，因此它是会被回收量的上界。有设备清单解不开时无法确定，为空（其余照常统计） |
| `devices[{id, learned, stats}]` | `learn/<dev>/`：按最新两代目录里的对象名拆分，`stats/` 开头的算打字统计，其余（含目录本身与待回收的旧对象）算学习数据；目录解不开的整份算学习数据 |
| `other` | 其余：`meta.json`、`devices/<dev>/` 下的清单与设备信息，以及同步根下不认识的对象（如设备号不合法的 `learn/` 目录） |
| `provider{used, available, quota}` | 服务端口径：`used` 已用，`available` 剩余，`quota` 总容量；各项给不出为空 |

设备按设备号列出，包括已移除但仍有学习数据的；设置端用名册显示名，名册外的标「已移除的设备（号）」。

**时机**：同步轮在本轮全部写入（文件、学习数据、设备信息补写）之后顺带统计，中途响应取消；距上次成功满 24 小时才算；设置端存储卡「刷新」发 `refresh_usage`，
立即在后台统计（与同步轮串行：同步中回「正在同步」，统计中不起同步轮）。统计期间状态带 `usage_busy`。
只有后端错误算失败：只记日志与状态里的 `usage_error`，不影响同步轮成败、不触发退避，一小时后再自动尝试，统计时间不更新。
手动统计线程异常退出也回一条失败（「统计异常中止」），不会卡在统计中。统计时间在未来（时钟回拨）视为到期。

**缓存**：`<state_dir>/usage.json`（`{library, usage}`，`usage.computed_at` 为统计时间）；只对统计时的同步库有效，
换库后不显示、下一轮重算。经 `sync.status` 的 `usage` 字段给设置端，从未统计过则省略。

**服务端用量**：`Backend::usage()` 可选，默认 `None`。WebDAV 以 Depth 0 PROPFIND 根集合取 RFC 4331
`quota-used-bytes` 填 `used`（只是这个目录的用量，界面标「此目录已用」）、`quota-available-bytes` 填 `available`
（按 u64 解析，负数如 Nextcloud 的 -3「不限」、超大或缺失为空），不合成总容量；服务端不支持（404/405、空属性）返回 `None`
不报错。S3 没有配额接口，返回 `None`。`quota` 留给将来的官方同步服务（计费口径），设置端只认这一个结构：主行有 `available`
时附「网盘剩余」，明细有 `quota` 才列「总容量」。

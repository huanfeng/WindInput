# 主题编辑器：皮肤可视化编辑（W3）

承接 `theme-skin-animation.md` §3–§9 与 `theme-editor-wasm.md` 的 W3 占位。跨仓：编辑器 `WindInputThemeEditor`
（含市场前端与后端 `worker/`）、设置端 `wind-setting`、主仓 `WindInput`。

编辑器现在只把 `[skin]` 当透传保留块原样往返，没有任何皮肤编辑 UI；候选窗预览已是 `wind-scene` wasm（W2）。本期目标：

- 皮肤在编辑器里是一等模型；
- 能在图上拖出拉伸带、内容框、槽位、装饰；
- 四个变体分页签编辑；
- 动画片段与事件可视化编辑；
- 带图主题经主题包（`.wtheme`）推到输入法、发布到市场；
- 引擎会拒收或会画歪的情形，落盘前就提示。

状态：设计稿，§11 的决定已由维护者拍板。皮肤格式未正式发布（0.131 首发），schema 与编辑器模型都不做兼容；
**在线连接的 TOML 推送、市场的 TOML 下载是已发布行为，保持可用**。

## 1. 取舍

| 问题 | 定案 | 理由 |
|---|---|---|
| 皮肤数据模型 | `ThemeV3.skin: SkinV3` 一等字段，逐层带 `extra` 透传；序列化成规整表头（`[skin.horizontal.frame]`、`[[skin.horizontal.decor]]`） | 表单与可视化要逐字段读写；现在 `dumpKvBlock` 把整个皮肤写成一行巨型行内表（`toml.ts:637`） |
| base 继承 | 编辑器 `resolveBase` 对 `skin` 按变体整表替换，与引擎 `replace_inherited_variants` 同一规则（W3.2 修） | 现在 `deepMerge`（`merge.ts:8`）逐字段合并同名变体，base 指向编辑器方案时预览与实机不一致 |
| 与引擎对账 | `check:engine` 扩到 `wind-theme/src/skin.rs` 的字段、表形态枚举、取值枚举，以及 `wind-transfer/src/theme.rs` 的包上限常量 | 皮肤字段多、还在变；包上限两仓各写一份，必须钉住 |
| 资源存放 | 资源仓（IndexedDB，按内容哈希存字节）；**只管皮肤图与今后新导入的位图**，已有 `data:` 图片不动，给手动「移入资源仓」按钮 | data URI 会进 localStorage（方案、撤销栈、版本三处，撤销栈不封顶、每步一份全量快照），几 MB 的皮肤图撑不过几次编辑 |
| 非包出口 | 在线连接推送、市场发布**改走主题包**，图片按文件传；工作区写回把图写进目录 `assets/`；复制 TOML 保留路径并提示需配合主题包 | base64 内联让体积 × 4/3，且设置端下载主题文本限 2 MiB、市场源码限 512 KiB，都装不下皮肤 |
| 拉伸带 / 内容框的编辑面 | 单独的**源图编辑器**面板（在窗体原图上按源像素拖） | 这些是源图坐标；预览里窗体已被 round / tile 重铺，在变形后的图上拖难对准、难理解 |
| 槽位 / 装饰 / 对齐点的编辑面 | **预览画布叠加层**（在 wasm 出的真实帧上拖） | 它们相对窗体 / 内容框的实时位置定义，只有在排好的帧上拖才所见即所得 |
| 叠加层要的几何 | wasm 帧回执加 `skin` 段（变体、`k`、窗体 / 内容框 / 槽位 / 装饰矩形、轴映射、对齐点） | 引擎每帧都算了（`SkinGeom`），只是没交出来；编辑器重算等于再抄一遍求解器 |
| 动图帧数 / 时长 / 缩略图 | wasm 加无状态函数 `inspectImage` / `decodeFrames`，用引擎同一个解码器；不用浏览器 `ImageDecoder` | 帧区间按**引擎原始帧号**写，帧数与时长归一必须与引擎一致；`ImageDecoder` 各浏览器支持不齐 |
| 引擎告警 | wasm 装 tracing 收集器，只收 `skin:` 前缀的 WARN，随回执带出；引擎零改动 | 编辑器显示的就是实机日志里的同一句话 |
| 编辑器自己的校验 | 槽位重叠、盖字装饰、拉伸带轮廓、包体上限、`assets/` 路径、帧区间越界 | 引擎对这些要么不报，要么只在加载时 warn；编辑器能在落盘前带定位指出 |
| wasm 不可用时 | 皮肤数值表单照常可用，可视化与皮肤预览禁用并说明原因；旧版 Canvas2D 不补皮肤 | 旧版在 W4 删，不为它再实现一遍切片 |

## 2. 现状

盘点基准：编辑器 `55a4988`（`feat/wasm-preview`），WindInput `feat/theme-editor-wasm`，wind-setting `feat/theme-skin`。

| 位置 | 现状 | 对 W3 的影响 |
|---|---|---|
| 编辑器 `src/lib/theme3/toml.ts:68`、`:1285` | `skin` 在 `RESERVED_TOP`，原值放进 `theme.extra.skin` | 升级成一等字段 |
| `toml.ts:637` | `extra` 的表按 `dumpKvBlock` 写，皮肤变成一行嵌套行内表 | 专门的序列化 |
| `src/lib/theme3/merge.ts:35` | `deepMerge` 逐字段合并，数组整替 | 变体级整表替换 |
| `src/lib/theme3/package.ts:126-129` | 导入时把 `[skin]` 里 `assets/` 开头的字符串回填成 data URI | 改为进资源仓、保留路径 |
| `src/shared/themePackage.ts:161` | `packTheme` 一律 `zipSync(level 6)`，无条目 / 体积检查；市场后端也用它现打 `.wtheme` | 图片存储模式；按导入口径自检 |
| `stores/theme3.ts:105`、`stores/history.ts`、`stores/versions.ts` | 方案、撤销快照（JSON 全量、不封顶）、版本都经 pinia-persistedstate 写 localStorage | 大图不能进主题对象 |
| `src/lib/preview/scenePreview.ts` `SceneRenderOpts` | 没传 `embedded` / `above` | 嵌入变体预览要补 |
| wasm `preview.rs:482` `render` | 回执 `{ width, height, rgba, nextMs, nodes, clamped }` | 加 `skin`、`warnings` |
| `wind-scene/src/candidate/skin_geom.rs:273` `SkinGeom` | 几何字段齐全但私有 | 加只读快照 |
| wind-setting `src/theme_editor_server.rs` | `GET /api/themes`、`GET /api/theme/:slug`（`theme.getText`）、`POST /api/theme/push`（JSON `{toml, force, slug}` → `theme.importFromText`）；请求体 `read_to_string` 不限长 | 加收包 / 出包两条路由，并给请求体设上限 |
| WindInput `wind-webdata` `theme.importPackage { path, slug?, force? }`、`theme.exportPackage { slug, path }` | 设置端「导入 / 导出主题包」与 CLI 已在用；回执形状与 `importFromText` 一致（`conflict` / `slug` / `reloaded`）；`wind-transfer` 按 200 条 / 64 MiB / 单图 16 MiB / 文本 4 MiB 校验，并检查皮肤引用齐全 | 推送直接复用，**WindInput 无需改动** |
| wind-setting `theme_import.rs` `download_https_text` | `windinput://import/theme?url=` 只按文本下载，上限 2 MiB、要求 UTF-8 | 要认包链接 |
| wind-setting `schema_url_import.rs` | 方案包按二进制下载：只认官方域（每一跳重查）、64 MB、写私有临时文件 | 主题包下载照抄这套 |
| 市场 `worker/routes/themes.ts` | 发布 / 更新是 JSON `{ source, preview_base64, … }`；源码存 `themes/{id}.yaml`；`/:id.toml` 原样给，`/:id.wtheme` 由源码现打；`publishRules.RULES.source.maxBytes = 512 KiB` | 加包形态的存储与分发 |
| 市场 `worker/lib/themeRes.ts` | res 域名按 `rev` 内容寻址缓存一年；**改了 `packTheme` 的输出必须给 `REV_SCHEME` 加一** | 图片改存储模式会改字节 |
| 市场 `src/lib/market/format.ts:98` `importUrl` | 一键导入链接一律指向 `.toml` | 带图主题改指 `.wtheme` |
| 生产代理 | openresty `client_max_body_size 16m`，后端容器 256 MB（`docs/superpowers/plans/2026-10-08-market-schema-publish-p3.md`） | 要放宽到能收 64 MiB 的包 |

## 3. 数据模型

### 3.1 类型

新建 `src/lib/theme3/skin.ts`，字段名与可选性对应 `wind-theme/src/skin.rs` 的 serde 结构。模型形态即写入形态，不做 normalize 式变形：

```ts
interface SkinV3 {
  scale_with?: "dpi" | "font"; density?: number; font_base?: number;
  horizontal?: SkinVariantV3; vertical?: SkinVariantV3;
  horizontal_embedded?: SkinVariantV3; vertical_embedded?: SkinVariantV3;
  extra?: Record<string, unknown>;
}
interface SkinVariantV3 {
  frame?: SkinFrameV3; content?: Insets; decor?: SkinDecorV3[];
  placement?: SkinPlacementV3; slots?: { preedit?: SlotV3; candidates?: SlotV3; footer?: SlotV3; extra? };
  extra?: Record<string, unknown>;
}
type BandV3 = [number, number] | { from: number; to: number; fill?: Fill; extra? };
interface SkinDecorV3 {
  image: string; anchor?: string; pivot?: string; offset?: [number, number]; size?: [number, number];
  density?: number; z?: number; hit?: "drag" | "block" | "pass"; opacity?: number; fps?: number;
  states?: Record<string, ClipEntry>; on?: Record<string, ActionEntry>; extra?;
}
// ClipEntry / ActionEntry：能认出的解析成 { frames } | { image } / { state } | { play, then }；
// 认不出的保留为 { raw: unknown }，表单只读显示并标警告（引擎对这类条目 warn 后丢弃）。
```

- 锚点、pivot 保持字符串。读入时认引擎认的全部写法（`top-left`、`center`、省略 `frame.`），不改写作者原文；
  只有作者在面板上动过，才写回规范形。
- 拉伸带保持 Pair / Table 两种形态：没写 `fill` 写 Pair，写了升成 Table。`placement.caret`、`avoid_caret` 同理保持原形态。

### 3.2 解析 / 序列化 / 往返

- `skinFromObject` / `dumpSkin` 进 `toml.ts`。`skin` 仍留在 `RESERVED_TOP`（是保留块，不当视图节点），只是不再落进 `extra`。
- 序列化顺序：
  1. `[skin]` 的标量；
  2. 各变体按 `SKIN_VARIANTS` 的顺序，依次写 `frame`、`content`、`placement`、`slots.*`；
  3. 再写 `[[skin.<v>.decor]]`，并紧跟它自己的 `[skin.<v>.decor.states]` / `[skin.<v>.decor.on]`——数组表的子表必须紧随其后，否则会归到下一个装饰。
  `states` / `on` 的条目写行内表。
- 未知字段：每层的 `extra` 写回该层末尾；未知的变体键落进 `SkinV3.extra`。
- `strict.ts` 的往返检查覆盖皮肤：认不出的字段照旧报「未识别」；已知键写错类型（落进该层 `extra`）单独报
  「类型不符，引擎拒收整个主题」，面板写这个键时删掉 `extra` 里的同名键。
- JSON 存不下的值（`nan` / `inf`、超出安全整数的整数、日期时间）解析后包成 `{ $toml: 原文 }`，经方案、撤销、
  版本的 JSON 持久化后照原文写回。
- 已知限制：smol-toml 把 `3.0` 与 `3` 都读成同一个 number，编辑器分不出浮点写法的整数（`z = 3.0` 引擎 i32
  会拒收，编辑器读成 3、写回 3）。方向安全，不补。

### 3.3 base 继承

`resolveBase` 合并前对 `skin` 做与引擎相同的预处理：子主题写了哪个变体键，就把 base 的同名变体整表删掉；`skin` 顶层标量仍逐键覆盖。
注释写明它与 `replace_inherited_variants` 是同一条规则（写法同 `dropInheritedArrowImages`）。

### 3.4 `check:engine` 扩展

- `findCrateSrc` 另读 `wind-theme/src/skin.rs` 与 `wind-transfer/src/theme.rs`。
- `PAIRINGS` 加 8 个 struct：`Skin`、`SkinVariant`、`SkinFrame`、`SkinInsets`、`SkinDecor`、`SkinPlacement`、`SkinSlots`、`SlotSpec`。
- 新增 `enumVariantFields`：抽 untagged 枚举表形态的字段（`BandSpec::Table`、`CaretSpec::Table`、`AvoidSpec::Strip`）。
- `rustStrArray` 放宽正则，兼认 `const X: [&str; N] = [..]`（用来对 `SKIN_VARIANTS`）。
- 取值枚举：从 `BandFill::parse`、`DecorEvent::parse` 等函数体里抽 `"xxx" =>` 字面量，与编辑器的下拉选项比。
- 包上限：抽 `MAX_THEME_ENTRIES`、`MAX_THEME_UNCOMPRESSED_BYTES`、`MAX_THEME_IMAGE_BYTES`、`MAX_THEME_TEXT_BYTES`，与 `src/shared/themePackage.ts` 的 `THEME_PKG_LIMITS`（新增）比。
- 找不到函数或常量就失败（引擎重构了要来改脚本）。

## 4. 资源

### 4.1 资源仓

只做最小可用，不做完整的虚拟文件系统（目录、改名、多主题共享视图都不做）：

| 部件 | 做法 |
|---|---|
| 字节存储 | IndexedDB 库 `wind-theme-editor` 新开 store `assets`（与 `handleStore.ts` 同库升版本），键 = SHA-256，值 = `{ bytes: Blob, mime }` |
| 主题里的引用 | 皮肤图与新导入的位图写 `assets/<文件名>`；`ThemeV3.assets?: Record<路径, 哈希>` 是该方案的清单。清单很小，跟着方案、撤销快照、版本一起持久化，所以撤销「换图」天然正确 |
| 同步读 | 启动时把各清单引用的字节读进内存 `Map<哈希, Uint8Array>`；序列化、预览、组包都同步取 |
| 回收 | 启动后扫一遍方案、撤销栈、版本里的清单，删掉没被引用的哈希 |
| IDB 不可用（隐私模式） | 退回内存 Map，提示「刷新后皮肤图会丢失，请导出主题包」；不退回 data URI |
| 已有 `data:` | 照旧支持、不自动迁移；资源面板给「移入资源仓」按钮（写清单、改引用，作为一步可撤销） |

### 4.2 各出口

| 出口 | 带图主题怎么出 |
|---|---|
| 预览（wasm） | 资源表 = 清单的 `路径 → 字节`，加上 `extractDataUris` 拆出的 data 图（旧图照旧） |
| 导出主题包 | 清单字节按路径进 `assets/`；PNG / WebP / GIF 用存储模式；导出前按 §8 自检 |
| 导入主题包 | `assets/` 文件进资源仓、写清单，引用保持原路径 |
| 在线连接推送 / 拉取 | 主题包（§5） |
| 市场发布 / 「在编辑器里打开」 | 主题包（§5） |
| 工作区写回 | `theme.toml` 照旧写；清单里的图写进工作区目录 `assets/`（同名同内容跳过，不删目录里别的文件）。载入时 `theme.toml` 引用的 `assets/*` 从目录读进资源仓 |
| 复制 TOML / TOML 面板 | 保留 `assets/` 路径；主题含资源仓图片时顶部提示「此主题的图片不在 TOML 里，分享请导出主题包」 |

## 5. 包通道

在线连接与市场的唯一载体都是 `.wtheme`（组包逻辑仍只在 `src/shared/themePackage.ts` 一处）。判据统一为：
**主题清单非空（引用了资源仓里的图）就走包，否则走原有 TOML 通道**。纯 TOML 主题的行为与现在逐字节一致。

### 5.1 共用：组包与校验

- `packTheme` 改为图片条目存储模式（`level: 0`）、文本条目照旧压缩；接受「清单字节」作为额外资源入参。
- 新增 `validateThemePackage(zip)`（`src/shared`，编辑器导出前与市场后端都调），口径与 `wind-transfer::theme` 一致：
  - 合法 zip，条目 ≤ 200；
  - 解压总量 ≤ 64 MiB，单图 ≤ 16 MiB，文本条目 ≤ 4 MiB；
  - 有 `theme.toml`；路径穿越 / 大小写重名拒收；
  - `[skin]` 引用的文件都在包内、都在 `assets/` 下。
  上限常量由 `check:engine` 对 `wind-transfer` 钉住（§3.4）。
- 市场后端复用 `src/shared/zipReader.ts` 的流式读与防炸弹判据，不整包同步解压。

### 5.2 在线连接（编辑器 ↔ wind-setting ↔ core）

新增两条路由，旧的三条不动：

| 路由 | 做法 |
|---|---|
| `POST /api/theme/package?slug=<id>&force=1` | 请求体是 `.wtheme` 原始字节（`Content-Type: application/octet-stream`，同在现有 CORS 允许的 `Content-Type` 头内）。服务端限长读取（64 MiB + 1 MiB 余量，超了 413 且不读完）→ 写进私有临时文件 → `theme.importPackage { path, slug, force }` → 删临时文件。回执与 `/api/theme/push` 同形（`imported` / `reloaded` / `theme_name` / `slug`，冲突回 409），编辑器的覆盖确认复用 |
| `GET /api/theme/:slug/package` | `theme.exportPackage { slug, path: <私有临时文件> }` → 回包字节 → 删临时文件。编辑器从输入法拉取带图主题时用它 |

- 走的是设置端「导入主题包」的同一条安装路径（`theme_import::import_package` 同一个 RPC），依赖链校验、回滚、
  当前主题热重载都由 core 负责；core 的 `importPackage` 已支持 `slug`（`theme_target_dir`），**WindInput 无改动**。
- `route_path` 剥查询串的行为保留；新路由自己解析 `slug` / `force`，`slug` 过 `validate_slug`。
- 顺带给 `/api/theme/push` 的请求体加限长读取（按 `MAX_THEME_BYTES` 2 MiB 加 JSON 余量），与设置端其他文本入口一致。
- 兼容：
  - 编辑器先试新路由；老版本设置端回 404「接口不存在」时，纯 TOML 主题退回 `/api/theme/push`，带图主题报
    「需要清风输入法 0.131 及以上才能推送带图片的主题」。
  - 拉取同理：`/:slug/package` 404 就退回 `/:slug`。
  - 老编辑器推纯 TOML 给新设置端，照旧可用。
- 测试（wind-setting）：限长读取（超限 413、未读完即断）、临时文件清理（成功 / 失败 / 冲突三路）、`slug` 校验、
  409 透传；RPC 用现有 `mock_reply` 打桩。

### 5.3 市场（编辑器前端 + `worker/` 后端 + wind-setting 链接导入）

**发布 / 更新**

- `POST /api/v1/themes`、`PUT /:id` 在 JSON 之外再收 `multipart/form-data`：`file` 字段是 `.wtheme`，其余字段（描述、标签、
  更新记录、预览图、状态、`if_updated_at`）同 JSON 版；写法照 `routes/packages.ts`（`bodyLimit`、声明长度预检、上传并发闸）。
- JSON 版（纯 TOML）保持原样，仍受 512 KiB 源码上限。
- 包版的处理：
  1. 服务端用 `validateThemePackage` 校验；
  2. 取出 `theme.toml` 走现有 `publishRules`（`meta` 字段规则不变；包版的源码上限取文本条目的 4 MiB）；
  3. 整包体积按包规则（64 MiB）。
- 编辑器 `PublishDialog`：主题清单非空时发包，否则照旧发 JSON。

**存储**

- 包版主题存两份对象：
  - 原包 `themes/{id}.wtheme`（新 key）；
  - 抽出的 `theme.toml` 仍写 `themes/{id}.yaml`——它供 `meta` 提取、搜索、管理后台查看、「在编辑器里打开」的首屏，
    写路径与纯 TOML 主题一致。
- `themes` 表加列 `format`（`'toml'` 缺省 / `'wtheme'`），迁移脚本放 `worker/migrations/sqlite`。

**下载**

| 路径 | `format = 'toml'`（含全部已发布主题） | `format = 'wtheme'` |
|---|---|---|
| `/:id.toml`、`/:id/toml`、`/:id/yaml` | 原样（不变） | 404 JSON「此主题含图片资源，请下载主题包」 |
| `/:id.wtheme`、`/:id/wtheme` | 由源码现打（同现在） | 原包，`package.toml` 按库记录重写（`name` / `author` / `version` 取库，同现打的口径），条目原样拷贝 |
| res 域名 `/themes/{id}/{rev}.{ext}` | 同上 | 同上；`computeThemeRev` 对包版主题把原包哈希计入 |

- `packTheme` 图片改存储模式会改变现打字节：`REV_SCHEME` 加一（`themeRes.ts` 的约定）。
- 一键导入：`importUrl` 对包版主题给 `.wtheme` 链接，纯 TOML 主题仍给 `.toml`。市场详情页对包版主题标
  「需要清风输入法 0.131 及以上」。
- 「在编辑器里打开」（`fetchMarketTheme`）对包版主题取 `.wtheme`，走 `packageToTheme`。
- 审核沿用现有的先发后管（发布即上架，管理员可下架）；管理后台主题详情对包版主题列出包内条目、尺寸，并显示图片缩略图。
  已做条目与尺寸（`GET /api/admin/themes/:id/package`，只读原包目录）；**缩略图留作后续**：要按区间取单条
  存储模式的图交给 `<img>`，并给后台加按条目取图的接口。

**输入法端（wind-setting）**

- `windinput://import/theme?url=` 现在只按文本下载（2 MiB、UTF-8）。改为：URL 路径以 `.wtheme` 结尾，或响应头几个字节是
  zip 签名，就走包下载。包下载照 `schema_url_import.rs`：
  - 只认官方域，每一跳重定向都重查；
  - 上限 64 MiB + 余量；
  - 写私有临时文件；
  然后走 `theme.previewPackage` → 现有确认框 → `theme.importPackage`。文本链接行为不变。
- 老版本设置端拿到 `.wtheme` 链接会报「主题文件不是有效 UTF-8 文本」。只有包版主题给这种链接，而它们本就需要 0.131 的引擎，接受。

**部署**

- 生产 openresty 只在 `POST /api/v1/themes` 与 `PUT /api/v1/themes/\d+` 两个 location 把 `client_max_body_size` 从 16m 放到 ≥ 69m（JSON 版后端另限 4 MiB；64 MiB 包 + 1 MiB zip 余量 + 3 MiB meta 字段〔预览图 base64〕+ multipart 包装），并保留 `proxy_request_buffering on`（包版路由不收无 Content-Length 的请求）。
- 上线后用一个接近上限、程序生成的包经 `api.windinput.com` 实走一遍，确认代理不回非 JSON 的 413。
- 容器 256 MB：上传闸全局只放一个；校验流式进行，不整包解压进内存。

## 6. 可视化编辑

### 6.1 坐标系

换算集中在纯函数模块 `src/lib/skin/coords.ts`（可单测）：

| 坐标 | 用在 | 与设备像素的关系 |
|---|---|---|
| 窗体图源像素 | `stretch_x/y` | × `render_scale / frame.density` |
| 主坐标（`skin.density` 源像素） | `content`、槽位 / 装饰的 `offset` 与 `size`、`placement` 偏移 | × `k`（= `render_scale / skin.density`；`scale_with = "font"` 时再乘字号比） |
| dp | `avoid_caret.before/after` | × `render_scale` |

- `render_scale` 与 `k` 由 wasm 回执给，编辑器不自己推字号比。
- 叠加层拖动：`Δ主坐标 = Δcss ÷ zoom × scale ÷ k`（画布 CSS 尺寸 = 逻辑 × zoom，设备 = 逻辑 × scale）。
- 源图编辑器里，内容框画在 `内缩 × frame.density / skin.density` 的图像素位置。
- 写回值取整到源像素，按住 `Shift` 时取到 0.5。

### 6.2 源图编辑器

皮肤面板（结构树「异形皮肤」）的每个变体页签里，数值表单上方是可折叠的「源图编辑器」，编辑当前页签的变体（继承自 base 时只读）：

- 窗体原图（棋盘格底），可缩放、平移；上、左两条标尺标源像素。
- 拉伸带：
  - 在标尺上拖出新带，拖两端改范围，拖中间平移，选中后可设 `fill`；
  - 带不得重叠、须落在图内，写回时排序；
  - 带内画半透明色块，`round` / `tile` 的带上叠一行重复单元示意。
- 不可伸缩（`[]`）与整轴拉伸（未写）用标尺旁的三态开关切换。
- 内容框是四条可拖的内缩线；槽位与装饰在这里不编辑，只画灰色参照框：把预览帧回执里的槽位、装饰矩形按轴映射反推回源图。
  **切到对应的预览才有参照框**——只有回执的 `skin.variant` 与本页签相同时才画（横排类看横排画布、竖排类看竖排画布，嵌入页签要开嵌入）；
  反推的是当前预览内容下的尺寸，要看最小尺寸时切「皮肤 · 1 条」预设。
- srcset 列表管理：坐标始终按主 `image` 写，查看其他份时按 density 比例换算显示。

### 6.3 预览叠加层

在 `MultiScreenPreview.vue` 的候选窗画布上加一层 SVG（与帧同尺寸、同缩放），仅在「皮肤编辑」开着时出现：

- 窗体框、内容框画细线，各带 9 个锚点圆点。
- 槽位：虚线框，拖动改 `offset`；拖到锚点圆点上（且指针移动超过 12 css px）松手，改 `anchor`、`offset` 归零。
  判距离用的点：显式写了 `pivot` 用 pivot；没写时引擎取 anchor 的方位作 pivot，吸附后 pivot 随新 anchor 变，所以用被拖物上「与圆点同方位」的点，落地不跳。
  按住 `Ctrl` 拖动不吸附。
- 装饰：外接框 + pivot 十字，拖动改 `offset`，按住 `Alt` 拖框角改 `size`（被拖角的对角不动，`offset` 随之补偿）。
  点中（不拖）装饰时，皮肤面板展开它的「动画」区块并滚到可见（按回执 `srcIndex` 对回模型）；反过来，面板里展开或聚焦
  某块的动画区块时，叠加层加粗高亮这块装饰。
- 对齐点：画光标标记，拖动写成 `{ anchor, offset }`；`avoid_caret` 保护带画成浅色条。
- 叠加层命中优先于 `nodes` 点选，点空白处落回原有点选。
- 拖动只改模型、出图仍走调度合并；一次拖动合成一步撤销（松手、取消、丢失指针捕获、窗口失焦都收尾，进行中不许撤销）。
- 继承自 base 的变体只画不拖，整层穿透给画布点选。

### 6.4 预览控制

- 预览面板加四项：
  - 变体（跟随页签）；
  - 嵌入开关；
  - 模拟 DPI（100 / 150 / 200 / 300%，即出图 `scale`；srcset 选档、`k`、拉伸带整数次重复都只按它）。编辑器 zoom
    不参与出图倍率，只把出好的帧按 CSS 放大显示——乘进去的话，放大预览就不再是那个 DPI 下实机的样子；
  - 内容预设（1 条 / 满页候选、长编码、带上方注释）。
- `sceneOpts` 补 `embedded`、`above`。

## 7. 变体

- 皮肤面板顶部四个页签：横排 / 竖排 / 横排嵌入 / 竖排嵌入。
  - 未配置的页签显示回退说明：横竖未配回退普通窗口，嵌入未配回退对应的非嵌入变体。
  - 页签上给「新建」与「从 ×× 复制」。
- 复制 = 深拷贝变体（含装饰与动画表），图片按路径共享不复制字节；目标已存在时确认覆盖。换 `frame.image` 后，超出新图尺寸的带与内缩标红。
- 切页签同时切预览：横排类用横排画布，竖排类用竖排画布，嵌入类打开嵌入开关。
  实际生效的变体以回执 `skin.variant` 为准，与页签不符时在画布角标出。
- base 继承：页签标「继承自 base」或「本主题覆盖」，并写明覆盖即整表替换。

## 8. 动画

### 8.1 帧信息与缩略条

wasm 新增两个模块级函数：

- `inspectImage(bytes) → { width, height, animated, frames: [{ ms }], truncated }`：`ms` 是引擎归一后的帧时长（`frame_ms`），
  `truncated` 表示超过了引擎的最大解帧数；
- `decodeFrames(bytes, maxEdge, from, to) → [{ w, h, rgba, ms }]`：面积平均缩到 `maxEdge` 的缩略帧，编辑器按可见区间分页取。

wasm 里已经编进了 `png` / `gif` / `webp` 解码器，这两个函数只是把 `image_cache::frames_of` 暴露出来。

缩略条：

- 装饰的「动画」区块展开时显示帧缩略条（帧号 + 时长；写了 `fps` 按 1/fps，钳到 33 ms – 60 s）。
  区块缺省对写了 `states` / `on` 的装饰展开，继承自 base 的变体也能展开查看；与叠加层的选中联动见 §6.3。
- 帧号与时长取自 `inspectImage`，各格一次列全；缩略帧按可见区间（再多取一页）分页取（一页 16 帧，滚动时补页）。
  动图只能从第 0 帧顺序解码，所以缺的相邻页并成一次 `decodeFrames`（一次至多 64 帧）；同一页只取一次、失败下次重取，
  按总字节（32 MiB）LRU 回收；图的缓存键换成短序号（data: 串可能有几 MB）。由 `lib/skin/frames.ts` 的 `FrameStore`
  承担，整页共用一份 `FrameTools`。引用按 `[resources]` 解析后的地址变了（改映射、换深浅色）就重取帧信息。
- 点选 / `Shift` 点选出区间，可「设为 states.××」（写 `{ frames = "a-b" }`，单帧写 `"a"`）或「设为 on.×× 的片段」
  （写 `play = "a-b"`，原来是 play 的保留 `then`）。原条目是认得的表（帧区间、文件、state 都算）就保留表上的其它字段。
- 条下用色带标出各状态与片段占用的区间，一个片段一行。
- 「播放」用缩略帧在编辑器本地循环该区间。

### 8.2 states / on 表

- `states` 表：名 / 片段类型 / 区间或文件。`idle` 置顶标「初始」；没有 `idle` 时，标出引擎会取按名排序的第一个。
- `on` 表：
  - 事件下拉列出引擎事件全集；`idle_<N>s` 带数字输入；`click` 只在 `hit` 为 drag / block 时可选。
  - 动作 = 切换状态，或播放片段 + `then`（缺省 `@prev`）。
- 认不出的条目、未知事件只读显示原文（按写回 TOML 的样子）+ 警告，可删；改别的条目时它们原样保留。
- 状态改名连带改同一装饰 `on` 表里的 `state = "旧名"` 与 `then = "旧名"`（`then = "@prev"` 是回原状态，不跟着改），
  整块装饰一次写回，算一步撤销；认不出的条目与未知事件原样保留。新名为空、与已有状态重名或是 `@prev` 时拒绝并提示。
  状态名按码点（= UTF-8 字节序，同引擎 BTreeMap）排序。
- 没有可引用的文件时「另一个文件」不可选，不写出 `{ image = "" }`。
- 动画区块的展开状态按装饰下标记，删装饰时后面的跟着前移；动画警告汇总到变体级列表，展开按钮上显示条数。
- 预算提示：24 MiB 帧预算按本变体要解帧的文件均分（同引擎 `skin_geom.rs` 的建帧计划）：装饰文件是动图计一份，
  `{ image }` 片段文件中的动图各计一份（装饰文件是静态图也照计；被引擎整条丢弃的 `on` 条目，如 `then` 不合法，不计）。
  帧数不全（超过解帧上限或解码预算）的动图照样计份，那一行标「帧数未知」、不估降帧。
  每份超了按引擎 `FrameBudget` 隔帧丢弃（帧数取上整的一半）直到落入份额。装饰目标尺寸同引擎：写了 `size` 按
  `size × k`，否则装饰文件源图 × `renderScale / 装饰密度`，片段文件按同一尺寸铺。`k` / `renderScale` 取最近一帧皮肤
  回执的值，没有回执时按模拟 DPI 与 `scale_with = "dpi"` 推；回执因帧闸降档出图时注明「预览降档，真机可能更紧」。
  不计超大装饰的位图上限钳制。引擎真降了，会经 `warnings` 再报一次：预览区常驻「引擎告警」列表，解析期告警取最近一次
  `setTheme` 的返回；帧告警按出图输入（方向 + 渲染参数去掉时刻：倍率、模拟内容…）分组记，显示两个方向当前输入那组，
  切到别的倍率 / 内容就不再显示（切回来那组还在），重新 `setTheme` 全清。引擎按场景去重的渲染期一次性告警，换了输入后
  可能不再出现，属已知取舍。
- 「模拟事件」按钮列表从两张画布本帧实际所用的变体（竖排画布不可见时只看横排；没出图时按 `effectiveVariant` 推）
  的 `on` 表生成，外加 `show` / `hide`；去重后按引擎事件表的顺序，`idle_<N>s` 按秒数排最后，同秒数的不同拼法
  （`idle_5s` / `idle_05s` / `idle_+5s`，引擎按 u32 解析视为同一事件）只留字节序靠前的那个。
  `click` 不算 `pass` 装饰上的（`hit` 没写或认不出按 drag），发的是不带下标的 `click`（所有写了 `click` 的装饰都收到），
  按钮标「点击（全部）」。另按每张画布本帧回执，drag / block 装饰各出一个「点击 装饰#k」（k 是 TOML 下标
  `srcIndex`，按它排；没写 `on.click` 的也出，点了没反应，同实机），发的是 `click:N`：N 取回执的 `index`（求值后按 z
  稳定排序的下标，与 k 不一定相同），只发给那张画布的预览器——两张画布的变体不同，同一个 N 指的不是同一块。两张画布
  都有按钮时前缀「横排·」「竖排·」。

## 9. 校验与提示

**错误**会阻止导出主题包、推送和发布（导入端本来就会拒）；**警告**只提示。每条都带定位。

| 检查 | 级别 | 数据来源 |
|---|---|---|
| 皮肤图引用不在 `assets/` 下（`..`、绝对路径、盘符）；大小写不同的同名路径 | 错误 | 模型 |
| 皮肤图引用 `data:` | 警告 + 一键移入资源仓 | 模型 |
| 引用的资源不存在 | 错误 | 清单 |
| 窗体图是 SVG | 错误 | 清单 mime |
| 包体：总解压 > 64 MiB、单图 > 16 MiB、文本 > 4 MiB、条目 > 200 | 错误；资源面板常驻用量条 | 清单字节数 |
| 拉伸带重叠、越出图、`from >= to` | 错误；越出图在导出 / 推送 / 发布时按主图文件头读尺寸（`imageHeader.ts`，不解码；GIF 取逻辑屏幕，同引擎画布；`[resources]` 深浅色两张按较小的查，只写一侧时同引擎 `Ld::select` 回退另一侧），读不出不拦、只给警告（TOML 通道同样提示，同一次推送只提示一次） | 模型 + 图尺寸 |
| 拉伸带外轮廓不恒定（`round` / `tile`） | 警告 | 窗体图像素 |
| 槽位互相重叠 | 警告；面板给「按 1 条 / 满页 / 长编码各查一遍」 | 回执 `skin.slots` |
| 装饰盖字（`z >= 100` 与 `item` / `index` / `preedit_bar` / `comment_above` / `footer_bar` 相交） | 提示 | 回执 `skin.decor` + `nodes` |
| 写了 `slots` 没写 `slots.candidates` | 警告 | 模型 |
| 帧区间越界、引用不存在的状态、未知事件；`click` 写在 `pass` 装饰上；同秒数的 `idle_<N>s` 写了多种拼法（只有字节序靠前的生效） | 警告（`lib/skin/anim.ts` 的 `decorAnimIssues`；认不出的条目与未知事件由 `strict.ts` 报）；帧数不全时不查越界，SVG 装饰按一帧查 | 模型 + `inspectImage` |
| 片段 `{ image = .. }` 指向不存在的文件 | 错误：与「引用的资源不存在」同一口径（包内 `assets/` 引用逐个查，不分在哪个键下），阻止导出、推送、发布 | 清单 |
| 其余引擎告警 | 警告（原文） | 回执 `warnings` |

拉伸带轮廓检查（`theme-skin-animation.md` §12 末段）由纯函数 `bandOutlineDrift(rgba, w, h, band, axis, alphaMin = 32)` 完成：

- `stretch_x` 的带覆盖列 `[from, to)`：逐列求最上、最下一个 alpha ≥ 32 的像素行，两者在带内的极差 > 1 px 即报，并给出漂移最大的列；
- `stretch_y` 对称地逐行求最左、最右。
- 只对 `round` / `tile` 报：`stretch` 只会把渐变轮廓拉成一条平滑斜边，不出锯齿。
- 像素取自 `createImageBitmap` + `OffscreenCanvas.getImageData`。

## 10. wasm API 增量（WindInput）

只加只读出口，不改渲染行为；几何只在 `render_candidate*` 出图时收集（同 `nodes`），实时窗口不受影响。

```text
Frame.skin?: {
  variant: "horizontal" | "vertical" | "horizontal_embedded" | "vertical_embedded",  // 实际所用（含回退）
  k: number, renderScale: number, frameSource: string, minSize: [w, h],
  frame: Rect, content: Rect,                                   // 设备像素，与 rgba 同坐标
  axisX / axisY: [{ src: [a, b], dst: [a, b], kind: "fixed" | "stretch" | "tile" | "round" }],
  slots: [{ name, box: "frame" | "content", rect }],           // free 布局才有
  decor: [{ index, rect, z, hit, clip?: { state, frame, raw } }],
  caret: { below: [x, y], above: [x, y] },
}
Frame.warnings: string[]        // 本次调用期间 `skin:` 前缀的 WARN 原文
Previewer.setTheme 同样回带 warnings
inspectImage(bytes) / decodeFrames(bytes, maxEdge, from, to)
```

- `wind-scene`：`SkinGeom::snapshot()` 返回只读拷贝（轴映射取 `skin::map_axis` 的结果），随 `nodes` 一起填进 `SceneFrame`。
- `wind-scene-wasm`：装一个最小 tracing `Subscriber`，只收 WARN 且消息以 `skin:` 开头的事件，存进线程局部缓冲，每次 `set_theme` / `render` 前清空、后取出。
  - `warn_skin_once` 的去重以场景为单位，`setTheme` 会重建场景；
  - 所以编辑器以「最近一次 `setTheme` 及其后首帧」的告警为准，之后的帧只并入、不清空。
- 新函数照 W1 设上限：输入 ≤ 16 MiB，`maxEdge` ≤ 256，一次取帧 ≤ 64。

## 11. 已定事项（维护者，2026-11-08）

| # | 事项 | 决定 |
|---|---|---|
| 1 | 资源仓范围 | 只管皮肤图与今后新导入的位图；已有 `data:` 图片不动，给手动「移入资源仓」按钮 |
| 2 | 非包出口 | 在线连接推送与市场发布都改走主题包，图片按文件传；已发布的 TOML 推送协议、TOML 下载照旧可用。工作区写回把图写进目录 `assets/`；复制 TOML 保留路径并提示需配合主题包 |
| 3 | 市场 | 本期开放皮肤主题，上限与包规则对齐（总 64 MiB、单图 16 MiB、文本 4 MiB、200 条） |
| 4 | wasm 告警 | tracing 收集，只收 `skin:` 前缀 WARN，引擎零改动 |
| 5 | WindInput 分支 | W3.3 在 `feat/theme-editor-wasm` 续做 |
| 6 | 测试依赖 | 编辑器可引入 devDependency `fake-indexeddb` |
| 7 | 不进 W3 | 精灵图、`slot.*` 装饰锚点、`max_width/height`、第三方皮肤导入 |
| 8 | base 继承 | 编辑器侧「变体整表替换」与引擎对齐，纳入 W3.2 |

## 12. 分期

| 期 | 涉及仓 | 内容 | 依赖 |
|---|---|---|---|
| W3.1 | 编辑器 | 资源仓 + 清单 + 本地出口（预览、包导入导出、工作区、复制 TOML）+ `validateThemePackage` + 包体自检 | — |
| W3.2 | 编辑器 | 皮肤一等模型、序列化、base 整表替换、`check:engine`、数值表单与变体页签、预览控制 | W3.1 |
| W3.3 | WindInput（产物检入编辑器） | wasm 回执 `skin` / `warnings`、`inspectImage` / `decodeFrames` | 无（与 W3.1、W3.2 并行） |
| W3.4 | 编辑器、wind-setting、市场后端（`worker/`）、部署 | 包通道：在线连接收包 / 出包、市场包版发布 / 存储 / 下载、一键导入认包链接 | W3.1（W3.2 之前之后均可） |
| W3.5 | 编辑器 | 源图编辑器、预览叠加层、几何类校验 | W3.2、W3.3 |
| W3.6 | 编辑器 | 动画：缩略条、states / on 表、预算提示、模拟事件列表 | W3.2、W3.3 |

建议顺序：W3.1 → W3.2 → W3.4，W3.3 同时起步，最后 W3.5 → W3.6。W3.4 早于可视化，是因为带图主题一旦能编辑，
就得能推到输入法上验，否则作者只能靠导出、双击导入来回绕。WindInput 在 W3.4 无改动（复用 `theme.importPackage` /
`theme.exportPackage`）；W3.3 是本期唯一改 WindInput 的一期。

### W3.1：资源仓（编辑器）

- 目标：位图字节不再进 localStorage；包导入导出、工作区走 `assets/` 路径；导出按导入口径自检。
- 验收：
  - 导入一个含 10 MiB 程序生成位图的主题包，连改 20 次字段后刷新，主题与图都在；该方案在 localStorage 的体积不随图大小增长。
  - 撤销「换图」回到旧图；删掉方案后，下次启动回收对应字节。
  - 「移入资源仓」把一张 `data:` 图改成 `assets/` 引用，一步可撤销。
  - 导出包：图片为存储模式；超 64 MiB / 16 MiB / 200 条，或引用缺失时被拦，并指出是哪个引用；
    编辑器产出的包能被 `wind-transfer` 的导入读回（做一次跨仓夹具）。
  - 工作区写回后，目录 `assets/` 里有清单中的图；重新载入目录，图回到资源仓。
  - IDB 不可用（测试注入失败）时，本会话照常工作并给出提示。
- 测试：vitest + `fake-indexeddb`，覆盖清单回收、哈希去重、自检各上限、导入导出往返。PNG 夹具在测试工具里用 fflate 拼，不用第三方素材。

### W3.2：皮肤模型、表单与变体（编辑器）

- 目标：`[skin]` 全字段可在属性栏编辑；四个变体页签与复制；预览可切变体、嵌入、模拟 DPI、内容预设；base 继承与引擎对齐。
- 验收：
  - `theme-skin-animation.md` §3.1、§6.2、§16 的示例，以及 `wind-theme/src/skin.rs` 测试里的 `SAMPLE`，经解析 → 序列化 → 解析结构相等；
    产出规整表头，`[[decor]]` 的子表紧随其后；每层塞一个未知字段，往返不丢。
  - 子主题只写 `skin.vertical` 时，base 的 `skin.horizontal` 保留、`skin.vertical` 被整表替换；与引擎 `replace_inherited_variants` 用同一份夹具对照。
  - `pnpm check:engine` 覆盖 8 个皮肤 struct、3 个表形态枚举、`SKIN_VARIANTS`、取值枚举、包上限常量；在编辑器侧故意删一个字段，它能红。
  - 嵌入页签打开时，预览以 `embedded: true` 出图。
- 测试：
  - `toml.test.ts` 把现有「透传往返」用例换成模型往返；
  - `merge` 加变体整替用例；
  - `strict.test.ts` 加未识别皮肤字段；
  - `sceneScheduler.test.ts` 补 `embedded` 透传。

### W3.3：wasm 皮肤回执与帧信息（WindInput，`feat/theme-editor-wasm`）

- 目标：§10 全部 API；同步编辑器 `scenePreview.ts` 的类型，重出 `public/scene-wasm/` 并更新 `VERSION`。
- 验收：
  - 程序生成的夹具皮肤（stack 与 free 各一份，各带一个外伸装饰），在 1x / 2x 下回执矩形与 `skin_geom` 的 `*_for_test` 一致，`k` 与 `renderScale` 正确；
    嵌入未配时 `variant` 报回退后的变体。
  - `inspectImage` 对 `anim_fixtures` 生成的 APNG / GIF（含一帧 0 ms）给出的帧数与时长，与 `anim_layout` 一致。
  - 窗体图倍率超限的主题，回执 `warnings` 带引擎原文；非 `skin:` 前缀的告警不进来。
  - 出厂主题 golden 不变，产物体积仍 < 3 MB。
- 测试：`wind-scene-wasm` 原生单测（mock 文字）；编辑器 `scenePreview.test.ts` 用检入产物核对回执有 `skin` 段。

### W3.4：包通道（编辑器 + wind-setting + 市场后端 + 部署）

- 目标：§5 全部；带图主题能推到输入法、从输入法拉回、发布到市场、经一键导入装上。纯 TOML 主题在三条通道上的行为不变。
- 各仓改动：
  - 编辑器：连接客户端发包 / 拉包，404 时回退；`PublishDialog` 发 multipart；`importUrl` 与 `fetchMarketTheme` 认包版；`packTheme` 改存储模式。
  - wind-setting：`theme_editor_server.rs` 新增两条路由，旧 push 加限长读取；`windinput://import/theme` 认包链接，走包下载 → `previewPackage` → 确认框 → `importPackage`。
  - 市场后端：multipart 发布 / 更新、包校验、`format` 列与迁移、包版下载、`computeThemeRev` 计入包哈希、`REV_SCHEME` 加一、管理后台列包内条目。
  - 部署：openresty `client_max_body_size` 放宽并实测。
  - WindInput：无改动。
- 验收：
  - 编辑器推一个含皮肤的主题（程序生成图，约 20 MiB）到本机输入法，装进用户主题目录且 `assets/` 齐全；推当前生效主题时 `reloaded = true`；
    同名回 409，确认后覆盖。
  - 从输入法拉回该主题，图片与清单完整。
  - 对老版本设置端（只有旧路由）：纯 TOML 推送照常；带图主题给出升级提示。
  - 市场：发布包版主题后，`/:id.wtheme` 能被 `wind-transfer` 导入，`package.toml` 署名取账号昵称；`/:id.toml` 回 404 说明。
    任取一个已发布的纯 TOML 主题，`/:id.toml` 字节不变、`/:id.wtheme` 能导入。
  - 超 64 MiB、单图超 16 MiB、皮肤引用缺文件的包被后端拒收，回 JSON 错误（不是代理的 HTML 413）。
  - 一键导入包版主题：设置端下载、弹确认框、装上；非官方域的 `.wtheme` 链接被拒。
- 测试：
  - wind-setting：路由与限长单测、协议包链接分流、临时文件清理；
  - 市场：`worker/test` 加包版发布 / 更新 / 下载 / rev 用例，以及纯 TOML 回归用例；
  - 编辑器：连接客户端回退逻辑（假 fetch）、`validateThemePackage` 与 `wind-transfer` 同一组越界夹具。

### W3.5：源图编辑器与叠加层（编辑器）

- 目标：§6.2、§6.3 的交互，以及 §9 中依赖几何与像素的校验。
- 验收：
  - 在程序生成的窗体图上拖出两条带、把其中一条改成 `round`：写回整数源像素且已排序，预览即时更新。
  - 在模拟 DPI 200%、zoom 150% 下把装饰拖 30 css px，写回的 `offset` 等于 `Δcss ÷ zoom × scale ÷ k`；拖到锚点圆点上松手，`anchor` 改变、`offset` 归零。
  - 「渐细飘带」夹具（带内轮廓逐列下移）放在 `round` 带上报轮廓警告，放在 `stretch` 带上不报；均匀框沿不报。
  - 两个槽位锚同一角时报重叠；`z = 120` 的装饰压在候选上时报盖字提示，`z = 50` 不报。
  - wasm 不可用时，可视化入口禁用并说明原因，数值表单照常可用。
- 测试：`coords.ts`、`bandOutlineDrift`、矩形求交的纯函数单测；拖动交互在 chrome 人工核对（随实现给出判据清单）。

### W3.6：动画编辑（编辑器）

- 目标：§8 全部。
- 验收：
  - 24 帧的程序生成 APNG 装饰显示 24 帧缩略与时长；`Shift` 选中 5–9 设为 `states.typing`，写回 `{ frames = "5-9" }`，色带显示该区间。
  - 区间改成 `20-30` 报越界；`on` 引用不存在的状态报警告；未知事件只读显示。
  - 「模拟事件」列表随 `on` 表变化；点 `typing` 后预览切到该片段。
  - 4 张大动图放在同一变体、模拟 DPI 300% 时出现降帧提示。
- 测试：states / on 的解析与写回（含认不出的条目保留）、区间选择转 `frames` 字符串、预算估算；缩略分页取帧用假预览模块测。

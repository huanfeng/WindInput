# 方案扩展词库的自动识别（细胞词库）

> 2026-10-09 设计稿，待维护者定稿。来源：rime-frost 细胞词库未使用的讨论；关联看板
> C2-1（扩展词库图形化增删，t68 / t246）、C2-36（一键挂载/启停，t177 / t214）、C2-66（一键导入热生效）。
> OpenCC 类转换（emoji / others / 中英 / 拆分）**不在本稿**，与 `${dict}` 注释的重叠另行规划。

## 1 目标与非目标

**目标**：用户把格式正确的词库文件放进方案的**指定目录**（用户目录覆盖层即可），设置端
「方案设置 → 扩展词库」里就自动出现这一项；**开启仍由用户在那里勾选**，出厂一律关。
全程不需要改方案文件或 `schema_overrides`。

**非目标**
- 不做文件监听：扫描发生在方案加载、`schema.getConfig` 时（core 本就没有 watcher）。
- 不做「导入向导」（拷文件 + 校验）：本稿先把「放进去就认」打通，向导是 C2-1 的下一步。
- 不决定是否随安装包附带 rime-frost 的 23 张细胞词库（§8 单列，需先实测）。

## 2 现状（2026-10-09 核查）

| 事实 | 位置 | 对本设计的影响 |
|---|---|---|
| 启停状态写在 `schema_overrides/<方案>.toml` 的 `dictionaries=[{id,enabled}]`，**只按 id 匹配方案文件里已声明的库**，未声明的 id 被静默丢弃 | `schema.rs:978-1009` | 发现的库必须在 `merge_toml` **之前**注入方案 base |
| 唯一写入口 `schema.setDictEnabled`，启用 ⇒ 失效重建、禁用 ⇒ 码表可热摘 | `webdata/lib.rs:1762`、`manager.rs:3667` | 直接复用 |
| 设置端扩展词库列表**不分引擎类型**：`getConfig` 里有非主库条目就显示 | `wind-setting schema_manager.rs:2698-2712` | 拼音方案注入后界面零改动即可出现 |
| 拼音**单库**走 `load_rime_pinyin_dict`：展开 `import_tables`，写音节边界 + 简拼段，内容指纹覆盖全部源 | `manager.rs:7301` | 细胞词库应走这条 |
| 拼音**多库**走 `load_merged_dicts`：枚举后 `add` 写回，**丢音节边界与简拼段**；产物 `combined.wdat` 按主库命名，**拼音与双拼共用一份、互相顶掉** | `manager.rs:7070-7153` | **不能**让细胞词库走这条 |
| `import_tables` 子表只按主表所在目录拼路径，不走分层 | `manager.rs:7236` | 用户目录里的子表不会被安装目录的主表引用 |
| 我们打包的 `rime_frost.dict.yaml` 原样带着上游完整 import 列表（含 `cn_dicts_cell/*`、`GB18030-2022`），实际只发了 6 张 ⇒ 每次启动 24 条 WARN | `scripts/dev.sh:447-454` | 构建时要裁掉未打包项 |
| 分层目录扫描已有现成函数（各层合并、靠前层遮蔽同名） | `Config::list_schema_resource_dir`（`config.rs:9454`） | 直接复用 |

## 3 用户可见行为

1. 用户把 `idiom.dict.yaml` 放到 `%APPDATA%\WindInput\schemas\pinyin\cn_dicts_cell\`。
2. 打开设置端「方案管理 → 全拼 → 方案设置 → 扩展词库」，列表里多出一项「idiom」（标题取
   词库头部的 `name:`），未勾选。全拼与双拼共用拼音主库，两个方案的列表里都会出现，各自独立开关。
3. 勾选并保存 ⇒ 该方案失效，下次使用时重建（大词库会弹「正在建立词库索引…」类提示，见 §6.3）。
4. 删掉文件 ⇒ 列表里消失；`schema_overrides` 里残留的 `{id, enabled}` 无害（被丢弃），不报错。

目录名沿用 rime-frost 的 `cn_dicts_cell`，用户可以把上游那个目录整个拷进来。

## 4 方案文件里声明「发现目录」

在方案文件里新增一段，声明哪个目录里的文件按什么类型自动识别：

```toml
# pinyin.schema.toml / shuangpin.schema.toml（两者主库都是 rime_frost）
[[dictionary_dirs]]
path = "pinyin/cn_dicts_cell"   # 相对 schemas/，各层合并扫描
type = "rime_pinyin"
```

```toml
# 码表方案同样适用，例如五笔
[[dictionary_dirs]]
path = "wubi86/ext"
type = "rime_codetable"
```

- 由**方案声明**而非全局约定：方案作者决定「本方案的扩展词库放哪、按什么解析」，码表/拼音通用。
- 用户想给某个方案开发现目录，可以在 `schema_overrides` 里补这一段（需让 `merge_toml` 接受
  `dictionary_dirs`；只追加、不删除出厂声明）。

## 5 识别规则（core）

注入点：`EngineManager::read_schema`，在 `merge_toml(override)` **之前**，对每个
`dictionary_dirs` 条目调用 `list_schema_resource_dir(path, ".dict.yaml")`，每个文件生成一个
`DictSpec`，追加到 `schema.dictionaries` 末尾：

| 字段 | 取值 |
|---|---|
| `id` | `dir:<相对 path 的文件主名>`，如 `dir:idiom`。带前缀，避免与方案手写的 id 撞 |
| `label` | 词库头部 `name:`（复用 `read_dict_head` / `dict_yaml_name`），空则文件主名 |
| `description` | 「自动识别：<相对路径>（用户目录/安装目录）」 |
| `path` | 相对 `schemas/` 的路径 |
| `type` | 继承 `dictionary_dirs.type` |
| `default` / `default_enabled` | `false` / `false`（出厂关） |
| `base_order` | 码表：排在全部手写扩展库之后（取已声明最大值 + 1 起递增）；拼音不读此字段 |

**跳过**：
- 方案里已有手写 `[[dictionaries]]` 指向同一路径的文件（手写声明优先）。
- 已被主库 `import_tables` 引用的文件——它们本就是主库的一部分。这条保证用户用上游完整的
  `rime_frost.dict.yaml` 覆盖主库时不会重复加载。

**扫描时机**：方案加载与 `schema.getConfig` 各扫一次，代价是一次目录列举加每个文件读头部几行；
不缓存，免得出现「放进去了设置页看不见」。

## 6 加载

### 6.1 拼音：细胞词库当作「动态追加的 import_tables」

启用的细胞词库**不**作为第二个 `[[dictionaries]]` 进 `load_merged_dicts`，而是在
`load_rime_pinyin_dict` 里，把它们的路径追加到主库展开出的源列表末尾：

- 自动获得音节边界、简拼段，以及按内容的源指纹（启停任何一张都会让指纹变化、触发重建）。
- 合并缓存文件名带上**启用集合的短哈希**：集合为空时仍是 `rime_frost.merged.wdat`（升级零变化），
  非空时为 `rime_frost.merged.<hash8>.wdat`。全拼与双拼各开各的不再互相顶掉。
- 旧哈希文件清理：成功写出新文件后，删掉同主库下最近未使用的哈希文件，只保留当前集合与空集合各一份。
- `enabled_dict_specs` / `load_dicts_individually` / `declared_dict_files` 三处选库口径要同步认这批库
  （反查索引、单字全码表、注释派生都要看见它们），沿用「同一函数出口」的纪律。

`load_merged_dicts` 丢边界与简拼是独立缺陷，本稿不修，但发现目录产生的库**绕开它**。

### 6.2 码表：直接走现有扩展层

码表扩展库已有完整机制（惰性加载、独立 wdat、热摘、影子层），发现的库就是普通扩展库，零新增。

### 6.3 重建成本

启用即失效重建。需要实测并写进本稿（S3 阶段）：
- 全拼主库 + idiom（4.9 万）/ place / composite（62 万）各自的重建耗时与构建峰值内存。
  参考：主库 65 万条合并峰值约 420 MB（`manager.rs:7370`）。
- 重建期间的提示：沿用「慢才弹」的延时提示模式（`INDEX_TOAST_DELAY` 同理），不新增无条件提示。

## 7 排序

细胞词库与主库同 text 时取较高权重（现状如此，主库词不会被压低）。细胞独有的词用自身权重，
抽样分布如下（rime-frost 上游，2026-10-09 下载）：

| 词库 | 条数 | 权重中位数 | p90 | 最大 |
|---|---:|---:|---:|---:|
| idiom | 48,714 | 0 | 2 | 5,052 |
| place | 6,396 | 1 | 19 | 20,906 |
| history | 19,957 | 0 | 4 | 1,715 |
| name2 | 18,360 | 0 | 2 | 1,535 |
| food | 144,437 | 0 | 1 | 119,454 |
| exthot | 2,080 | 1 | 31 | 788 |
| composite | 622,481 | 6 | 39 | 6,577 |

主库中位数约 200（`pinyin.schema.toml` 注释），补全与整句让位的绝对阈值是 100 / 50。
所以绝大多数细胞词天然排在后面，**风险在离群值**（food 的 11.9 万、place 的 2 万会越过主库常用词）。

建议 v1：细胞独有词的权重**封顶**到一个上限（候选：主库 p50，即约 200），参数先写死；
用 `pinyin_eval` 对比「无细胞 / 开 idiom+place / 开 composite」三组的界面序，逐条看得失后再定上限，
必要时改成每个发现目录可配。

## 8 随包附带（单列，后续决定）

- 构建期（`assemble_data`）裁掉打包的 `rime_frost.dict.yaml` 里**没有随包**的 import 项，消掉 24 条 WARN。
  这一步与本设计独立，可以先做。
- 是否随包附带细胞词库，以及附带哪几张：等 §6.3 / §7 的实测出来再定。附带的话就放进安装目录的
  `pinyin/cn_dicts_cell/`，靠发现机制出现在列表里，出厂关。

## 9 设置端（wind-setting）

列表本身不用改就会出现。小改动：
- label 为空时退回 id（现在行标题是空的）。
- 描述里显示来源层（用户目录/安装目录）。
- 本节加一个「打开扩展词库目录」按钮，打开用户目录下的发现目录（不存在就先建）。
- 设置对话框打开时重新 `getConfig` 已足够反映新文件，不另加刷新机制。

顺带修 CLI 口径不一致：`schema dict list` 在 `enabled` / `default_enabled` 都未写时按「启用」显示
（`schema_cli.rs:200`），引擎按「关闭」处理，统一成按引擎口径。

## 10 分阶段

| 阶段 | 内容 | 验证 |
|---|---|---|
| S1 | 构建期裁掉未打包的 import 项 | 启动日志无 import 缺失 WARN；合并缓存指纹不变（同一份数据不重建） |
| S2 | `dictionary_dirs` 声明 + 识别注入（§4、§5），码表直接可用 | 单测：分层遮蔽、手写优先、import 已引用跳过、override 开关生效、文件删除后消失 |
| S3 | 拼音动态 import + 哈希缓存名（§6.1），实测重建成本（§6.3） | 单测：边界/简拼保留、启停换指纹、全拼/双拼不互顶；靶机实测耗时与峰值 |
| S4 | 权重封顶 + pinyin_eval 评测（§7） | 三组评测逐条 diff，维护者看过再定上限 |
| S5 | 设置端小改动 + 文档站说明 | 设置端测试；文档站「扩展词库」页 |

## 11 已定（维护者 2026-10-09）

1. 目录名由每个方案在 `[[dictionary_dirs]]` 里自行声明；内置的全拼 / 双拼用 `cn_dicts_cell`。
2. 细胞独有词的权重上限：S3 前原样合并，S4 用 pinyin_eval 评测后再定。
3. 是否随包附带细胞词库：S3 / S4 实测后再定。

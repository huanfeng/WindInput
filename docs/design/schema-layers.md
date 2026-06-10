<!-- Generated: 2026-05-28 | Updated: 2026-05-28 -->

# 方案配置三层叠加架构

本文档约束**所有**对输入方案 (`*.schema.yaml`) 的修改入口。新增配置项、词库开关、扩展点等任何会"持久化用户对方案的调整"的功能，**必须**按本文档的层级语义选择写入位置。

## 三层体系

方案配置按"内置 → 用户方案 → 用户覆盖"自下而上叠加。最终生效的方案 = `merge(L1, L2, L3)`。

| 层 | 物理位置 | 语义 | 写入者 |
|---|---|---|---|
| **L1 内置方案** | `{exeDir}/schemas/<id>.schema.yaml` | 程序分发的完整方案，**禁止运行时改写** | 仅 release/构建流水线 |
| **L2 用户方案** | `{configDir}/schemas/<id>.schema.yaml` | 用户自带的第三方方案 / 完全独立的自定义方案；同 ID 时与 L1 合并（用户提供的字段覆盖 L1） | 用户手动放置、方案导入器 |
| **L3 用户覆盖** | `{configDir}/schema_overrides.toml` | **设置界面**对方案做的所有 diff，按 `{schemaID: 稀疏字段}` 索引 | 设置界面的"方案设置"对话框、附加词库开关、其它将来的 UI 调整入口 |

> `configDir` = `pkg/config.GetConfigDir()`，参见 `wind_input/pkg/config`。

## 黄金原则

> **设置界面对方案做的任何修改，一律写 L3，绝不碰 L1/L2。**

L1 是程序资产，升级时随发行覆盖；L2 是用户"另一个方案"。把设置界面的 diff 写进这两层都会污染语义边界。L3 的存在就是为了让"内置方案 + 用户调整"可分离、可重置、可随版本升级而保留用户改动。

### 反例（已修复）

历史上 `SetDictEnabled`（附加词库开关）曾把 `dictionaries[].enabled` 写到 L2 `{configDir}/schemas/<id>.schema.yaml`，造成：

1. 每次切换开关都在 L2 凭空造一个与 L1 同名的"伪用户方案"文件；
2. 为防止这个稀疏文件冲掉 L1 的完整 `label/path/type` 元数据，被迫在 `loader.go` 和 `wind_setting/app_schema.go` 各写一份 `mergeDictsByID` / `mergeSchemaConfigDicts` 防御性合并；
3. 与"方案设置"对话框其它字段（都走 L3）的位置不一致；
4. 内置方案后续升级（新增/重命名词库）时，L2 残留容易让用户陷入幽灵状态。

后已统一改写为：**`SetDictEnabled` 把 `dictionaries: [{id, enabled}]` 稀疏 diff 写进 L3**（参见 `wind_setting/app_schema.go`）。

## 实现细节

### 写入 L3 的标准模式

```go
// 1. 读 L3 现状（必须保留其他字段，避免相互覆盖）
override := map[string]any{}
if reply, err := rpcClient.ConfigGetSchemaOverride(schemaID); err == nil && reply != nil {
    override = reply.Data
}

// 2. 修改自己关心的字段
override["my_field"] = newValue

// 3. 写回（触发 wind_input 热重载）
rpcClient.ConfigSetSchemaOverride(schemaID, override)
```

`ConfigSetSchemaOverride` 是**整个方案 override 的全量替换**——任何写入入口都必须先读后写，否则会清掉其他写入入口的成果。`SaveSchemaConfig`（方案设置对话框保存）已经显式保留了 `dictionaries` 字段以避免冲掉 `SetDictEnabled` 的成果。

### 数组字段（dictionaries）的稀疏合并

YAML 数组 unmarshal 是**整体替换**而不是"按 id 合并"。如果 L3 写了 `dictionaries: [{id, enabled}]` 这种稀疏列表，朴素 unmarshal 会把 L1+L2 合并出的完整词库元数据（`label/path/type/role/...`）全部抹掉。

解决方案：**所有读取层叠加 L3 时，都要按 id patch dictionaries**，不要全量替换。统一实现：

- 引擎侧：`wind_input/internal/schema/loader.go:mergeDictsByID`，被 `manager.go` 在 L3 叠加路径调用
- 设置侧：`wind_setting/app_schema.go:mergeSchemaConfigDicts`，被 `loadSchemaBase` 和 `GetSchemaConfig` 在 L2 / L3 叠加路径调用

新增类似"按 id 合并数组"的字段（例如未来给方案加一组可配置的快捷键），请复用这两个 helper 的模式，**不要重新写一份用 `[]struct` 暴力 unmarshal 的合并**。

### 字段命名约定

L3 里允许出现的字段是 `SchemaConfig` 的子集（参见 `wind_setting/app_schema.go`）。设置界面新增任何持久化字段时：

1. 字段须能被 `yaml.Marshal/Unmarshal` 安全往返；
2. 对于"0 / false 是有效值"的字段，YAML tag **不能加 `omitempty`**（参见 `SchemaConfigFreq.ProtectTopN`、`SchemaConfigLearning.TempPromoteCount` 的注释），否则 `ComputeYAMLDiff` 会把它当默认值丢弃，导致 L3 不持久化、下次打开恢复 L1 默认值；
3. 写入入口要遵循"先读 L3 → 修改自己字段 → 写回"的模式。

## 决策树：我要持久化方案上的某个修改，写哪一层？

```
是用户在程序 UI 里改的吗？
├─ 是 → 写 L3（schema_overrides.toml）
│   ├─ 通过"方案设置"对话框？  → SaveSchemaConfig（计算 diff 进 L3）
│   └─ 单点开关 / 即时生效？    → 单独函数，按"读 L3 → patch → 写 L3"模式
└─ 否
    ├─ 是程序自带分发的资产？   → L1，随版本发布
    └─ 是用户手动导入/编辑的    → L2（用户自带方案）
       完整方案？                  禁止由程序代码写入
```

## 相关代码

| 文件 | 角色 |
|------|------|
| `wind_input/internal/schema/loader.go` | L1 + L2 合并，`mergeDictsByID` |
| `wind_input/internal/schema/manager.go` | L1+L2 之上叠加 L3 |
| `wind_input/pkg/config/schema_overrides.go` | L3 文件读写、按 schemaID 增删改 |
| `wind_setting/app_schema.go` | 设置界面读 L1+L2+L3 / 写 L3 的统一入口 (`GetSchemaConfig`、`SaveSchemaConfig`、`SetDictEnabled`、`ResetSchemaConfig`) |

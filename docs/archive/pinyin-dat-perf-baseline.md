# 拼音 DAT 性能基线数据

> 测试日期: 2026-04-22
> 测试环境: Windows 11, Debug 版便携安装
> 词库规模: 457,038 条拼音词条 (rime-ice)
> 测试输入: 单字母 "s"（最差前缀场景）

## 优化历程

| 阶段 | 首字母 "s" 耗时 | 候选数 | 说明 |
|------|----------------|--------|------|
| 原始（无优化） | 偏慢/卡顿 | 7183 页 | 混输临时拼音泄漏 + 全量 Trie 遍历 |
| 修复泄漏 + prefixSafeLimit=300 | ~89ms | 181 | wdb 二分 + Trie 收集 300 条 |
| DAT 替代 wdb（未传 limit） | ~109ms (冷) / 53ms (热) | 162 | DAT 遍历全子树 |
| DAT + LookupPrefix 传 limit | ~53ms (冷) / 12ms (热) | 162 | DAT 收集 limit*2 叶节点 |

## 分步耗时（热缓存，第 2-3 次输入 "s"）

| 步骤 | 耗时 | 说明 |
|------|------|------|
| step0_cmd | 0ms | 命令匹配 |
| step0b_viterbi | 0ms | 造句（跳过：无完成音节） |
| step1_exact | 0ms | 精确匹配（跳过） |
| step1b_altsplit | 0ms | 多切分（跳过） |
| step2_subphrase | 0ms | 子词组（跳过） |
| step4_singlechar | 0ms | 单字（跳过） |
| **step5a_prefix** | **1.3ms** | LookupPrefix("s", 30) 通过 DAT |
| step5b_expand | 0.5ms | GetPossibleSyllables 展开 ~35 音节 |
| **step5_total** | **1.8ms** | 步骤 5 合计 |
| step6_abbrev | 0ms | 简拼（跳过：单字母不触发） |
| codeHints | 0ms | 编码提示（热缓存） |
| **总计** | **~12ms** | convertCore 全流程 |

## 分步耗时（冷启动，首次输入 "s"）

| 步骤 | 耗时 | 说明 |
|------|------|------|
| step5a_prefix | 1.7ms | DAT 前缀查询 |
| step5_total | 2.1ms | 步骤 5 合计 |
| **codeHints** | **43ms** | 反查码表 mmap 首次页面载入 |
| **总计** | **~53ms** | 一次性开销，后续为 ~12ms |

## 架构说明

- 索引格式: DAT (Double-Array Trie), .wdat 文件, mmap 零拷贝
- 前缀查询限制: CompositeDict prefixSafeLimit=300, DAT leafLimit=limit*2
- 用户词库/临时词库/Shadow/词频: 不受 DAT 影响，仍使用 bbolt
- DAT 通过 `dict_format: "dat"` 配置启用，默认仍为 wdb

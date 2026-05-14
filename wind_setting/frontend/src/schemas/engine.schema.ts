// 方案引擎设置的 Schema 声明
// 对应 SchemaSettingsDialog.vue 中三种引擎类型的设置项
//
// 注意：
//   - 模糊音矩阵（FuzzyDialog）、方案引用展示、混输引用提示 保留手写
//   - 智能学习字段在码表/拼音中均出现，用两条独立条目区分 engines+tab

import type { EngineSchema } from './schema-engine-types'

// 晋升次数选项（0~10）
const promoteCounts = [
  { value: '0', label: '永不晋升' },
  ...Array.from({ length: 10 }, (_, i) => ({
    value: String(i + 1),
    label: `${i + 1} 次`,
  })),
]

export const engineSchema: EngineSchema = [
  // ════════════════════════════════════════════════════════════
  // 码表引擎 - 基础 Tab
  // ════════════════════════════════════════════════════════════
  { type: 'section', label: '上屏行为', engines: ['codetable'], tab: 'basic' },
  {
    type: 'toggle',
    key: 'engine.codetable.auto_commit_unique',
    label: '满码唯一自动上屏',
    hint: (cfg) => {
      const len = cfg?.engine?.codetable?.max_code_length || 4
      return `输入达到最大码长（${len}码）且只有唯一候选时自动上屏`
    },
    engines: ['codetable'],
    tab: 'basic',
  },
  {
    type: 'toggle',
    key: 'engine.codetable.clear_on_empty_max',
    label: '满码空码清空',
    hint: (cfg) => {
      const len = cfg?.engine?.codetable?.max_code_length || 4
      return `输入达到最大码长（${len}码）无匹配时自动清空`
    },
    engines: ['codetable'],
    tab: 'basic',
  },
  {
    type: 'toggle',
    key: 'engine.codetable.top_code_commit',
    label: '顶码上屏',
    hint: (cfg) => {
      const len = cfg?.engine?.codetable?.max_code_length || 4
      return `超过最大码长（${len}码）时自动上屏首选`
    },
    engines: ['codetable'],
    tab: 'basic',
  },
  {
    type: 'toggle',
    key: 'engine.codetable.punct_commit',
    label: '标点顶码上屏',
    hint: '输入标点时自动上屏首选',
    engines: ['codetable'],
    tab: 'basic',
  },

  { type: 'section', label: '输入模式', engines: ['codetable'], tab: 'basic' },
  {
    type: 'toggle',
    key: 'engine.codetable.single_code_input',
    label: '精确匹配',
    hint: '关闭前缀匹配，仅显示精确匹配',
    engines: ['codetable'],
    tab: 'basic',
  },
  {
    type: 'toggle',
    key: 'engine.codetable.single_code_complete',
    label: '精确匹配时空码补全',
    hint: '精确匹配下无候选时，从更长编码中取首个候选',
    engines: ['codetable'],
    tab: 'basic',
    dependsOn: (cfg) => !!(cfg?.engine?.codetable?.single_code_input),
  },

  { type: 'section', label: '常用功能', engines: ['codetable'], tab: 'basic' },
  {
    type: 'toggle',
    key: 'engine.codetable.z_key_repeat',
    label: 'Z键重复上屏',
    hint: '输入z时首选为上一次上屏的内容，快速重复输入',
    engines: ['codetable'],
    tab: 'basic',
  },
  {
    type: 'toggle',
    key: 'engine.codetable.temp_pinyin.enabled',
    label: '临时拼音',
    hint: '通过触发键临时切换拼音输入，用于查找不会打的字',
    engines: ['codetable'],
    tab: 'basic',
  },
  {
    type: 'toggle',
    key: 'engine.codetable.show_code_hint',
    label: '显示编码提示',
    hint: '在前缀匹配的候选词旁显示剩余编码',
    engines: ['codetable'],
    tab: 'basic',
  },

  { type: 'section', label: '智能学习', engines: ['codetable'], tab: 'basic' },
  {
    type: 'toggle',
    key: 'learning.freq.enabled',
    label: '自动调频',
    hint: '根据使用频率自动调整候选词排序',
    engines: ['codetable'],
    tab: 'basic',
  },
  {
    type: 'select',
    key: 'learning.freq.protect_top_n',
    label: '首选保护',
    hint: '锁定前 N 位候选的排序位置，防止调频改变首选',
    width: '140px',
    engines: ['codetable'],
    tab: 'basic',
    dependsOn: (cfg) => !!(cfg?.learning?.freq?.enabled),
    options: [
      { value: '0', label: '不保护' },
      { value: '1', label: '保护首选' },
      { value: '2', label: '保护前2位' },
      { value: '3', label: '保护前3位' },
    ],
  },
  {
    type: 'toggle',
    key: 'engine.codetable.skip_single_char_freq',
    label: '单字不调频',
    hint: '防止高频单字打乱码表顺序',
    engines: ['codetable'],
    tab: 'basic',
    dependsOn: (cfg) => !!(cfg?.learning?.freq?.enabled),
  },
  {
    type: 'toggle',
    key: 'learning.auto_learn.enabled',
    label: '自动造词',
    hint: '连续输入单字后以标点、词组或回车结束时，自动将单字序列组词并加入临时词库',
    engines: ['codetable'],
    tab: 'basic',
  },
  {
    type: 'select',
    key: 'learning.temp_promote_count',
    label: '晋升用户词库次数',
    hint: '临时词被选中达到该次数后晋升到用户词库；0 表示永不晋升（始终留在临时词库）',
    width: '140px',
    engines: ['codetable'],
    tab: 'basic',
    dependsOn: (cfg) => !!(cfg?.learning?.auto_learn?.enabled),
    options: promoteCounts,
  },

  // ════════════════════════════════════════════════════════════
  // 码表引擎 - 高级 Tab
  // ════════════════════════════════════════════════════════════
  { type: 'section', label: '候选行为', engines: ['codetable'], tab: 'advanced' },
  {
    type: 'select',
    key: 'engine.codetable.candidate_sort_mode',
    label: '候选排序',
    hint: '候选词的排列方式（许多词库依赖默认顺序，请勿随意修改）',
    width: '140px',
    engines: ['codetable'],
    tab: 'advanced',
    options: [
      { value: 'frequency', label: '词频优先' },
      { value: 'natural',   label: '原始顺序' },
    ],
  },
  {
    type: 'select',
    key: 'engine.codetable.charset_preference',
    label: '候选字符偏好',
    hint: '特定情况下的候选词组或单字优先',
    width: '140px',
    engines: ['codetable'],
    tab: 'advanced',
    options: [
      { value: 'none',                  label: '无偏好' },
      { value: 'single_first',          label: '单字绝对优先' },
      { value: 'phrase_first',          label: '词组绝对优先' },
      { value: 'full_code_phrase_first',label: '满码词组优先' },
    ],
  },
  {
    type: 'toggle',
    key: 'engine.codetable.short_code_first',
    label: '短码优先',
    hint: '前缀匹配时对较长的候选词施加降权惩罚',
    engines: ['codetable'],
    tab: 'advanced',
  },
  {
    type: 'toggle',
    key: 'engine.codetable.dedup_candidates',
    label: '候选去重',
    hint: '合并相同文字的多个候选词',
    engines: ['codetable'],
    tab: 'advanced',
  },

  { type: 'section', label: '底层设置', engines: ['codetable'], tab: 'advanced' },
  {
    type: 'select',
    key: 'engine.codetable.prefix_mode',
    label: '前缀匹配模式',
    hint: '分层扫描：按编码长度逐层补足候选，覆盖更全；传统顺序：按词库存储顺序线性扫描，行为更可预测',
    width: '140px',
    engines: ['codetable'],
    tab: 'advanced',
    options: [
      { value: 'bfs_bucket', label: '分层扫描(推荐)' },
      { value: 'sequential', label: '传统顺序' },
    ],
  },
  {
    type: 'select',
    key: 'engine.codetable.weight_mode',
    label: '权重解释策略',
    hint: '处理词库内词频权重的规则',
    width: '140px',
    engines: ['codetable'],
    tab: 'advanced',
    options: [
      { value: 'auto',        label: '自动判定' },
      { value: 'global_freq', label: '全局词频' },
      { value: 'inner_order', label: '仅同码内排序' },
    ],
  },
  {
    type: 'select',
    key: 'engine.codetable.load_mode',
    label: '加载模式',
    hint: '内存占用与极致查询速度的权衡',
    width: '140px',
    engines: ['codetable'],
    tab: 'advanced',
    options: [
      { value: 'mmap',   label: '节约内存(mmap)' },
      { value: 'memory', label: '极速(全内存)' },
    ],
  },

  // ════════════════════════════════════════════════════════════
  // 拼音引擎
  // ════════════════════════════════════════════════════════════
  // 双拼布局：hidden 当非双拼时
  {
    type: 'select',
    key: 'engine.pinyin.shuangpin.layout',
    label: '双拼方案',
    hint: '选择双拼键位布局',
    width: '140px',
    engines: ['pinyin'],
    hidden: (cfg) => cfg?.engine?.pinyin?.scheme !== 'shuangpin',
    options: [
      { value: 'xiaohe',  label: '小鹤双拼' },
      { value: 'ziranma', label: '自然码' },
      { value: 'mspy',    label: '微软双拼' },
      { value: 'sogou',   label: '搜狗双拼' },
      { value: 'abc',     label: '智能ABC' },
      { value: 'ziguang', label: '紫光双拼' },
    ],
  },
  {
    type: 'toggle',
    key: 'engine.pinyin.show_code_hint',
    label: '编码反查提示',
    hint: '在候选词旁显示对应的码表编码',
    engines: ['pinyin'],
  },
  {
    type: 'toggle',
    key: 'engine.pinyin.use_smart_compose',
    label: '智能组句',
    hint: '使用语言模型优化多字词组匹配',
    engines: ['pinyin'],
  },
  // 模糊音：checkbox+button 保留手写
  {
    type: 'toggle',
    key: 'learning.freq.enabled',
    label: '自动调频',
    hint: '根据使用频率自动调整候选词排序',
    engines: ['pinyin'],
  },
  {
    type: 'toggle',
    key: 'learning.auto_learn.enabled',
    label: '自动造词',
    hint: '选词时自动学习新词组，先加入临时词库，多次使用后晋升到用户词库',
    engines: ['pinyin'],
  },
  {
    type: 'select',
    key: 'learning.temp_promote_count',
    label: '晋升用户词库次数',
    hint: '临时词被选中达到该次数后晋升到用户词库；0 表示永不晋升',
    width: '140px',
    engines: ['pinyin'],
    dependsOn: (cfg) => !!(cfg?.learning?.auto_learn?.enabled),
    options: promoteCounts,
  },

  // ════════════════════════════════════════════════════════════
  // 混输引擎 - 非引用式：码表/拼音设置（引用式手写提示）
  // ════════════════════════════════════════════════════════════
  { type: 'section', label: '码表设置', engines: ['mixed'] },
  {
    type: 'toggle',
    key: 'engine.codetable.show_code_hint',
    label: '显示编码提示',
    hint: '在前缀匹配的候选词旁显示剩余编码',
    engines: ['mixed'],
  },
  {
    type: 'toggle',
    key: 'engine.codetable.punct_commit',
    label: '标点顶码上屏',
    hint: '输入标点时自动上屏首选',
    engines: ['mixed'],
  },
  {
    type: 'select',
    key: 'engine.codetable.candidate_sort_mode',
    label: '候选排序',
    hint: '码表候选词的排列方式',
    width: '140px',
    engines: ['mixed'],
    options: [
      { value: 'frequency', label: '词频优先' },
      { value: 'natural',   label: '原始顺序' },
    ],
  },
  {
    type: 'select',
    key: 'engine.codetable.prefix_mode',
    label: '前缀匹配模式',
    hint: '输入未完成时的提示逻辑',
    width: '140px',
    engines: ['mixed'],
    options: [
      { value: 'bfs_bucket', label: '分层扫描(推荐)' },
      { value: 'sequential', label: '传统顺序' },
      { value: 'none',       label: '关闭' },
    ],
  },
  {
    type: 'select',
    key: 'engine.codetable.weight_mode',
    label: '权重解释策略',
    hint: '处理词库内词频权重的规则',
    width: '140px',
    engines: ['mixed'],
    options: [
      { value: 'auto',        label: '自动判定' },
      { value: 'global_freq', label: '全局词频' },
      { value: 'inner_order', label: '仅同码内排序' },
    ],
  },
  {
    type: 'select',
    key: 'engine.codetable.charset_preference',
    label: '候选字符偏好',
    hint: '特定情况下的候选词组或单字优先',
    width: '140px',
    engines: ['mixed'],
    options: [
      { value: 'none',                  label: '无偏好' },
      { value: 'single_first',          label: '单字绝对优先' },
      { value: 'phrase_first',          label: '词组绝对优先' },
      { value: 'full_code_phrase_first',label: '满码词组优先' },
    ],
  },
  {
    type: 'toggle',
    key: 'engine.codetable.short_code_first',
    label: '短码优先提示',
    hint: '前缀匹配时对较长的候选词施加降权惩罚',
    engines: ['mixed'],
  },
  {
    type: 'select',
    key: 'engine.codetable.load_mode',
    label: '加载模式',
    hint: '内存占用与极致查询速度的权衡',
    width: '140px',
    engines: ['mixed'],
    options: [
      { value: 'mmap',   label: '节约内存(mmap)' },
      { value: 'memory', label: '极速(全内存)' },
    ],
  },

  { type: 'section', label: '拼音设置', engines: ['mixed'] },
  {
    type: 'toggle',
    key: 'engine.pinyin.show_code_hint',
    label: '编码反查提示',
    hint: '在拼音候选词旁显示对应的码表编码',
    engines: ['mixed'],
  },
  {
    type: 'toggle',
    key: 'engine.pinyin.use_smart_compose',
    label: '智能组句',
    hint: '使用语言模型优化多字词组匹配',
    engines: ['mixed'],
  },
  // 混输的模糊音：checkbox+button 保留手写

  // 混输专属（引用式和非引用式都显示，由 SchemaEngineRenderer 外层控制条件）
  { type: 'section', label: '混输设置', engines: ['mixed'] },
  {
    type: 'select',
    key: 'engine.mixed.min_pinyin_length',
    label: '拼音最小触发长度',
    hint: '输入几码后开始查询拼音候选（1=始终查询，2=两码起查询）',
    width: '140px',
    engines: ['mixed'],
    options: [
      { value: '1', label: '1码' },
      { value: '2', label: '2码' },
      { value: '3', label: '3码' },
    ],
  },
  {
    type: 'toggle',
    key: 'engine.mixed.show_source_hint',
    label: '显示来源标记',
    hint: '在拼音候选旁显示"拼"标记以区分来源',
    engines: ['mixed'],
  },
  {
    type: 'toggle',
    key: 'engine.mixed.enable_abbrev_match',
    label: '简拼匹配',
    hint: '允许输入声母缩写查找拼音候选（如 bg 匹配"不过"）',
    engines: ['mixed'],
  },
  {
    type: 'toggle',
    key: 'engine.mixed.z_key_repeat',
    label: 'Z键重复上屏',
    hint: '输入z时首选为上一次上屏的内容，快速重复输入',
    engines: ['mixed'],
  },
]

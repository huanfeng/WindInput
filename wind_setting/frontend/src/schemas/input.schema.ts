import type { PageSchema } from './types'
import {
  EnterBehavior,
  SpaceOnEmptyBehavior,
  OverflowBehavior,
  FilterMode,
  PinyinSeparatorMode,
  NumpadBehavior,
  ShiftBehavior,
  StatusDisplayMode,
} from '@/lib/enums'

export const inputSchema: PageSchema = [
  // ── 字符与标点 ────────────────────────────────────────────
  { type: 'card', label: '字符与标点' },
  {
    type: 'select',
    key: 'input.filter_mode',
    label: '候选检索范围',
    hint: '过滤候选词中的生僻字',
    width: '200px',
    options: [
      { value: FilterMode.Smart,   label: '智能模式',  tag: '推荐' },
      { value: FilterMode.General, label: '仅常用字' },
      { value: FilterMode.GB18030, label: '全部字符' },
    ],
  },
  {
    type: 'toggle',
    key: 'input.punct_follow_mode',
    label: '标点随中英文切换',
    hint: '切换到中文模式时自动切换中文标点',
  },
  {
    type: 'toggle',
    key: 'input.smart_punct_after_digit',
    label: '数字后智能标点',
    hint: '数字后句号输出点号、逗号输出英文逗号，方便输入 IP、小数、千分位等',
  },

  // ── 按键行为 ──────────────────────────────────────────────
  { type: 'card', label: '按键行为' },
  {
    type: 'select',
    key: 'input.enter_behavior',
    label: '回车键功能',
    hint: '有编码时按回车键的处理方式',
    options: [
      { value: EnterBehavior.Commit, label: '上屏编码' },
      { value: EnterBehavior.Clear,  label: '清空编码' },
    ],
  },
  {
    type: 'select',
    key: 'input.space_on_empty_behavior',
    label: '空码时空格键功能',
    hint: '无候选词时按空格键的处理方式',
    options: [
      { value: SpaceOnEmptyBehavior.Commit, label: '上屏编码' },
      { value: SpaceOnEmptyBehavior.Clear,  label: '清空编码' },
    ],
  },
  {
    type: 'select',
    key: 'input.numpad_behavior',
    label: '数字小键盘功能',
    hint: '控制小键盘数字键的行为，选择"同主键盘区数字"后可用于候选选择和快捷输入',
    width: '200px',
    options: [
      { value: NumpadBehavior.Direct,     label: '直接输入数字' },
      { value: NumpadBehavior.FollowMain, label: '同主键盘区数字' },
    ],
  },

  // ── 候选无效按键 ──────────────────────────────────────────
  { type: 'card', label: '候选无效按键' },
  {
    type: 'select',
    key: 'input.overflow_behavior.number_key',
    label: '数字键无效时',
    hint: '按的数字超出当前页候选数量时的处理方式',
    options: [
      { value: OverflowBehavior.Ignore,        label: '不起作用' },
      { value: OverflowBehavior.Commit,        label: '候选上屏' },
      { value: OverflowBehavior.CommitAndInput, label: '顶码上屏' },
    ],
  },
  {
    type: 'select',
    key: 'input.overflow_behavior.select_key',
    label: '次选三选键无效时',
    hint: '候选数量不足时按次选或三选键的处理方式',
    options: [
      { value: OverflowBehavior.Ignore,        label: '不起作用' },
      { value: OverflowBehavior.Commit,        label: '候选上屏' },
      { value: OverflowBehavior.CommitAndInput, label: '顶码上屏' },
    ],
  },
  {
    type: 'select',
    key: 'input.overflow_behavior.select_char_key',
    label: '以词定字键无效时',
    hint: '候选词长度不足时按以词定字键的处理方式',
    options: [
      { value: OverflowBehavior.Ignore,        label: '不起作用' },
      { value: OverflowBehavior.Commit,        label: '候选上屏' },
      { value: OverflowBehavior.CommitAndInput, label: '顶码上屏' },
    ],
  },

  // ── 快捷输入（trigger_keys 手写，以下两项 schema 驱动）──
  // 注意：这两条在 InputPage 中以 bare 模式嵌入到快捷输入 card 内部
  { type: 'card', label: '__quick_input_extra__' }, // 占位符，不实际使用
  {
    type: 'toggle',
    key: 'input.quick_input.force_vertical',
    label: '强制竖排显示',
    hint: '快捷输入时候选窗口强制使用竖排布局，退出后恢复原布局',
    dependsOn: (cfg) => cfg.input.quick_input.trigger_keys.length > 0,
  },
  {
    type: 'number-input',
    key: 'input.quick_input.decimal_places',
    label: '小数保留位数',
    hint: '计算结果最多保留的小数位数（0 表示取整）',
    min: 0,
    max: 6,
    dependsOn: (cfg) => cfg.input.quick_input.trigger_keys.length > 0,
  },

  // ── 临时拼音（trigger_keys 手写，分隔符 schema 驱动）────
  { type: 'card', label: '__pinyin_extra__' }, // 占位符，不实际使用
  {
    type: 'select',
    key: 'input.pinyin_separator',
    label: '拼音分隔符',
    hint: "拼音模式下用于消歧的分隔符，如输入 xi'an 得到「西安」",
    width: '280px',
    options: [
      { value: PinyinSeparatorMode.Auto,    label: "自动（' 被选择键占用时改用 `）" },
      { value: PinyinSeparatorMode.Quote,   label: "单引号 ( ' )" },
      { value: PinyinSeparatorMode.Backtick,label: '反引号 ( ` )' },
      { value: PinyinSeparatorMode.None,    label: '不使用' },
    ],
  },

  // ── 临时英文（trigger_keys 手写，以下两项 schema 驱动）──
  { type: 'card', label: '__shift_extra__' }, // 占位符，不实际使用
  {
    type: 'select',
    key: 'input.shift_temp_english.shift_behavior',
    label: 'Shift+字母行为',
    hint: '中文模式下按 Shift+字母时的行为',
    width: '240px',
    options: [
      { value: ShiftBehavior.TempEnglish,   label: '进入临时英文模式' },
      { value: ShiftBehavior.DirectCommit,  label: '直接上屏大写字母' },
    ],
  },
  {
    type: 'toggle',
    key: 'input.shift_temp_english.show_english_candidates',
    label: '显示英文候选',
    hint: '临时英文模式下查询英文词库显示候选词',
  },
  {
    type: 'toggle',
    key: 'input.shift_temp_english.allow_symbols',
    label: '允许输入符号与数字',
    hint: '可输入下划线、点号等符号，便于书写标识符或代码',
  },
  {
    type: 'toggle',
    key: 'input.shift_temp_english.space_as_input',
    label: '空格作为输入字符',
    hint: '空格不再上屏，可连续输入多个单词，回车上屏',
  },

  // ── 默认状态（segmented controls 手写，记忆状态 schema 驱动）──
  { type: 'card', label: '__startup_extra__' }, // 占位符，不实际使用
  {
    type: 'toggle',
    key: 'startup.remember_last_state',
    label: '记忆前次状态',
    hint: '启用后恢复上次的中英文、全半角和标点状态',
  },
]

// 按 card 分组，供页面按需取用
// 各 card 的 key 前缀用于 InputPage 中 bare 模式渲染

/** 字符与标点卡片内的字段（bare 模式） */
export const punctSchema: PageSchema = inputSchema.slice(1, 4)

/** 按键行为卡片内的字段（full 卡片） */
export const keyBehaviorSchema: PageSchema = inputSchema.slice(5, 8)

/** 候选无效按键卡片内的字段（full 卡片） */
export const overflowSchema: PageSchema = inputSchema.slice(9, 12)

/** 快捷输入卡片内的额外字段（bare 模式，trigger_keys 手写在前） */
export const quickInputExtraSchema: PageSchema = inputSchema.slice(13, 15)

/** 临时拼音卡片内的分隔符字段（bare 模式） */
export const pinyinSeparatorSchema: PageSchema = inputSchema.slice(16, 17)

/** 临时英文卡片内的额外字段（bare 模式） */
export const shiftExtraSchema: PageSchema = inputSchema.slice(18, 22)

/** 默认状态卡片内的记忆字段（bare 模式） */
export const startupExtraSchema: PageSchema = inputSchema.slice(23, 24)

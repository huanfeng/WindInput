// enums.ts — 前端配置枚举常量的单一权威来源。
//
// 这些常量值与后端 Go 端的字面量必须保持一致（见 wind_input/pkg/config/enums.go、
// wind_input/pkg/keys/keys.go、wind_input/pkg/keys/pair.go），因为它们是 YAML/JSON
// 协议字段值。本文件的目的是消除前端 Vue/TS 代码里散落的字符串字面量，提供集中
// 修改点。
//
// 用法：
//   import { EnterBehavior, FilterMode, ... } from '@/lib/enums';
//   if (cfg.enter_behavior === EnterBehavior.Commit) { ... }
//
// 注意：模板中的 `<SelectItem value="commit">` 等字面量出于"展示+本地化文案"
// 性质保留，但 `<script>` 中的逻辑比较应使用本文件的常量。

// ============================================================
// 行为枚举（对应 pkg/config/enums.go）
// ============================================================

export const EnterBehavior = {
  Commit: "commit",
  Clear: "clear",
  CommitAndInput: "commit_and_input",
  Ignore: "ignore",
} as const;
export type EnterBehaviorValue =
  (typeof EnterBehavior)[keyof typeof EnterBehavior];

export const SpaceOnEmptyBehavior = {
  Commit: "commit",
  Clear: "clear",
  CommitAndInput: "commit_and_input",
  Ignore: "ignore",
} as const;
export type SpaceOnEmptyBehaviorValue =
  (typeof SpaceOnEmptyBehavior)[keyof typeof SpaceOnEmptyBehavior];

export const OverflowBehavior = {
  Ignore: "ignore",
  Commit: "commit",
  CommitAndInput: "commit_and_input",
} as const;
export type OverflowBehaviorValue =
  (typeof OverflowBehavior)[keyof typeof OverflowBehavior];

export const FilterMode = {
  Smart: "smart",
  General: "general",
  GB18030: "gb18030",
} as const;
export type FilterModeValue = (typeof FilterMode)[keyof typeof FilterMode];

export const ThemeStyle = {
  System: "system",
  Light: "light",
  Dark: "dark",
} as const;
export type ThemeStyleValue = (typeof ThemeStyle)[keyof typeof ThemeStyle];

export const CandidateLayout = {
  Horizontal: "horizontal",
  Vertical: "vertical",
} as const;
export type CandidateLayoutValue =
  (typeof CandidateLayout)[keyof typeof CandidateLayout];

export const PreeditMode = {
  Top: "top",
  Embedded: "embedded",
} as const;
export type PreeditModeValue = (typeof PreeditMode)[keyof typeof PreeditMode];

export const PinyinSeparatorMode = {
  Auto: "auto",
  Quote: "quote",
  Backtick: "backtick",
  None: "none",
} as const;
export type PinyinSeparatorModeValue =
  (typeof PinyinSeparatorMode)[keyof typeof PinyinSeparatorMode];

export const FontEngine = {
  DirectWrite: "directwrite",
  GDI: "gdi",
  Freetype: "freetype",
} as const;
export type FontEngineValue = (typeof FontEngine)[keyof typeof FontEngine];

export const PagerDisplayMode = {
  Default: "",       // 使用主题配置
  Never: "never",    // 不显示页码文字
  Auto: "auto",      // 仅在多页时显示
  Always: "always",  // 总是显示
} as const;
export type PagerDisplayModeValue =
  (typeof PagerDisplayMode)[keyof typeof PagerDisplayMode];

// ============================================================
// 修饰键 / 按键名（对应 pkg/keys/keys.go）
// ============================================================

export const Modifier = {
  Ctrl: "ctrl",
  Shift: "shift",
  Alt: "alt",
  Win: "win",
} as const;
export type ModifierValue = (typeof Modifier)[keyof typeof Modifier];

// 仅收录前端实际使用到的按键 token；如需扩充按 keys.go 同步添加。
export const Key = {
  // 字母（仅前端用到的）
  Z: "z",
  // 标点
  Semicolon: "semicolon",
  Quote: "quote",
  Comma: "comma",
  Period: "period",
  Slash: "slash",
  Backslash: "backslash",
  OpenBracket: "open_bracket",
  CloseBracket: "close_bracket",
  Backtick: "backtick",
  // 控制键
  Tab: "tab",
  // 修饰键作为独立 token
  LShift: "lshift",
  RShift: "rshift",
  LCtrl: "lctrl",
  RCtrl: "rctrl",
  CapsLock: "capslock",
} as const;
export type KeyValue = (typeof Key)[keyof typeof Key];

// ============================================================
// 组合键群（对应 pkg/keys/pair.go）
// ============================================================

export const PairGroup = {
  SemicolonQuote: "semicolon_quote",
  CommaPeriod: "comma_period",
  LRShift: "lrshift",
  LRCtrl: "lrctrl",
  PageUpDown: "pageupdown",
  MinusEqual: "minus_equal",
  Brackets: "brackets",
  ShiftTab: "shift_tab",
  Tab: "tab",
  Arrows: "arrows",
} as const;
export type PairGroupValue = (typeof PairGroup)[keyof typeof PairGroup];

// ============================================================
// UI 子枚举（前端独有，未在 Go 端枚举类型化，但仍在多处使用）
// ============================================================

// 状态提示显示模式
export const StatusDisplayMode = {
  Temp: "temp",
  Always: "always",
} as const;
export type StatusDisplayModeValue =
  (typeof StatusDisplayMode)[keyof typeof StatusDisplayMode];

// 状态提示方案名风格
export const SchemaNameStyle = {
  Short: "short",
  Full: "full",
} as const;
export type SchemaNameStyleValue =
  (typeof SchemaNameStyle)[keyof typeof SchemaNameStyle];

// 状态提示位置模式
export const StatusPositionMode = {
  FollowCaret: "follow_caret",
  Custom: "custom",
} as const;
export type StatusPositionModeValue =
  (typeof StatusPositionMode)[keyof typeof StatusPositionMode];

// 数字小键盘行为
export const NumpadBehavior = {
  Direct: "direct",
  FollowMain: "follow_main",
} as const;
export type NumpadBehaviorValue =
  (typeof NumpadBehavior)[keyof typeof NumpadBehavior];

// Shift+字母临时英文行为
export const ShiftBehavior = {
  TempEnglish: "temp_english",
  DirectCommit: "direct_commit",
} as const;
export type ShiftBehaviorValue =
  (typeof ShiftBehavior)[keyof typeof ShiftBehavior];

// ============================================================
// Wails 前端事件名（对应 wind_input/pkg/rpcapi/types.go 中的 WailsEventXxx）
// ============================================================

export const WailsEvent = {
  Config: "config-event",
  Dict: "dict-event",
  Stats: "stats-event",
  System: "system-event",
} as const;
export type WailsEventValue = (typeof WailsEvent)[keyof typeof WailsEvent];

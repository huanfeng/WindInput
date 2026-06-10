import type { PageSchema } from './types'

// tsfLogConfig 属于独立 prop，不在 formData 内，需手写
// 这里只覆盖 formData.advanced 下的两个字段

export const advancedLogSchema: PageSchema = [
  {
    type: 'select',
    key: 'debug.log_level',
    label: '服务日志级别',
    hint: '重启输入法服务后生效',
    options: [
      { value: 'debug', label: 'Debug（调试）' },
      { value: 'info',  label: 'Info（信息）' },
      { value: 'warn',  label: 'Warn（警告）' },
      { value: 'error', label: 'Error（错误）' },
    ],
  },
]

export const advancedPerfSchema: PageSchema = [
  {
    type: 'toggle',
    key: 'debug.perf_sampling',
    label: '按键链路采样',
    hint: '开启后记录每次按键的引擎耗时等数据，用于性能分析',
  },
]

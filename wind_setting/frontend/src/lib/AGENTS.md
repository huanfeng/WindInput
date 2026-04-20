<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

# lib

## Purpose
前端工具函数库。提供通用的样式类合并、CSS 类名处理等辅助工具，供各组件使用。

## Key Files
| 文件 | 说明 |
|------|------|
| `utils.ts` | 样式类合并函数：`cn()`，使用 `clsx` 和 `tailwind-merge` 组合 CSS 类名，支持条件类名和 Tailwind 冲突解决 |

## For AI Agents
### Working In This Directory
- `cn()` 函数是 shadcn 样式工具的标准实现，接收任意数量的 `ClassValue` 参数
- 使用场景：组件属性、条件样式、Tailwind 类冲突解决
- 与 `@tanstack/vue-table` 等无头组件库配合，快速构建样式化表格、对话框等

### Common Patterns
```typescript
import { cn } from '@/lib/utils'

// 合并多个类名
const buttonClass = cn('px-4 py-2', 'rounded-md', 'bg-blue-500')

// 条件类名（用于组件 props）
const containerClass = cn(
  'flex items-center',
  isActive && 'bg-blue-100',
  size === 'large' && 'text-lg'
)

// Tailwind 冲突解决（twMerge 自动处理）
const headingClass = cn('text-lg', 'text-base')  // 结果：text-base（后者覆盖）
```

## Testing Requirements
- TypeScript 编译无错误：`pnpm run build`
- 验证 `cn()` 函数可正确合并类名

## Dependencies
### Internal
无

### External
- `clsx` — 条件类名生成
- `tailwind-merge` — Tailwind CSS 冲突解决

<!-- MANUAL: -->

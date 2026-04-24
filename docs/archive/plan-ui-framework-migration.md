# wind_setting 前端 UI 框架迁移实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 wind_setting 前端从纯手写 CSS 迁移到 shadcn-vue (Radix Vue + Tailwind CSS) + TanStack Table，支持暗色模式，统一主题变量（颜色 + 尺寸）。

**Architecture:** 渐进式迁移 — 先搭建 Tailwind + shadcn-vue 基础设施和主题系统，再逐步替换基础控件（Switch、Dialog、Select、Toast、Button、Checkbox），最后用 TanStack Table 统一重构 5 个词库表格面板。新旧 CSS 共存，逐步删除旧样式。

**Tech Stack:** Vue 3.5+ / TypeScript 5+ / Vite 6+ / Tailwind CSS 4 / Radix Vue / shadcn-vue / TanStack Table / vue-sonner

---

## File Structure Overview

```
wind_setting/frontend/
├── src/
│   ├── assets/css/
│   │   └── theme.css              ← NEW: CSS 变量主题（亮 + 暗）
│   ├── components/
│   │   └── ui/                    ← NEW: shadcn-vue 组件目录
│   │       ├── button/
│   │       │   ├── Button.vue
│   │       │   └── index.ts
│   │       ├── switch/
│   │       ├── dialog/
│   │       ├── alert-dialog/
│   │       ├── select/
│   │       ├── dropdown-menu/
│   │       ├── checkbox/
│   │       ├── badge/
│   │       ├── table/
│   │       ├── input/
│   │       └── data-table/        ← NEW: TanStack Table 封装
│   │           ├── DataTable.vue
│   │           ├── DataTableToolbar.vue
│   │           ├── DataTablePagination.vue
│   │           └── index.ts
│   ├── components/dict/
│   │   ├── DictDataTable.vue      ← NEW: 词库表格共享封装
│   │   ├── PhrasePanel.vue        ← MODIFY: 使用 DictDataTable
│   │   ├── FreqPanel.vue          ← MODIFY
│   │   ├── UserDictPanel.vue      ← MODIFY
│   │   ├── ShadowPanel.vue        ← MODIFY
│   │   ├── TempDictPanel.vue      ← MODIFY
│   │   └── dict-shared.css        ← DELETE (最终)
│   ├── lib/
│   │   └── utils.ts               ← NEW: cn() 工具函数
│   ├── app.css                    ← NEW: Tailwind 入口 + 全局基础样式
│   ├── global.css                 ← MODIFY → DELETE (逐步迁移)
│   └── main.ts                    ← MODIFY: 导入新 CSS
├── components.json                ← NEW: shadcn-vue 配置
├── tailwind.config.ts             ← NEW: Tailwind 主题
├── postcss.config.js              ← NEW: PostCSS 配置（Tailwind 4 备用）
├── vite.config.ts                 ← MODIFY: 添加路径别名 + Tailwind 插件
├── tsconfig.json                  ← MODIFY: 添加路径别名
└── package.json                   ← MODIFY: 依赖升级
```

---

## Task 1: 升级工具链与安装依赖

**Files:**
- Modify: `wind_setting/frontend/package.json`
- Modify: `wind_setting/frontend/vite.config.ts`
- Modify: `wind_setting/frontend/tsconfig.json`
- Modify: `wind_setting/frontend/tsconfig.node.json`

- [ ] **Step 1: 升级核心依赖**

在 `wind_setting/frontend/` 目录下执行：

```bash
pnpm add vue@latest
pnpm add -D vite@latest @vitejs/plugin-vue@latest typescript@latest vue-tsc@latest
```

- [ ] **Step 2: 安装 Tailwind CSS 4 + shadcn-vue 依赖**

```bash
pnpm add radix-vue class-variance-authority clsx tailwind-merge lucide-vue-next vue-sonner
pnpm add -D tailwindcss@latest @tailwindcss/vite
```

- [ ] **Step 3: 安装 TanStack Table**

```bash
pnpm add @tanstack/vue-table
```

- [ ] **Step 4: 更新 vite.config.ts — 添加路径别名和 Tailwind 插件**

```ts
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import { fileURLToPath, URL } from 'node:url'

export default defineConfig({
  plugins: [vue(), tailwindcss()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
})
```

- [ ] **Step 5: 更新 tsconfig.json — 添加路径别名**

在 `compilerOptions` 中添加：

```json
{
  "compilerOptions": {
    "target": "ESNext",
    "useDefineForClassFields": true,
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "jsx": "preserve",
    "sourceMap": true,
    "resolveJsonModule": true,
    "isolatedModules": true,
    "esModuleInterop": true,
    "lib": ["ESNext", "DOM"],
    "skipLibCheck": true,
    "baseUrl": ".",
    "paths": {
      "@/*": ["./src/*"]
    }
  },
  "include": [
    "src/**/*.ts",
    "src/**/*.d.ts",
    "src/**/*.tsx",
    "src/**/*.vue"
  ],
  "references": [{ "path": "./tsconfig.node.json" }]
}
```

- [ ] **Step 6: 更新 tsconfig.node.json**

```json
{
  "compilerOptions": {
    "composite": true,
    "module": "ESNext",
    "moduleResolution": "bundler",
    "allowSyntheticDefaultImports": true,
    "skipLibCheck": true
  },
  "include": ["vite.config.ts"]
}
```

- [ ] **Step 7: 验证构建**

```bash
cd wind_setting/frontend && pnpm run build
```

Expected: 构建成功，无错误。如果有类型错误，根据提示修复（通常是 vue-tsc 升级后的细微类型差异）。

- [ ] **Step 8: Commit**

```bash
git add wind_setting/frontend/package.json wind_setting/frontend/pnpm-lock.yaml wind_setting/frontend/vite.config.ts wind_setting/frontend/tsconfig.json wind_setting/frontend/tsconfig.node.json
git commit -m "chore(setting): 升级前端工具链 Vite 6 + Vue 3.5 + TypeScript 5 + Tailwind CSS 4"
```

---

## Task 2: 配置 Tailwind CSS + 主题系统

**Files:**
- Create: `wind_setting/frontend/src/assets/css/theme.css`
- Create: `wind_setting/frontend/src/app.css`
- Create: `wind_setting/frontend/src/lib/utils.ts`
- Create: `wind_setting/frontend/components.json`
- Modify: `wind_setting/frontend/src/main.ts`

- [ ] **Step 1: 创建主题 CSS 变量文件**

创建 `src/assets/css/theme.css`，定义亮色 + 暗色主题变量。颜色使用 HSL 格式（shadcn-vue 惯例），尺寸变量统一管理控件大小：

```css
@layer base {
  :root {
    /* ── 颜色（HSL，不带 hsl() 包裹） ── */
    --background: 220 16% 96%;       /* #f0f2f5 — 页面背景 */
    --foreground: 215 28% 17%;       /* #1f2937 — 主文字 */

    --card: 0 0% 100%;               /* #ffffff — 卡片背景 */
    --card-foreground: 215 28% 17%;

    --popover: 0 0% 100%;
    --popover-foreground: 215 28% 17%;

    --primary: 217 91% 53%;          /* #2563eb — 主色 */
    --primary-foreground: 210 40% 98%;

    --secondary: 220 14% 96%;        /* #f3f4f6 — 次要背景 */
    --secondary-foreground: 215 28% 17%;

    --muted: 220 14% 96%;            /* #f3f4f6 */
    --muted-foreground: 220 9% 46%;  /* #6b7280 — 次要文字 */

    --accent: 220 14% 96%;
    --accent-foreground: 215 28% 17%;

    --destructive: 0 84% 50%;        /* #dc2626 — 危险/错误 */
    --destructive-foreground: 210 40% 98%;

    --success: 142 72% 36%;          /* #16a34a — 成功 */
    --warning: 38 92% 50%;           /* #d97706 — 警告 */

    --border: 216 12% 84%;           /* #d1d5db */
    --input: 216 12% 84%;
    --ring: 217 91% 53%;             /* #2563eb — 焦点环 */

    /* ── 圆角 ── */
    --radius: 0.375rem;              /* 6px — 按钮/输入框 */
    --radius-lg: 0.5rem;             /* 8px — 表格/列表 */
    --radius-xl: 0.75rem;            /* 12px — 卡片/弹窗 */

    /* ── 控件尺寸（默认值，组件可覆盖） ── */
    --control-h: 2.25rem;            /* 36px — 按钮/输入框/select 默认高度 */
    --control-h-sm: 1.75rem;         /* 28px — 小号变体 */
    --control-h-lg: 2.5rem;          /* 40px — 大号变体 */

    /* ── 字号 ── */
    --text-base: 0.875rem;           /* 14px */
    --text-sm: 0.8125rem;            /* 13px */
    --text-xs: 0.75rem;              /* 12px */

    /* ── 间距 ── */
    --spacing-card: 1.25rem;         /* 20px — 卡片内边距 */
    --spacing-item: 0.875rem;        /* 14px — setting-item 行间距 */

    /* ── 侧边栏 ── */
    --sidebar-width: 200px;
  }

  .dark {
    --background: 222 47% 6%;        /* 深灰背景 */
    --foreground: 210 40% 93%;       /* 浅色文字 */

    --card: 222 47% 9%;
    --card-foreground: 210 40% 93%;

    --popover: 222 47% 9%;
    --popover-foreground: 210 40% 93%;

    --primary: 217 91% 60%;          /* 亮色模式蓝色稍亮 */
    --primary-foreground: 222 47% 6%;

    --secondary: 217 33% 14%;
    --secondary-foreground: 210 40% 93%;

    --muted: 217 33% 14%;
    --muted-foreground: 215 20% 60%;

    --accent: 217 33% 14%;
    --accent-foreground: 210 40% 93%;

    --destructive: 0 63% 55%;
    --destructive-foreground: 210 40% 98%;

    --success: 142 55% 45%;
    --warning: 38 80% 55%;

    --border: 217 33% 20%;
    --input: 217 33% 20%;
    --ring: 217 91% 60%;
  }
}
```

- [ ] **Step 2: 创建 Tailwind CSS 入口文件**

创建 `src/app.css`：

```css
@import "tailwindcss";
@import "./assets/css/theme.css";

/*
 * Tailwind CSS 4 自定义主题扩展
 * 将 CSS 变量映射到 Tailwind 工具类
 */
@theme inline {
  /* 颜色 */
  --color-background: hsl(var(--background));
  --color-foreground: hsl(var(--foreground));
  --color-card: hsl(var(--card));
  --color-card-foreground: hsl(var(--card-foreground));
  --color-popover: hsl(var(--popover));
  --color-popover-foreground: hsl(var(--popover-foreground));
  --color-primary: hsl(var(--primary));
  --color-primary-foreground: hsl(var(--primary-foreground));
  --color-secondary: hsl(var(--secondary));
  --color-secondary-foreground: hsl(var(--secondary-foreground));
  --color-muted: hsl(var(--muted));
  --color-muted-foreground: hsl(var(--muted-foreground));
  --color-accent: hsl(var(--accent));
  --color-accent-foreground: hsl(var(--accent-foreground));
  --color-destructive: hsl(var(--destructive));
  --color-destructive-foreground: hsl(var(--destructive-foreground));
  --color-success: hsl(var(--success));
  --color-warning: hsl(var(--warning));
  --color-border: hsl(var(--border));
  --color-input: hsl(var(--input));
  --color-ring: hsl(var(--ring));

  /* 圆角 */
  --radius-sm: calc(var(--radius) - 2px);
  --radius-md: var(--radius);
  --radius-lg: var(--radius-lg);
  --radius-xl: var(--radius-xl);

  /* 字号 */
  --font-size-xs: var(--text-xs);
  --font-size-sm: var(--text-sm);
  --font-size-base: var(--text-base);

  /* 动画 */
  --animate-accordion-down: accordion-down 0.2s ease-out;
  --animate-accordion-up: accordion-up 0.2s ease-out;

  @keyframes accordion-down {
    from { height: 0; }
    to { height: var(--radix-accordion-content-height); }
  }
  @keyframes accordion-up {
    from { height: var(--radix-accordion-content-height); }
    to { height: 0; }
  }
}

/*
 * 全局基础样式 — 沿用现有设计语言
 */
@layer base {
  * {
    @apply border-border;
  }

  body {
    @apply bg-background text-foreground;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "Microsoft YaHei UI", sans-serif;
    font-size: var(--text-base);
    line-height: 1.5;
  }
}
```

- [ ] **Step 3: 创建 cn() 工具函数**

创建 `src/lib/utils.ts`：

```ts
import { type ClassValue, clsx } from 'clsx'
import { twMerge } from 'tailwind-merge'

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
```

- [ ] **Step 4: 创建 shadcn-vue 配置文件**

创建 `components.json`：

```json
{
  "style": "default",
  "typescript": true,
  "tailwind": {
    "config": "",
    "css": "src/app.css",
    "baseColor": "zinc"
  },
  "framework": "vite",
  "aliases": {
    "components": "@/components",
    "utils": "@/lib/utils",
    "ui": "@/components/ui"
  }
}
```

- [ ] **Step 5: 更新 main.ts — 切换 CSS 入口**

```ts
import { createApp } from 'vue'
import App from './App.vue'
import './app.css'
import './global.css'  // 暂时保留旧样式，渐进迁移后删除

createApp(App).mount('#app')
```

注意：`app.css`（Tailwind）在 `global.css` 之前导入，这样旧样式可以在过渡期覆盖 Tailwind 的 base reset，保持现有外观不变。

- [ ] **Step 6: 验证构建**

```bash
cd wind_setting/frontend && pnpm run build
```

Expected: 构建成功。Tailwind CSS 应该正常工作，但因为还没有使用 Tailwind 类名，所以界面外观应与之前完全一致。

- [ ] **Step 7: Commit**

```bash
git add wind_setting/frontend/src/assets/css/theme.css wind_setting/frontend/src/app.css wind_setting/frontend/src/lib/utils.ts wind_setting/frontend/components.json wind_setting/frontend/src/main.ts
git commit -m "feat(setting): 配置 Tailwind CSS 4 + shadcn-vue 主题系统，支持亮色/暗色模式"
```

---

## Task 3: 添加 shadcn-vue 基础组件

**Files:**
- Create: `src/components/ui/button/Button.vue` + `index.ts`
- Create: `src/components/ui/switch/Switch.vue` + `index.ts`
- Create: `src/components/ui/dialog/` (Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter, DialogClose)
- Create: `src/components/ui/alert-dialog/` (AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, AlertDialogTrigger)
- Create: `src/components/ui/select/` (Select, SelectContent, SelectItem, SelectTrigger, SelectValue)
- Create: `src/components/ui/dropdown-menu/` (DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger)
- Create: `src/components/ui/checkbox/Checkbox.vue` + `index.ts`
- Create: `src/components/ui/badge/Badge.vue` + `index.ts`
- Create: `src/components/ui/input/Input.vue` + `index.ts`
- Create: `src/components/ui/table/` (Table, TableBody, TableCell, TableHead, TableHeader, TableRow)

- [ ] **Step 1: 使用 shadcn-vue CLI 批量添加组件**

```bash
cd wind_setting/frontend
npx shadcn-vue@latest add button switch dialog alert-dialog select dropdown-menu checkbox badge input table
```

CLI 会将组件源码生成到 `src/components/ui/` 下。如果 CLI 因为 Tailwind 4 兼容问题报错，手动从 shadcn-vue 仓库复制组件并调整。

- [ ] **Step 2: 检查生成的组件，确保使用了项目的主题变量**

打开任意一个生成的组件（如 `Button.vue`），确认：
- 使用 `cn()` 做类名合并
- 使用了 `bg-primary`、`text-primary-foreground` 等主题色类名（而非硬编码颜色）
- 尺寸 variant 使用 `h-9`（对应 --control-h 36px）、`h-7`（对应 --control-h-sm 28px）

如果需要调整默认尺寸以匹配现有设计，修改 Button.vue 的 `variants`：

```ts
const buttonVariants = cva(
  'inline-flex items-center justify-center whitespace-nowrap rounded-md text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50',
  {
    variants: {
      variant: {
        default: 'bg-primary text-primary-foreground shadow hover:bg-primary/90',
        destructive: 'bg-destructive text-destructive-foreground shadow-sm hover:bg-destructive/90',
        outline: 'border border-input bg-background shadow-sm hover:bg-accent hover:text-accent-foreground',
        secondary: 'bg-secondary text-secondary-foreground shadow-sm hover:bg-secondary/80',
        ghost: 'hover:bg-accent hover:text-accent-foreground',
        link: 'text-primary underline-offset-4 hover:underline',
      },
      size: {
        default: 'h-[var(--control-h)] px-4 py-2 text-[length:var(--text-base)]',
        sm: 'h-[var(--control-h-sm)] rounded-md px-2.5 text-[length:var(--text-xs)]',
        lg: 'h-[var(--control-h-lg)] rounded-md px-8',
        icon: 'h-[var(--control-h-sm)] w-[var(--control-h-sm)]',
      },
    },
    defaultVariants: {
      variant: 'default',
      size: 'default',
    },
  },
)
```

- [ ] **Step 3: 验证构建**

```bash
cd wind_setting/frontend && pnpm run build
```

Expected: 构建成功。组件已生成但尚未使用，界面无变化。

- [ ] **Step 4: Commit**

```bash
git add wind_setting/frontend/src/components/ui/
git commit -m "feat(setting): 添加 shadcn-vue 基础 UI 组件库"
```

---

## Task 4: 替换全局基础控件

本任务分子步骤逐个替换 App.vue 及各页面中的基础控件。每替换一类控件，验证后再继续。

**Files:**
- Modify: `src/App.vue`
- Modify: `src/composables/useToast.ts`
- Modify: `src/composables/useConfirm.ts`
- Delete: `src/components/ToastContainer.vue`
- Modify: `src/pages/GeneralPage.vue`
- Modify: `src/pages/InputPage.vue`
- Modify: `src/pages/HotkeyPage.vue`
- Modify: `src/pages/AppearancePage.vue`
- Modify: `src/pages/DictionaryPage.vue`
- Modify: `src/pages/AdvancedPage.vue`

### 4A: 替换 Toast → vue-sonner

- [ ] **Step 1: 在 App.vue 中引入 Sonner**

替换 `ToastContainer` 引用：

```vue
<script setup>
// 移除旧的 toast 导入
// import ToastContainer from './components/ToastContainer.vue'
// const { toasts, toast } = provideToast()

import { Toaster, toast } from 'vue-sonner'
import { provide } from 'vue'

// 提供 toast 给子组件
provide('toast', toast)
</script>

<template>
  <!-- 替换 <ToastContainer :toasts="toasts" /> 为 -->
  <Toaster position="top-center" :duration="3000" rich-colors />
  <!-- ...其余模板不变... -->
</template>
```

- [ ] **Step 2: 更新 useToast.ts 为 vue-sonner 的封装**

```ts
import { inject } from 'vue'
import { toast as sonnerToast } from 'vue-sonner'

type ToastFn = typeof sonnerToast

const TOAST_KEY = Symbol('toast') as InjectionKey<ToastFn>

export function provideToast() {
  provide(TOAST_KEY, sonnerToast)
  return { toast: sonnerToast }
}

export function useToast() {
  const t = inject(TOAST_KEY, sonnerToast)
  return {
    toast(message: string, type: 'success' | 'error' = 'success', duration = 3000) {
      if (type === 'error') {
        t.error(message, { duration })
      } else {
        t.success(message, { duration })
      }
    },
  }
}
```

这样保持了原有的 `toast(msg, type, duration)` 调用签名不变，所有现有调用点无需修改。

- [ ] **Step 3: 删除 ToastContainer.vue**

```bash
rm wind_setting/frontend/src/components/ToastContainer.vue
```

- [ ] **Step 4: 验证构建并测试 toast 功能**

```bash
cd wind_setting/frontend && pnpm run build
```

### 4B: 替换 Confirm Dialog → AlertDialog

- [ ] **Step 5: 更新 useConfirm.ts 使用 shadcn AlertDialog**

useConfirm 继续保持 Promise-based API，但渲染改为 shadcn AlertDialog：

```ts
import { ref } from 'vue'

const visible = ref(false)
const message = ref('')
const resolvePromise = ref<((value: boolean) => void) | null>(null)

export function useConfirm() {
  function confirm(msg: string): Promise<boolean> {
    message.value = msg
    visible.value = true
    return new Promise<boolean>((resolve) => {
      resolvePromise.value = resolve
    })
  }

  function handleConfirm() {
    visible.value = false
    resolvePromise.value?.(true)
    resolvePromise.value = null
  }

  function handleCancel() {
    visible.value = false
    resolvePromise.value?.(false)
    resolvePromise.value = null
  }

  return { confirmVisible: visible, confirmMessage: message, confirm, handleConfirm, handleCancel }
}
```

API 不变。在 App.vue 中将旧的 confirm 弹窗模板替换为 shadcn AlertDialog：

```vue
<AlertDialog :open="confirmVisible">
  <AlertDialogContent>
    <AlertDialogHeader>
      <AlertDialogTitle>确认操作</AlertDialogTitle>
      <AlertDialogDescription>{{ confirmMessage }}</AlertDialogDescription>
    </AlertDialogHeader>
    <AlertDialogFooter>
      <AlertDialogCancel @click="handleCancel">取消</AlertDialogCancel>
      <AlertDialogAction @click="handleConfirm">确认</AlertDialogAction>
    </AlertDialogFooter>
  </AlertDialogContent>
</AlertDialog>
```

替换 App.vue 中原有的 `<div v-if="confirmVisible" class="dialog-overlay">...</div>` 代码块。

### 4C: 替换 Switch/Toggle

- [ ] **Step 6: 在所有页面中替换 Switch**

搜索所有使用 `<label class="switch">` 的地方，替换为 shadcn Switch：

**之前：**
```vue
<label class="switch">
  <input type="checkbox" v-model="formData.input.punct_follow_mode" />
  <span class="slider"></span>
</label>
```

**之后：**
```vue
<Switch v-model:checked="formData.input.punct_follow_mode" />
```

涉及文件：`App.vue`、`GeneralPage.vue`、`InputPage.vue`、`HotkeyPage.vue`、`AppearancePage.vue`、`AdvancedPage.vue`。

dict 面板中的小号 `.toggle-switch` 暂时保留，在 Task 5 表格重构时一并替换。

### 4D: 替换 Select/Dropdown

- [ ] **Step 7: 替换自定义下拉菜单为 shadcn Select**

1) InputPage.vue 的 `filter-dropdown`（候选检索范围选择器）替换为 shadcn Select：

**之前：** `filterDropdownRef` + `filterDropdownOpen` + click-outside 逻辑 + `.filter-menu` + `.filter-option`

**之后：**
```vue
<Select v-model="formData.input.filter_mode">
  <SelectTrigger class="w-[200px]">
    <SelectValue />
  </SelectTrigger>
  <SelectContent>
    <SelectItem v-for="opt in filterModeOptions" :key="opt.value" :value="opt.value">
      <div class="flex flex-col">
        <span>{{ opt.label }}</span>
        <span class="text-xs text-muted-foreground">{{ opt.desc }}</span>
      </div>
    </SelectItem>
  </SelectContent>
</Select>
```

可以删除 `filterDropdownOpen`、`filterDropdownRef`、document click-outside 的 `onMounted/onUnmounted` 监听。

2) DictionaryPage.vue 的操作菜单（`toolbar-more` + `toolbar-dropdown`）替换为 DropdownMenu：

```vue
<DropdownMenu>
  <DropdownMenuTrigger as-child>
    <Button variant="destructive" size="sm">操作 ▾</Button>
  </DropdownMenuTrigger>
  <DropdownMenuContent align="end">
    <DropdownMenuItem class="text-destructive" @click="handleResetCurrentSchema">
      重置当前方案
    </DropdownMenuItem>
    <DropdownMenuItem class="text-destructive" @click="handleResetAllSchemas">
      重置所有方案
    </DropdownMenuItem>
    <DropdownMenuItem
      v-if="selectedSchemaOrphaned"
      class="text-destructive"
      @click="handleDeleteOrphanedSchema"
    >
      删除当前方案
    </DropdownMenuItem>
  </DropdownMenuContent>
</DropdownMenu>
```

可以删除 `showSchemaMenu` 状态和 `toggleSchemaMenu` 方法。

3) AppearancePage.vue 的主题选择下拉菜单 — 保持自定义实现，因为布局复杂度高（多行显示、badge 等），用 shadcn Popover 包裹即可获得 click-outside 和焦点管理能力。

### 4E: 替换 Button

- [ ] **Step 8: 全局替换按钮**

使用 shadcn Button 替换所有 `<button class="btn ...">` 元素：

| 旧写法 | 新写法 |
|--------|--------|
| `<button class="btn">` | `<Button variant="outline">` |
| `<button class="btn btn-primary">` | `<Button>` |
| `<button class="btn btn-sm">` | `<Button variant="outline" size="sm">` |
| `<button class="btn btn-primary btn-sm">` | `<Button size="sm">` |
| `<button class="btn btn-sm btn-danger-outline">` | `<Button variant="destructive" size="sm">` |

涉及所有页面文件。批量替换后运行构建确认无报错。

### 4F: 替换 Checkbox

- [ ] **Step 9: 替换 Checkbox**

将 `.checkbox-item` / `.checkbox-group` 中的原生 checkbox 替换为 shadcn Checkbox：

**之前：**
```vue
<label class="checkbox-item">
  <input type="checkbox" v-model="someValue" />
  <span>标签文字</span>
</label>
```

**之后：**
```vue
<div class="flex items-center gap-2">
  <Checkbox v-model:checked="someValue" id="some-id" />
  <label for="some-id" class="text-sm">标签文字</label>
</div>
```

涉及文件：`HotkeyPage.vue`（模式切换键组、候选操作键组）、`InputPage.vue`（括号自动配对）、`GeneralPage.vue`（模糊音配置）。

- [ ] **Step 10: 验证构建 + 视觉检查**

```bash
cd wind_setting/frontend && pnpm run build
```

用 `wails dev` 或 `pnpm dev` 启动，逐页检查各控件渲染是否正确，特别关注：
- Switch 开关状态正确反映 v-model
- Dialog/AlertDialog 弹出和关闭正常
- Select 下拉项选择正常
- Toast 消息正常弹出
- Button 尺寸和颜色与原有设计一致

- [ ] **Step 11: Commit**

```bash
git add -A
git commit -m "refactor(setting): 替换基础控件为 shadcn-vue（Switch、Dialog、Toast、Select、Button、Checkbox）"
```

---

## Task 5: 构建共享词库表格组件 DictDataTable

**Files:**
- Create: `src/components/dict/DictDataTable.vue`
- Create: `src/components/dict/DictTableToolbar.vue`
- Create: `src/components/dict/DictTablePagination.vue`

- [ ] **Step 1: 创建 DictDataTable.vue — 基于 TanStack Table 的共享表格**

分析 5 个面板的共同模式，封装为一个统一的表格组件：

```vue
<script setup lang="ts" generic="TData extends Record<string, any>">
import { ref, computed, watch } from 'vue'
import {
  useVueTable,
  getCoreRowModel,
  getFilteredRowModel,
  getPaginationRowModel,
  getSortedRowModel,
  FlexRender,
  type ColumnDef,
  type SortingState,
  type RowSelectionState,
} from '@tanstack/vue-table'
import { Switch } from '@/components/ui/switch'
import { Checkbox } from '@/components/ui/checkbox'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from '@/components/ui/table'

interface Props {
  columns: ColumnDef<TData, any>[]
  data: TData[]
  loading?: boolean
  searchable?: boolean
  searchPlaceholder?: string
  selectable?: boolean
  /** 服务端分页模式 */
  serverPagination?: {
    total: number
    pageSize: number
    page: number
  }
  /** 客户端分页 (0 = 禁用) */
  pageSize?: number
  /** 行唯一键函数 */
  rowKey: (row: TData) => string
  emptyText?: string
  searchEmptyText?: string
}

const props = withDefaults(defineProps<Props>(), {
  loading: false,
  searchable: true,
  searchPlaceholder: '搜索...',
  selectable: true,
  pageSize: 0,
  emptyText: '暂无数据',
  searchEmptyText: '未找到匹配数据',
})

const emit = defineEmits<{
  'update:selection': [keys: Set<string>]
  'page-change': [page: number]
}>()

const globalFilter = ref('')
const sorting = ref<SortingState>([])
const rowSelection = ref<RowSelectionState>({})

const table = useVueTable({
  get data() { return props.data },
  get columns() { return props.columns },
  state: {
    get globalFilter() { return globalFilter.value },
    get sorting() { return sorting.value },
    get rowSelection() { return rowSelection.value },
  },
  onSortingChange: (updater) => {
    sorting.value = typeof updater === 'function' ? updater(sorting.value) : updater
  },
  onRowSelectionChange: (updater) => {
    rowSelection.value = typeof updater === 'function' ? updater(rowSelection.value) : updater
  },
  onGlobalFilterChange: (updater) => {
    globalFilter.value = typeof updater === 'function' ? updater(globalFilter.value) : updater
  },
  getCoreRowModel: getCoreRowModel(),
  getFilteredRowModel: props.serverPagination ? undefined : getFilteredRowModel(),
  getSortedRowModel: getSortedRowModel(),
  getPaginationRowModel: props.pageSize > 0 ? getPaginationRowModel() : undefined,
  getRowId: (row) => props.rowKey(row),
  enableRowSelection: props.selectable,
})

// 选中状态同步给父组件
watch(rowSelection, () => {
  const keys = new Set(Object.keys(rowSelection.value).filter(k => rowSelection.value[k]))
  emit('update:selection', keys)
}, { deep: true })

// 暴露给父组件
const selectedCount = computed(() =>
  Object.values(rowSelection.value).filter(Boolean).length
)

function clearSelection() {
  rowSelection.value = {}
}

defineExpose({ table, globalFilter, clearSelection, selectedCount })
</script>

<template>
  <div class="flex flex-col flex-1 min-h-0 overflow-hidden">
    <!-- 工具栏 -->
    <div class="flex items-center gap-2 mb-2 shrink-0 flex-nowrap">
      <slot name="toolbar-start" :selected-count="selectedCount" :clear-selection="clearSelection" />

      <div class="flex-1 min-w-1" />

      <Input
        v-if="searchable"
        v-model="globalFilter"
        type="text"
        :placeholder="searchPlaceholder"
        class="w-[100px] min-w-[60px] shrink h-[var(--control-h-sm)]"
      />

      <span class="text-xs text-muted-foreground shrink-0 whitespace-nowrap">
        共 {{ serverPagination?.total ?? data.length }} 条
      </span>

      <slot name="toolbar-end" />
    </div>

    <!-- 表格 -->
    <div class="relative flex flex-col flex-1 min-h-0 overflow-hidden border rounded-lg border-border">
      <!-- Loading overlay -->
      <div v-if="loading" class="absolute inset-0 z-10 flex items-center justify-center bg-card/70 rounded-lg">
        <div class="h-8 w-8 border-3 border-border border-t-primary rounded-full animate-spin" />
      </div>

      <div class="overflow-y-auto flex-1 min-h-0">
        <Table>
          <TableHeader class="sticky top-0 z-[1] bg-secondary">
            <TableRow v-for="headerGroup in table.getHeaderGroups()" :key="headerGroup.id">
              <TableHead v-for="header in headerGroup.headers" :key="header.id"
                :class="[
                  header.column.getCanSort() ? 'cursor-pointer select-none hover:text-foreground' : '',
                ]"
                :style="{ width: header.getSize() !== 150 ? `${header.getSize()}px` : undefined }"
                @click="header.column.getToggleSortingHandler()?.($event)"
              >
                <FlexRender
                  v-if="!header.isPlaceholder"
                  :render="header.column.columnDef.header"
                  :props="header.getContext()"
                />
                <span v-if="header.column.getIsSorted() === 'asc'" class="ml-1 text-xs">▲</span>
                <span v-else-if="header.column.getIsSorted() === 'desc'" class="ml-1 text-xs">▼</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <template v-if="table.getRowModel().rows.length > 0">
              <TableRow
                v-for="row in table.getRowModel().rows"
                :key="row.id"
                :class="{ 'bg-primary/5': row.getIsSelected() }"
              >
                <TableCell v-for="cell in row.getVisibleCells()" :key="cell.id">
                  <FlexRender :render="cell.column.columnDef.cell" :props="cell.getContext()" />
                </TableCell>
              </TableRow>
            </template>
            <template v-else>
              <TableRow>
                <TableCell :colspan="columns.length" class="h-24 text-center text-muted-foreground">
                  {{ globalFilter ? searchEmptyText : emptyText }}
                </TableCell>
              </TableRow>
            </template>
          </TableBody>
        </Table>
      </div>
    </div>

    <!-- 分页 (服务端分页模式) -->
    <div
      v-if="serverPagination && serverPagination.total > serverPagination.pageSize"
      class="flex items-center justify-center gap-3 pt-2 shrink-0"
    >
      <Button variant="outline" size="sm"
        :disabled="serverPagination.page === 0"
        @click="emit('page-change', serverPagination.page - 1)"
      >上一页</Button>
      <span class="text-xs text-muted-foreground">
        {{ serverPagination.page * serverPagination.pageSize + 1 }}-{{
          Math.min((serverPagination.page + 1) * serverPagination.pageSize, serverPagination.total)
        }} / {{ serverPagination.total }}
      </span>
      <Button variant="outline" size="sm"
        :disabled="(serverPagination.page + 1) * serverPagination.pageSize >= serverPagination.total"
        @click="emit('page-change', serverPagination.page + 1)"
      >下一页</Button>
    </div>

    <!-- 客户端分页 -->
    <div
      v-else-if="pageSize > 0 && data.length > pageSize"
      class="flex items-center justify-center gap-3 pt-2 shrink-0"
    >
      <Button variant="outline" size="sm"
        :disabled="!table.getCanPreviousPage()"
        @click="table.previousPage()"
      >上一页</Button>
      <span class="text-xs text-muted-foreground">
        第 {{ table.getState().pagination.pageIndex + 1 }} / {{ table.getPageCount() }} 页
      </span>
      <Button variant="outline" size="sm"
        :disabled="!table.getCanNextPage()"
        @click="table.nextPage()"
      >下一页</Button>
    </div>
  </div>
</template>
```

- [ ] **Step 2: 验证构建**

```bash
cd wind_setting/frontend && pnpm run build
```

- [ ] **Step 3: Commit**

```bash
git add wind_setting/frontend/src/components/dict/DictDataTable.vue
git commit -m "feat(setting): 创建 DictDataTable 共享词库表格组件（TanStack Table）"
```

---

## Task 6: 迁移 5 个词库面板到 DictDataTable

每个面板的迁移模式相同：定义 columns → 替换 template 为 DictDataTable → 删除手写表格/分页/选择逻辑。

### 6A: 迁移 FreqPanel（最简单，无 add/edit dialog）

**Files:**
- Modify: `src/components/dict/FreqPanel.vue`

- [ ] **Step 1: 重写 FreqPanel.vue**

移除手写 `<table>`、手动 checkbox 逻辑、手动分页。改用 DictDataTable：

```vue
<script setup lang="ts">
import { ref, watch } from 'vue'
import { h } from 'vue'
import type { ColumnDef } from '@tanstack/vue-table'
import DictDataTable from './DictDataTable.vue'
import { Checkbox } from '@/components/ui/checkbox'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
// ... Wails API imports 保持不变 ...

const props = defineProps<{ schemaId: string; schemaName: string }>()
const emit = defineEmits<{ loading: [boolean] }>()

const { confirm } = useConfirm()
const { toast } = useToast()

const freqList = ref<any[]>([])
const loading = ref(false)
const selectedKeys = ref<Set<string>>(new Set())
const page = ref(0)
const total = ref(0)
const pageSize = 100

const tableRef = ref<InstanceType<typeof DictDataTable>>()

const columns: ColumnDef<any, any>[] = [
  {
    id: 'select',
    size: 32,
    header: ({ table }) => h(Checkbox, {
      checked: table.getIsAllPageRowsSelected(),
      'onUpdate:checked': (val: boolean) => table.toggleAllPageRowsSelected(val),
    }),
    cell: ({ row }) => h(Checkbox, {
      checked: row.getIsSelected(),
      'onUpdate:checked': (val: boolean) => row.toggleSelected(val),
    }),
    enableSorting: false,
  },
  {
    accessorKey: 'code',
    header: '编码',
    size: 100,
    cell: ({ row }) => h('span', {
      class: 'font-mono text-sm text-muted-foreground bg-secondary px-2 py-0.5 rounded',
    }, row.getValue('code')),
  },
  { accessorKey: 'text', header: '词条' },
  { accessorKey: 'count', header: '次数', size: 60, cell: ({ row }) => h('span', { class: 'text-xs text-muted-foreground' }, row.getValue('count')) },
  { accessorKey: 'boost', header: '提升', size: 70, cell: ({ row }) => h('span', { class: 'text-xs text-muted-foreground' }, row.getValue('boost')) },
  {
    accessorKey: 'last_used',
    header: '最后使用',
    size: 120,
    cell: ({ row }) => h('span', { class: 'text-xs text-muted-foreground' }, formatLastUsed(row.getValue('last_used'))),
  },
  {
    id: 'actions',
    size: 50,
    cell: ({ row }) => h(Button, {
      variant: 'ghost', size: 'icon',
      class: 'h-6 w-6 text-muted-foreground hover:text-destructive',
      onClick: () => handleDelete(row.original),
    }, () => '×'),
  },
]

// loadData, handleDelete, handleBatchDelete, handleClear, formatLastUsed
// ... 这些业务方法保持不变 ...

watch(() => props.schemaId, () => { page.value = 0; loadData() }, { immediate: true })
</script>

<template>
  <DictDataTable
    ref="tableRef"
    :columns="columns"
    :data="freqList"
    :loading="loading"
    :row-key="(row) => `${row.code}||${row.text}`"
    :server-pagination="{ total, pageSize, page }"
    empty-text="暂无词频记录"
    search-empty-text="未找到匹配词频记录"
    @update:selection="selectedKeys = $event"
    @page-change="page = $event; loadData()"
  >
    <template #toolbar-start="{ selectedCount }">
      <Button variant="destructive" size="sm"
        :disabled="selectedCount === 0"
        @click="handleBatchDelete"
      >
        删除{{ selectedCount > 0 ? ` (${selectedCount})` : '' }}
      </Button>
    </template>
    <template #toolbar-end>
      <Button variant="destructive" size="sm" @click="handleClear">清空</Button>
    </template>
  </DictDataTable>
</template>
```

- [ ] **Step 2: 验证构建 + 测试词频面板**

```bash
cd wind_setting/frontend && pnpm run build
```

### 6B: 迁移 TempDictPanel

**Files:** `src/components/dict/TempDictPanel.vue`

- [ ] **Step 3: 重写 TempDictPanel.vue**

与 FreqPanel 类似，无 add/edit dialog。额外的"转正"操作作为 actions 列的第二个按钮：

columns 定义：select, code, text, weight, count, actions（含 promote + delete 两个按钮）。

toolbar-start: 批量删除按钮。
toolbar-end: "全部转正" + "清空" 按钮。

### 6C: 迁移 ShadowPanel

**Files:** `src/components/dict/ShadowPanel.vue`

- [ ] **Step 4: 重写 ShadowPanel.vue**

columns: select, code, word, type(badge: pin/delete), position(条件显示), actions。
保留 add/edit dialog，改用 shadcn Dialog 组件渲染。
toolbar-start: "+ 添加" 按钮 + 批量删除按钮。

### 6D: 迁移 UserDictPanel

**Files:** `src/components/dict/UserDictPanel.vue`

- [ ] **Step 5: 重写 UserDictPanel.vue**

columns: select, code(sortable), text(sortable), weight(sortable), actions。
这是唯一有排序功能的面板 — TanStack Table 原生支持，只需在列定义中启用 `enableSorting: true`。
保留 AddWordPage 对话框引用。

### 6E: 迁移 PhrasePanel

**Files:** `src/components/dict/PhrasePanel.vue`

- [ ] **Step 6: 重写 PhrasePanel.vue**

最复杂的面板。columns: select, enabled(Switch), code, content, type(badge), position, actions。
保留 add/edit dialog，改用 shadcn Dialog。
toolbar-start: "全选" checkbox + "+ 添加" + 批量删除。
toolbar-end: "恢复默认" 按钮。

- [ ] **Step 7: 删除 dict-shared.css**

所有面板迁移完成后，`dict-shared.css` 中的样式已不再使用：

```bash
rm wind_setting/frontend/src/components/dict/dict-shared.css
```

全局搜索确认没有其他文件引用它。

- [ ] **Step 8: 验证构建 + 全面测试词库管理**

```bash
cd wind_setting/frontend && pnpm run build
```

用 `wails dev` 启动，逐一测试每个词库面板：
- 加载数据、搜索过滤
- 全选/单选 checkbox
- 批量删除、单条删除
- 添加/编辑弹窗（PhrasePanel、ShadowPanel、UserDictPanel）
- 分页（FreqPanel）
- 排序（UserDictPanel）
- 转正操作（TempDictPanel）

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor(setting): 迁移词库表格到 TanStack Table + DictDataTable 共享组件"
```

---

## Task 7: 清理旧 CSS + 迁移剩余页面样式

**Files:**
- Modify: `src/global.css` → 大幅精简或删除
- Modify: `src/app.css` → 补充必要的全局样式
- Modify: 各页面的 `<style scoped>` 块

- [ ] **Step 1: 精简 global.css**

将 `global.css` 中已被 shadcn-vue 组件/Tailwind 类替代的样式块删除：

**可以删除的：**
- Reset & Base（由 Tailwind preflight 接管）
- `.btn` 系列（由 shadcn Button 接管）
- `.switch` / `.slider`（由 shadcn Switch 接管）
- `.select` 系列（由 shadcn Select 接管）
- `.input` 系列（由 shadcn Input 接管）
- `.dialog-overlay` / `.dialog-box` 系列（由 shadcn Dialog 接管）
- `.checkbox-group` / `.checkbox-item`（由 shadcn Checkbox 接管）
- `.spinner` / `.loading`（由 Tailwind animate-spin 接管）

**应该保留并迁移到 app.css 的 `@layer components` 中：**
- Layout 样式（`.app`, `.sidebar`, `.nav`, `.main`, `.content`）
- Section/Card 样式（`.section`, `.settings-card`, `.card-title`）
- Setting item 样式（`.setting-item`, `.setting-info`, `.setting-hint`, `.setting-control`）
- Warning card 样式
- Segmented control 样式（如果尚未替换）
- 主题下拉菜单（AppearancePage 特有）

- [ ] **Step 2: 将保留样式迁移到 Tailwind + CSS 变量风格**

示例 — 将 `.settings-card` 迁移：

**之前（global.css）：**
```css
.settings-card {
  background: #fff;
  border-radius: 12px;
  padding: 20px;
  margin-bottom: 16px;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.05);
}
```

**之后（app.css @layer components）：**
```css
@layer components {
  .settings-card {
    @apply bg-card rounded-xl p-[var(--spacing-card)] mb-4 shadow-sm;
  }
}
```

这样 `.settings-card` 在暗色模式下自动使用 `--card` 颜色。

- [ ] **Step 3: 迁移所有保留样式为主题感知**

逐一将 `global.css` 中保留的样式改为使用 Tailwind 主题类或 CSS 变量：
- 硬编码颜色 `#1f2937` → `text-foreground` 或 `color: hsl(var(--foreground))`
- 硬编码背景 `#fff` → `bg-card`
- 硬编码边框 `#e5e7eb` → `border-border`
- 硬编码 hover `#f3f4f6` → `bg-secondary`

- [ ] **Step 4: 删除 global.css 中已废弃的样式**

经过 Step 1-3，global.css 应该可以完全删除或仅剩少量过渡样式。将剩余内容合并到 `app.css`，然后删除 `global.css` 并移除 `main.ts` 中的导入。

更新 `main.ts`：
```ts
import { createApp } from 'vue'
import App from './App.vue'
import './app.css'

createApp(App).mount('#app')
```

- [ ] **Step 5: 清理各页面 scoped style 中的硬编码颜色**

搜索各 `.vue` 文件中 `<style scoped>` 块里的硬编码颜色值，替换为 CSS 变量引用。确保暗色模式下所有元素颜色正确切换。

- [ ] **Step 6: 验证构建 + 暗色模式测试**

```bash
cd wind_setting/frontend && pnpm run build
```

在浏览器中测试：
1. 正常亮色模式 — 外观应与迁移前基本一致
2. 系统切换到暗色模式（或在 DevTools 中模拟 `prefers-color-scheme: dark`）— 所有页面应正确显示暗色主题
3. 重点检查：卡片背景、文字颜色、边框、按钮、表格、弹窗

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(setting): 清理旧 CSS，全面迁移到 Tailwind 主题变量，支持暗色模式"
```

---

## Task 8: 最终验证与收尾

**Files:** 无新增文件

- [ ] **Step 1: 全面功能回归测试**

用 `wails dev` 启动应用，逐页测试：

| 页面 | 测试点 |
|------|--------|
| 常规设置 | 方案列表排序、方案切换、引擎设置展开折叠、模糊音配置弹窗 |
| 输入习惯 | 所有 Switch 开关、候选范围 Select、标点映射配置弹窗 |
| 快捷键 | 所有 Checkbox 组、冲突检测、HotkeyComposer 组件 |
| 外观设置 | 主题选择下拉、字体选择、预览 |
| 词库管理 | 5 个面板的完整 CRUD 流程 |
| 高级设置 | 配置文件操作、日志级别 |
| 关于 | 版本信息显示 |

- [ ] **Step 2: 暗色模式视觉检查**

系统切到暗色模式，检查所有页面无白色硬编码区域、文字清晰可读、边框和分割线可见。

- [ ] **Step 3: 构建产物体积检查**

```bash
cd wind_setting/frontend && pnpm run build
```

对比迁移前后的 `dist/` 体积，确保 tree-shaking 正常（未引入的 shadcn 组件不应出现在产物中）。

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore(setting): UI 框架迁移完成，验证通过"
```

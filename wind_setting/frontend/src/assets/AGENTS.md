<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-08 -->

# assets

## Purpose
前端静态资源目录，存放字体文件和图片。资源由 Vite 处理后嵌入构建产物，最终随 Go 二进制一并打包。

## Key Files
| 文件 | 说明 |
|------|------|
| `fonts/nunito-v16-latin-regular.woff2` | Nunito 字体（woff2 格式），用于 UI 字体显示 |
| `fonts/OFL.txt` | Nunito 字体开源许可证（SIL Open Font License） |
| `images/logo-universal.png` | 清风输入法应用图标，在"关于"页面和侧边栏显示 |

## For AI Agents
### Working In This Directory
- 添加新资源后在 CSS 或 Vue 组件中通过相对路径或 `import.meta.url` 引用
- 字体通过 `global.css` 中的 `@font-face` 声明加载
- 图片在 `App.vue` 中通过 `new URL('./assets/images/logo-universal.png', import.meta.url).href` 引用（Vite 资源处理）
- 不要在此目录放置需要动态生成的内容，仅存放静态文件

### Testing Requirements
- `pnpm run build` 确认资源正确打包（dist 目录下包含 assets 文件）

### Common Patterns
- 字体引用：`@font-face { src: url('./assets/fonts/nunito-v16-latin-regular.woff2') format('woff2'); }`
- 图片引用：`new URL('./assets/images/logo-universal.png', import.meta.url).href`

## Dependencies
### Internal
无

### External
- Vite 资源处理管道

<!-- MANUAL: -->

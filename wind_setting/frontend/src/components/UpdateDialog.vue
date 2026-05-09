<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { marked } from 'marked'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import {
  getUpdateConfig,
  saveUpdateConfig,
  checkUpdate,
  startDownload,
  cancelDownload,
  installRelease,
  onDownloadProgress,
  onDownloadDone,
  onDownloadError,
  type UpdateConfig,
  type CheckResult,
  type DownloadProgress,
} from '@/api/updater'
import { EventsOff } from '../../wailsjs/runtime/runtime'
import { OpenExternalURL } from '../../wailsjs/go/main/App'

const props = defineProps<{
  open: boolean
  initialResult?: CheckResult | null
}>()

const emit = defineEmits<{ close: [] }>()

type Phase = 'consent' | 'checking' | 'has_update' | 'no_update' | 'downloading' | 'done' | 'error'

const phase = ref<Phase>('checking')
const cfg = ref<UpdateConfig>({ network_consent: false, auto_check: false, auto_install: false })
const checkResult = ref<CheckResult | null>(null)
const cachedResult = ref<CheckResult | null>(null)
const autoInstall = ref(false)
const progress = ref<DownloadProgress | null>(null)
const installerPath = ref('')
const errorMsg = ref('')

const dialogTitle = computed(() => {
  if (phase.value === 'downloading') return '正在下载'
  if (phase.value === 'done') return '下载完成'
  return '检查更新'
})

// 清理 release body。
// 项目 CI 生成格式：[header] → --- → [版本说明(可选)] → ## 更新记录 → ... → --- → [footer]
// 优先提取 ## 更新记录 段（去掉标题本身）；找不到时兜底处理 GitHub 自动生成格式。
function cleanReleaseNotes(body: string): string {
  const normalized = body.replace(/\r\n/g, '\n').replace(/\r/g, '\n')

  // 主路径：提取 ## 更新记录 到下一个 --- 分隔符（或末尾）
  const m = normalized.match(/\n## 更新记录\n([\s\S]*?)(?:\n---(?:\n|$)|$)/)
  if (m) {
    return m[1].replace(/\n{3,}/g, '\n\n').trim()
  }

  // 兜底：按 --- 分段，取中间部分；再过滤 GitHub 自动生成内容
  const parts = normalized.split(/\n---\n/)
  let content = parts.length >= 3
    ? parts.slice(1, -1).join('\n---\n')
    : parts.length === 2
      ? parts[0]
      : normalized

  const lines = content.split('\n')
  const result: string[] = []
  let skip = false
  for (const line of lines) {
    if (/^\*\*Full Changelog\*\*:/i.test(line.trim())) continue
    if (/^##\s+(What's Changed|New Contributors)/i.test(line)) { skip = true; continue }
    if (skip && /^##/.test(line)) skip = false
    if (skip) continue
    result.push(line)
  }
  return result.join('\n').replace(/\n{3,}/g, '\n\n').trim()
}

function renderMarkdown(text: string): string {
  const cleaned = cleanReleaseNotes(text)
  if (!cleaned) return '<p style="color:var(--muted-foreground)">暂无更新说明</p>'
  const html = marked(cleaned) as string
  return html.replace(/<a\b[^>]*>([\s\S]*?)<\/a>/gi, '<span class="md-link">$1</span>')
}

const renderedNotes = computed(() => {
  const notes = checkResult.value?.release_notes
  if (!notes) return '<p style="color:var(--muted-foreground)">暂无更新说明</p>'
  return renderMarkdown(notes)
})

function formatSize(bytes: number): string {
  if (!bytes || bytes <= 0) return ''
  return `（${(bytes / 1024 / 1024).toFixed(1)} MB）`
}

function fmtMB(bytes: number): string {
  return (bytes / 1024 / 1024).toFixed(1) + ' MB'
}

async function init() {
  cfg.value = await getUpdateConfig()
  if (props.initialResult) {
    checkResult.value = props.initialResult
    cachedResult.value = props.initialResult
    phase.value = 'has_update'
  } else if (cachedResult.value) {
    // 本次会话已检查过，直接复用结果，无需重新请求
    checkResult.value = cachedResult.value
    phase.value = cachedResult.value.has_update ? 'has_update' : 'no_update'
  } else if (cfg.value.network_consent) {
    await doCheck()
  } else {
    phase.value = 'consent'
  }
}

async function onAgree() {
  cfg.value.network_consent = true
  await saveUpdateConfig(cfg.value)
  await doCheck()
}

async function doCheck() {
  phase.value = 'checking'
  errorMsg.value = ''
  try {
    const result = await checkUpdate()
    checkResult.value = result
    cachedResult.value = result
    phase.value = result.has_update ? 'has_update' : 'no_update'
  } catch (e: unknown) {
    errorMsg.value = '检查失败：' + (e instanceof Error ? e.message : String(e))
    phase.value = 'error'
  }
}

async function doRecheck() {
  cachedResult.value = null
  await doCheck()
}

function onDownloadClick() {
  phase.value = 'downloading'
  progress.value = null
  if (checkResult.value) {
    startDownload(checkResult.value.download_url, checkResult.value.asset_name, checkResult.value.asset_size)
  }
}

function onCancelClick() {
  cancelDownload()
  phase.value = 'has_update'
  progress.value = null
}

async function onInstallClick() {
  if (!installerPath.value) return
  try {
    await installRelease(installerPath.value, autoInstall.value)
    emit('close')
  } catch (e: unknown) {
    errorMsg.value = '启动安装程序失败：' + (e instanceof Error ? e.message : String(e))
    phase.value = 'error'
  }
}

async function onAutoCheckChange() {
  await saveUpdateConfig(cfg.value)
}

async function openReleaseURL() {
  if (!checkResult.value?.release_url) return
  try {
    await OpenExternalURL(checkResult.value.release_url)
  } catch {
    window.open(checkResult.value.release_url, '_blank')
  }
}

function onClose() {
  if (phase.value === 'downloading') cancelDownload()
  emit('close')
}

watch(
  () => props.open,
  async (val) => {
    if (val) {
      // 用 checking（spinner）作为初始态，避免 consent→checking 的闪动
      phase.value = 'checking'
      progress.value = null
      installerPath.value = ''
      errorMsg.value = ''
      autoInstall.value = false
      await init()
    }
  },
)

onMounted(() => {
  onDownloadProgress((p) => { progress.value = p })
  onDownloadDone((path) => {
    installerPath.value = path
    if (autoInstall.value) {
      installRelease(path, true)
        .then(() => emit('close'))
        .catch((e: unknown) => {
          errorMsg.value = '启动安装程序失败：' + (e instanceof Error ? e.message : String(e))
          phase.value = 'error'
        })
    } else {
      phase.value = 'done'
    }
  })
  onDownloadError((msg) => {
    errorMsg.value = msg
    phase.value = 'error'
  })
})

onUnmounted(() => {
  EventsOff('update:progress', 'update:done', 'update:error')
})
</script>

<template>
  <Dialog :open="open" @update:open="(v) => !v && onClose()">
    <DialogContent class="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>{{ dialogTitle }}</DialogTitle>
        <DialogDescription v-if="phase === 'has_update'">
          发现新版本 {{ checkResult?.latest_version }}，当前版本 {{ checkResult?.current_version }}
        </DialogDescription>
        <DialogDescription v-else-if="phase === 'no_update'">
          当前版本 {{ checkResult?.current_version }}
        </DialogDescription>
      </DialogHeader>

      <div class="phase-body">
      <!-- 联网同意 -->
      <template v-if="phase === 'consent'">
        <div class="space-y-2 py-1 text-sm text-muted-foreground">
          <p>检查更新需要连接网络访问 GitHub，这是清风输入法首次使用网络功能。</p>
          <p>本功能将访问以下地址以检查版本更新：</p>
          <ul class="list-inside list-disc space-y-1 pl-2">
            <li>api.github.com（GitHub 官方 API）</li>
            <li>镜像站点（网络不稳定时自动切换）</li>
          </ul>
          <p>不会收集任何个人信息或输入内容。</p>
        </div>
      </template>

      <!-- 检查中 -->
      <template v-else-if="phase === 'checking'">
        <div class="flex items-center gap-3 py-4 text-sm text-muted-foreground">
          <svg class="h-4 w-4 shrink-0 animate-spin" viewBox="0 0 24 24" fill="none">
            <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"/>
            <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v4a4 4 0 00-4 4H4z"/>
          </svg>
          正在检查更新…
        </div>
      </template>

      <!-- 有更新 -->
      <template v-else-if="phase === 'has_update'">
        <div
          class="markdown-body max-h-52 overflow-y-auto rounded border bg-muted p-3 text-sm"
          v-html="renderedNotes"
        />
      </template>

      <!-- 无更新 -->
      <template v-else-if="phase === 'no_update'">
        <div class="flex items-center gap-2 py-3 text-sm">
          <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" class="shrink-0 text-green-500">
            <path d="M20 6L9 17l-5-5"/>
          </svg>
          已是最新版本
        </div>
      </template>

      <!-- 下载中 -->
      <template v-else-if="phase === 'downloading'">
        <div class="space-y-3 py-1">
          <div class="h-2 w-full rounded-full bg-secondary">
            <div
              class="h-2 rounded-full bg-primary transition-all duration-200"
              :style="{ width: `${progress?.percent ?? 0}%` }"
            />
          </div>
          <p class="text-right text-sm text-muted-foreground">
            <template v-if="progress && progress.total > 0">
              {{ fmtMB(progress.downloaded) }} / {{ fmtMB(progress.total) }}（{{ Math.round(progress.percent) }}%）
            </template>
            <template v-else-if="progress">已下载 {{ fmtMB(progress.downloaded) }}</template>
            <template v-else>准备中…</template>
          </p>
        </div>
      </template>

      <!-- 下载完成 -->
      <template v-else-if="phase === 'done'">
        <div class="space-y-1 py-1 text-sm">
          <p>安装包已下载完成，可以立即安装。</p>
          <p class="text-muted-foreground">提示：点击安装后设置窗口将关闭，请确保已保存其他工作。</p>
        </div>
      </template>

      <!-- 错误 -->
      <template v-else-if="phase === 'error'">
        <p class="py-1 text-sm text-destructive">{{ errorMsg }}</p>
      </template>
      </div>

      <DialogFooter class="items-center gap-2">
        <!-- 同意 -->
        <template v-if="phase === 'consent'">
          <Button variant="outline" @click="onClose">取消</Button>
          <Button @click="onAgree">允许联网</Button>
        </template>

        <!-- 检查中 -->
        <template v-else-if="phase === 'checking'">
          <Button variant="outline" @click="onClose">取消</Button>
        </template>

        <!-- 有更新：选项行 + 按钮行 -->
        <template v-else-if="phase === 'has_update'">
          <div class="has-update-footer">
            <div class="footer-options">
              <label class="flex cursor-pointer items-center gap-1.5">
                <input type="checkbox" v-model="autoInstall" class="h-4 w-4 rounded" />
                静默安装
              </label>
              <label class="flex cursor-pointer items-center gap-1.5">
                <input type="checkbox" v-model="cfg.auto_check" @change="onAutoCheckChange" class="h-4 w-4 rounded" />
                自动检查
              </label>
              <span v-if="autoInstall" class="auto-install-tip">下载完成后将自动静默安装并关闭设置窗口</span>
            </div>
            <div class="footer-buttons">
              <Button variant="outline" @click="doRecheck" class="mr-auto">重新检查</Button>
              <Button variant="outline" @click="openReleaseURL">发布页面</Button>
              <Button variant="outline" @click="onClose">关闭</Button>
              <Button @click="onDownloadClick">
                {{ autoInstall ? '下载并安装' : '下载安装包' }}{{ formatSize(checkResult?.asset_size ?? 0) }}
              </Button>
            </div>
          </div>
        </template>

        <!-- 无更新：左侧自动检查 + 右侧重新检查+确定 -->
        <template v-else-if="phase === 'no_update'">
          <label class="mr-auto flex cursor-pointer items-center gap-1.5 text-sm text-muted-foreground">
            <input type="checkbox" v-model="cfg.auto_check" @change="onAutoCheckChange" class="h-4 w-4 rounded" />
            自动检查更新
          </label>
          <Button variant="outline" @click="doRecheck">重新检查</Button>
          <Button @click="onClose">确定</Button>
        </template>

        <!-- 下载中 -->
        <template v-else-if="phase === 'downloading'">
          <Button variant="outline" @click="onCancelClick">取消</Button>
        </template>

        <!-- 完成 -->
        <template v-else-if="phase === 'done'">
          <Button variant="outline" @click="onClose">关闭</Button>
          <Button @click="onInstallClick">立即安装</Button>
        </template>

        <!-- 错误：左侧自动检查 + 右侧重试+关闭 -->
        <template v-else-if="phase === 'error'">
          <label class="mr-auto flex cursor-pointer items-center gap-1.5 text-sm text-muted-foreground">
            <input type="checkbox" v-model="cfg.auto_check" @change="onAutoCheckChange" class="h-4 w-4 rounded" />
            自动检查更新
          </label>
          <Button variant="outline" @click="doRecheck">重试</Button>
          <Button @click="onClose">关闭</Button>
        </template>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

<style scoped>
.phase-body {
  min-height: 80px;
}

.has-update-footer {
  display: flex;
  flex-direction: column;
  gap: 10px;
  width: 100%;
}
.footer-options {
  display: flex;
  align-items: center;
  gap: 16px;
  font-size: 0.875rem;
  color: hsl(var(--muted-foreground));
}
.footer-buttons {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
.auto-install-tip {
  font-size: 0.75rem;
  color: hsl(var(--warning));
}

.markdown-body :deep(h1),
.markdown-body :deep(h2),
.markdown-body :deep(h3) {
  font-weight: 600;
  margin: 0.6em 0 0.2em;
  color: hsl(var(--foreground));
}
.markdown-body :deep(h1) { font-size: 1.05em; }
.markdown-body :deep(h2) { font-size: 1em; }
.markdown-body :deep(h3) { font-size: 0.95em; }
.markdown-body :deep(p) { margin: 0.35em 0; color: hsl(var(--foreground)); }
.markdown-body :deep(ul),
.markdown-body :deep(ol) { padding-left: 1.4em; margin: 0.35em 0; }
.markdown-body :deep(li) { margin: 0.1em 0; }
.markdown-body :deep(code) {
  background: hsl(var(--border));
  border-radius: 3px;
  padding: 0.1em 0.35em;
  font-size: 0.88em;
  font-family: monospace;
}
.markdown-body :deep(pre) {
  background: hsl(var(--border));
  border-radius: 6px;
  padding: 0.6em 0.8em;
  overflow-x: auto;
  margin: 0.4em 0;
}
.markdown-body :deep(pre code) { background: none; padding: 0; }
.markdown-body :deep(blockquote) {
  border-left: 3px solid hsl(var(--border));
  padding-left: 0.75em;
  color: hsl(var(--muted-foreground));
  margin: 0.4em 0;
}
.markdown-body :deep(.md-link) {
  color: hsl(var(--muted-foreground));
  text-decoration: underline;
  text-decoration-style: dashed;
  cursor: default;
}
.markdown-body :deep(hr) {
  border: none;
  border-top: 1px solid hsl(var(--border));
  margin: 0.6em 0;
}
</style>

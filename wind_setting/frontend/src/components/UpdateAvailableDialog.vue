<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
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
  startDownload,
  cancelDownload,
  installRelease,
  onDownloadProgress,
  onDownloadDone,
  onDownloadError,
  type CheckResult,
  type DownloadProgress,
} from '@/api/updater'
import { EventsOff } from '../../wailsjs/runtime/runtime'

const props = defineProps<{
  open: boolean
  result: CheckResult
}>()

const emit = defineEmits<{
  close: []
}>()

type Phase = 'info' | 'downloading' | 'done' | 'error'
const phase = ref<Phase>('info')
const autoInstall = ref(false)
const progress = ref<DownloadProgress | null>(null)
const installerPath = ref('')
const errorMsg = ref('')

const renderedNotes = computed(() => {
  const notes = props.result?.release_notes
  if (!notes) return '<p style="color:var(--muted-foreground);font-size:13px">暂无更新说明</p>'
  return marked(notes) as string
})

function formatSize(bytes: number): string {
  if (bytes <= 0) return ''
  return `（${(bytes / 1024 / 1024).toFixed(1)} MB）`
}

function fmtMB(bytes: number): string {
  return (bytes / 1024 / 1024).toFixed(1) + ' MB'
}

function openReleaseURL() {
  window.open(props.result.release_url, '_blank')
}

function onDownloadClick() {
  phase.value = 'downloading'
  progress.value = null
  startDownload(props.result.download_url, props.result.asset_name, props.result.asset_size)
}

function onCancelClick() {
  cancelDownload()
  phase.value = 'info'
  progress.value = null
}

async function onInstallClick() {
  await installRelease(installerPath.value, false)
  emit('close')
}

function onClose() {
  if (phase.value === 'downloading') {
    cancelDownload()
  }
  phase.value = 'info'
  progress.value = null
  errorMsg.value = ''
  emit('close')
}

onMounted(() => {
  onDownloadProgress((p) => {
    progress.value = p
  })
  onDownloadDone((path) => {
    installerPath.value = path
    if (autoInstall.value) {
      installRelease(path, true)
      emit('close')
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
        <DialogTitle>发现新版本 {{ result.latest_version }}</DialogTitle>
        <DialogDescription>当前版本：{{ result.current_version }}</DialogDescription>
      </DialogHeader>

      <!-- 阶段：详情 -->
      <template v-if="phase === 'info'">
        <div
          class="prose prose-sm max-h-52 overflow-y-auto rounded border bg-muted p-3 text-sm"
          v-html="renderedNotes"
        />
      </template>

      <!-- 阶段：下载中 -->
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
              {{ fmtMB(progress.downloaded) }} / {{ fmtMB(progress.total) }}
              （{{ Math.round(progress.percent) }}%）
            </template>
            <template v-else-if="progress">
              已下载 {{ fmtMB(progress.downloaded) }}
            </template>
            <template v-else>准备中…</template>
          </p>
        </div>
      </template>

      <!-- 阶段：完成 -->
      <template v-else-if="phase === 'done'">
        <p class="py-1 text-sm">安装包已下载完成，可以立即安装。</p>
      </template>

      <!-- 阶段：错误 -->
      <template v-else-if="phase === 'error'">
        <p class="py-1 text-sm text-destructive">{{ errorMsg }}</p>
      </template>

      <DialogFooter class="items-center gap-2">
        <!-- 左侧：静默安装勾选框（仅在 info 阶段显示） -->
        <label v-if="phase === 'info'" class="mr-auto flex cursor-pointer items-center gap-2 text-sm text-muted-foreground">
          <input type="checkbox" v-model="autoInstall" class="h-4 w-4 rounded" />
          静默安装
        </label>

        <!-- 右侧按钮 -->
        <template v-if="phase === 'info'">
          <Button variant="outline" @click="openReleaseURL">打开发布页面</Button>
          <Button variant="outline" @click="onClose">稍后</Button>
          <Button @click="onDownloadClick">
            {{ autoInstall ? '下载并安装' : '下载安装包' }}{{ formatSize(result.asset_size) }}
          </Button>
        </template>

        <template v-else-if="phase === 'downloading'">
          <Button variant="outline" @click="onCancelClick">取消</Button>
        </template>

        <template v-else-if="phase === 'done'">
          <Button variant="outline" @click="onClose">关闭</Button>
          <Button @click="onInstallClick">立即安装</Button>
        </template>

        <template v-else-if="phase === 'error'">
          <Button @click="onClose">关闭</Button>
        </template>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

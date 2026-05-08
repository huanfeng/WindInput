<script setup lang="ts">
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import type { DownloadProgress } from '@/api/updater'

defineProps<{
  open: boolean
  progress: DownloadProgress | null
  done: boolean
  error: string
  autoInstall: boolean
}>()

const emit = defineEmits<{
  cancel: []
  install: []
  close: []
}>()

function fmtMB(bytes: number): string {
  return (bytes / 1024 / 1024).toFixed(1) + ' MB'
}
</script>

<template>
  <Dialog :open="open" @update:open="(v) => !v && emit('close')">
    <DialogContent class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>
          {{ error ? '下载失败' : done ? '下载完成' : '正在下载更新…' }}
        </DialogTitle>
      </DialogHeader>

      <div class="space-y-3 py-2">
        <template v-if="error">
          <p class="text-sm text-destructive">{{ error }}</p>
        </template>

        <template v-else-if="done">
          <p class="text-sm">安装包已下载完成。</p>
          <p v-if="autoInstall" class="text-sm text-muted-foreground">
            正在启动安装程序，请稍候…
          </p>
        </template>

        <template v-else>
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
        </template>
      </div>

      <DialogFooter class="gap-2">
        <template v-if="error">
          <Button @click="emit('close')">关闭</Button>
        </template>
        <template v-else-if="done && !autoInstall">
          <Button variant="outline" @click="emit('close')">关闭</Button>
          <Button @click="emit('install')">立即安装</Button>
        </template>
        <template v-else-if="!done">
          <Button variant="outline" @click="emit('cancel')">取消</Button>
        </template>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

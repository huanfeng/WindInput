<script setup lang="ts">
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'

defineProps<{ open: boolean }>()

const emit = defineEmits<{
  agree: []
  cancel: []
}>()
</script>

<template>
  <Dialog :open="open" @update:open="(v) => !v && emit('cancel')">
    <DialogContent class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>联网功能提示</DialogTitle>
        <DialogDescription>
          检查更新需要连接网络访问 GitHub，这是清风输入法首次使用网络功能。
        </DialogDescription>
      </DialogHeader>
      <div class="space-y-2 py-2 text-sm text-muted-foreground">
        <p>本功能将访问以下地址以检查版本更新：</p>
        <ul class="list-disc list-inside space-y-1 pl-2">
          <li>api.github.com（GitHub 官方 API）</li>
          <li>镜像站点（网络不稳定时自动切换）</li>
        </ul>
        <p>不会收集任何个人信息或输入内容。</p>
      </div>
      <DialogFooter class="gap-2">
        <Button variant="outline" @click="emit('cancel')">取消</Button>
        <Button @click="emit('agree')">允许联网</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

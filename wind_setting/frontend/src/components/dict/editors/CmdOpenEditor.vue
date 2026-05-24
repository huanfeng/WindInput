<script setup lang="ts">
// 命令·打开 编辑器: 结构化输入单 action 的 $CC / $CC1 命令。
// 子类型:
//   url  → open("https://...")
//   app  → proc.run("path"[, "args"])
//   file → open("path")

import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from "@/components/ui/select";
import { pickExePath, pickAnyPath } from "@/api/wails";

export type CmdOpenSubKind = "url" | "app" | "file";

export interface CmdOpenBuffer {
  display: string;
  subKind: CmdOpenSubKind;
  target: string;
  args: string;
  prefixVisible: boolean;
}

const props = defineProps<{ modelValue: CmdOpenBuffer }>();
const emit = defineEmits<{
  (e: "update:modelValue", value: CmdOpenBuffer): void;
}>();

function update(patch: Partial<CmdOpenBuffer>) {
  emit("update:modelValue", { ...props.modelValue, ...patch });
}

async function pickApp() {
  try {
    const p = await pickExePath();
    if (p) update({ target: p });
  } catch (_e) {
    // 用户取消或失败, 忽略
  }
}

async function pickFile() {
  try {
    const p = await pickAnyPath();
    if (p) update({ target: p });
  } catch (_e) {
    // 用户取消或失败, 忽略
  }
}

function targetPlaceholder(kind: CmdOpenSubKind): string {
  switch (kind) {
    case "url":
      return "https://example.com";
    case "app":
      return "C:\\Program Files\\App\\app.exe";
    case "file":
      return "C:\\path\\to\\file";
  }
}
</script>

<template>
  <div class="space-y-2">
    <!-- 显示名 -->
    <div class="grid grid-cols-[80px_1fr] items-center gap-2">
      <label class="text-sm font-medium text-right">显示名</label>
      <Input
        :model-value="modelValue.display"
        @update:model-value="update({ display: String($event ?? '') })"
        placeholder="如: 打开百度"
      />
    </div>

    <!-- 子类型 -->
    <div class="grid grid-cols-[80px_1fr] items-center gap-2">
      <label class="text-sm font-medium text-right">类型</label>
      <Select
        :model-value="modelValue.subKind"
        @update:model-value="
          update({ subKind: ($event as CmdOpenSubKind) || 'url' })
        "
      >
        <SelectTrigger class="w-40">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="url">URL</SelectItem>
          <SelectItem value="app">程序</SelectItem>
          <SelectItem value="file">文件</SelectItem>
        </SelectContent>
      </Select>
    </div>

    <!-- 路径 / URL -->
    <div class="grid grid-cols-[80px_1fr] items-center gap-2">
      <label class="text-sm font-medium text-right">
        {{ modelValue.subKind === "url" ? "URL" : "路径" }}
      </label>
      <div class="flex gap-2 items-center">
        <Input
          :model-value="modelValue.target"
          @update:model-value="update({ target: String($event ?? '') })"
          :placeholder="targetPlaceholder(modelValue.subKind)"
          class="flex-1"
        />
        <Button
          v-if="modelValue.subKind === 'app'"
          type="button"
          variant="outline"
          size="sm"
          class="h-9"
          @click="pickApp"
        >
          选择...
        </Button>
        <Button
          v-else-if="modelValue.subKind === 'file'"
          type="button"
          variant="outline"
          size="sm"
          class="h-9"
          @click="pickFile"
        >
          选择...
        </Button>
      </div>
    </div>

    <!-- 参数 (仅程序) -->
    <div
      v-if="modelValue.subKind === 'app'"
      class="grid grid-cols-[80px_1fr] items-center gap-2"
    >
      <label class="text-sm font-medium text-right">参数</label>
      <Input
        :model-value="modelValue.args"
        @update:model-value="update({ args: String($event ?? '') })"
        placeholder="可选, 启动参数"
      />
    </div>

    <!-- 前缀展开 -->
    <div class="grid grid-cols-[80px_1fr] items-center gap-2">
      <span></span>
      <label class="flex items-center gap-2 text-sm cursor-pointer select-none">
        <Checkbox
          :checked="modelValue.prefixVisible"
          @update:checked="update({ prefixVisible: !!$event })"
        />
        <span>允许前缀展开 (使用 $CC1)</span>
      </label>
    </div>
  </div>
</template>

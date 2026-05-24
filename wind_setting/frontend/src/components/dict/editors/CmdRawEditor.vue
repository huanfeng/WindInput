<script setup lang="ts">
// 命令·手动 编辑器: 用户直接书写 $CC(...) / $CC1(...) 内容。

export interface CmdRawBuffer {
  text: string;
}

const props = defineProps<{ modelValue: CmdRawBuffer }>();
const emit = defineEmits<{
  (e: "update:modelValue", value: CmdRawBuffer): void;
}>();

function updateText(text: string) {
  emit("update:modelValue", { ...props.modelValue, text });
}
</script>

<template>
  <div class="space-y-1.5">
    <textarea
      :value="modelValue.text"
      @input="updateText(($event.target as HTMLTextAreaElement).value)"
      rows="2"
      placeholder='$CC("显示名", open("https://..."))  或  $CC1("展开前缀", proc.run("notepad.exe"))'
      class="flex w-full rounded-md border border-input bg-transparent px-3 py-2 text-sm font-mono shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring resize-y"
    />
    <div class="text-[11px] text-muted-foreground">
      支持 open / proc.run / proc.shell / key.tap / clip.copy 等动作, 多条用 + 串联。
    </div>
  </div>
</template>

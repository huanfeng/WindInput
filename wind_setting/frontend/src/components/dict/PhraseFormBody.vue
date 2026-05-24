<script setup lang="ts">
// 短语 / 用户词库 通用表单体:
//   分类 + 编码 (并排) → 内容 (子编辑器) → 预览 (单行) → 权重
// 状态由父组件通过 v-model 持有 PhraseFormState; 本组件负责:
//   - 切换 editorType 时把已合成文本暂存到 normal buffer
//   - 切换到结构化类型时反向解析 normal buffer
//   - 暴露 composedText 和 hasValidationError 给父
import { computed, ref, watch } from "vue";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from "@/components/ui/select";
import CmdbarValuePreview from "./CmdbarValuePreview.vue";
import HintTooltip from "./HintTooltip.vue";

const WEIGHT_HINT_HEAD = "范围: 0~10000";
const WEIGHT_HINT_ROWS: ReadonlyArray<string> = [
  "1000 默认中间值",
  "< 500 低频",
  "> 5000 高频",
  "> 8000 优先",
];
import NormalEditor from "./editors/NormalEditor.vue";
import CmdOpenEditor from "./editors/CmdOpenEditor.vue";
import CmdRawEditor from "./editors/CmdRawEditor.vue";
import ArrayEditor from "./editors/ArrayEditor.vue";
import ArraySSEditor from "./editors/ArraySSEditor.vue";
import type {
  CmdOpenBuffer,
  CmdOpenSubKind,
} from "./editors/CmdOpenEditor.vue";
import type {
  ArraySSBuffer,
  EditorType,
  PhraseFormState,
} from "./phraseForm";

const props = withDefaults(
  defineProps<{
    modelValue: PhraseFormState;
    showCodeGen?: boolean;
    onGenerateCode?: () => void;
    codeGenerating?: boolean;
    codePlaceholder?: string;
    codeLabel?: string;
    codeHint?: string;
  }>(),
  {
    showCodeGen: false,
    codeGenerating: false,
    codePlaceholder: "如: zdy",
    codeLabel: "编码",
    codeHint: "",
  },
);

const emit = defineEmits<{
  (e: "update:modelValue", value: PhraseFormState): void;
  (e: "composed-text", text: string): void;
  (e: "validation-error", hasError: boolean): void;
  // 用户在编码输入框手动改动 (区别于程序化 setCode 调用, 这里只在 input
  // 事件中触发, 供父组件追踪"用户手改编码"状态)。
  (e: "code-input"): void;
}>();

// 双向: 直接复用 props.modelValue 的引用 (Vue 中通过 v-model 父子共享同一对象)
const state = computed(() => props.modelValue);

const hasValidationError = ref(false);

function emitState() {
  emit("update:modelValue", state.value);
}

function setCode(v: string) {
  state.value.code = v;
  emitState();
}

// 用户在编码框 input/keyup 时触发, 表明这是手动编辑 (而非程序化的自动生成)
function onCodeUserInput() {
  emit("code-input");
}

function setWeight(v: number) {
  state.value.weight = v;
  emitState();
}

// ── 合成文本 ──
function escapeStr(s: string): string {
  return JSON.stringify(s ?? "");
}

function composeCmdOpen(b: CmdOpenBuffer): string {
  const marker = b.prefixVisible ? "$CC1" : "$CC";
  const disp = escapeStr(b.display);
  let action: string;
  if (b.subKind === "url" || b.subKind === "file") {
    action = `open(${escapeStr(b.target)})`;
  } else {
    action = b.args.trim()
      ? `proc.run(${escapeStr(b.target)}, ${escapeStr(b.args)})`
      : `proc.run(${escapeStr(b.target)})`;
  }
  return `${marker}(${disp}, ${action})`;
}

function composeArray(b: { name: string; chars: string }): string {
  return `$AA(${escapeStr(b.name)}, ${escapeStr(b.chars)})`;
}

function composeArraySS(b: ArraySSBuffer): string {
  const args: string[] = [escapeStr(b.name)];
  for (const e of b.elements) {
    if (e.kind === "string") {
      args.push(escapeStr(e.text));
    } else {
      const disp = escapeStr(e.display);
      let action: string;
      if (e.subKind === "url" || e.subKind === "file") {
        action = `open(${escapeStr(e.target)})`;
      } else {
        action = e.args.trim()
          ? `proc.run(${escapeStr(e.target)}, ${escapeStr(e.args)})`
          : `proc.run(${escapeStr(e.target)})`;
      }
      args.push(`$CC(${disp}, ${action})`);
    }
  }
  return `$SS(${args.join(", ")})`;
}

// 编码生成按钮是否可用: 当前 editor 的"内容"非空才允许生成
const hasContentText = computed<boolean>(() => {
  const s = state.value;
  switch (s.editorType) {
    case "normal":
      return s.buffers.normal.text.trim().length > 0;
    case "cmd-open":
      return (
        s.buffers.cmdOpen.display.trim().length > 0 ||
        s.buffers.cmdOpen.target.trim().length > 0
      );
    case "cmd-raw":
      return s.buffers.cmdRaw.text.trim().length > 0;
    case "array":
      return (
        s.buffers.array.name.trim().length > 0 ||
        s.buffers.array.chars.trim().length > 0
      );
    case "array-ss":
      return (
        s.buffers.arraySS.name.trim().length > 0 ||
        s.buffers.arraySS.elements.length > 0
      );
  }
  return false;
});

const composedText = computed<string>(() => {
  const s = state.value;
  switch (s.editorType) {
    case "normal":
      return s.buffers.normal.text;
    case "cmd-open":
      return composeCmdOpen(s.buffers.cmdOpen);
    case "cmd-raw":
      return s.buffers.cmdRaw.text;
    case "array":
      return composeArray(s.buffers.array);
    case "array-ss":
      return composeArraySS(s.buffers.arraySS);
  }
  return "";
});

watch(
  composedText,
  (v) => {
    emit("composed-text", v);
  },
  { immediate: true },
);

// ── 反向解析 ──
const cmdOpenRE =
  /^\$CC(1)?\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*(open|proc\.run)\(\s*"((?:[^"\\]|\\.)*)"(?:\s*,\s*"((?:[^"\\]|\\.)*)")?\s*\)\s*\)$/;

function matchesCmdOpen(text: string): RegExpMatchArray | null {
  return text.trim().match(cmdOpenRE);
}

function inferType(text: string): EditorType {
  const t = (text ?? "").trim();
  if (t.startsWith("$AA(")) return "array";
  if (t.startsWith("$SS(")) return "array-ss";
  if (t.startsWith("$CC1(") || t.startsWith("$CC(")) {
    if (matchesCmdOpen(t)) return "cmd-open";
    return "cmd-raw";
  }
  return "normal";
}

function unquote(literal: string): string {
  try {
    return JSON.parse(`"${literal}"`);
  } catch {
    return literal;
  }
}

function parseCmdOpenInto(text: string, buf: CmdOpenBuffer): boolean {
  const m = matchesCmdOpen(text);
  if (!m) return false;
  const prefix1 = !!m[1];
  const display = unquote(m[2] ?? "");
  const verb = m[3] as "open" | "proc.run";
  const target = unquote(m[4] ?? "");
  const args = m[5] !== undefined ? unquote(m[5]) : "";
  let subKind: CmdOpenSubKind;
  if (/^https?:\/\//i.test(target)) {
    subKind = "url";
  } else if (verb === "proc.run") {
    subKind = "app";
  } else {
    subKind = "file";
  }
  buf.display = display;
  buf.subKind = subKind;
  buf.target = target;
  buf.args = args;
  buf.prefixVisible = prefix1;
  return true;
}

const arrayRE = /^\$AA\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)"\s*\)$/;

function parseArrayInto(
  text: string,
  buf: { name: string; chars: string },
): boolean {
  const m = text.trim().match(arrayRE);
  if (!m) return false;
  buf.name = unquote(m[1] ?? "");
  buf.chars = unquote(m[2] ?? "");
  return true;
}

// $SS 反向解析: 当前仅静态提取 name (第一个字符串字面量), elements
// 由 cmdbar parser 在保存阶段重新求值; 用户切换到 array-ss 后看到 name
// 但需要在 UI 中重新添加元素。完整反向解析含嵌套 $CC 留待后续完善。
const ssNameRE = /^\$SS\(\s*"((?:[^"\\]|\\.)*)"/;

function parseArraySSInto(text: string, buf: ArraySSBuffer): boolean {
  const m = text.trim().match(ssNameRE);
  if (!m) return false;
  buf.name = unquote(m[1] ?? "");
  // elements 暂留, 不清空 (用户切换回来时如果之前编辑过, 应保留)
  return true;
}

function handleEditorTypeChange(next: EditorType) {
  const s = state.value;
  if (next === s.editorType) return;
  const prev = s.editorType;
  const prevText = composedText.value;
  if (prev !== "normal") {
    s.buffers.normal.text = prevText;
  }
  s.editorType = next;

  if (next === "normal") {
    emitState();
    return;
  }

  const sourceText = s.buffers.normal.text;
  const inferred = inferType(sourceText);
  if (next === "cmd-open") {
    if (
      inferred === "cmd-open" &&
      parseCmdOpenInto(sourceText, s.buffers.cmdOpen)
    ) {
      emitState();
      return;
    }
    s.buffers.cmdOpen.display = "";
    s.buffers.cmdOpen.subKind = "url";
    s.buffers.cmdOpen.target = "";
    s.buffers.cmdOpen.args = "";
    s.buffers.cmdOpen.prefixVisible = false;
    emitState();
    return;
  }
  if (next === "cmd-raw") {
    if (prev === "cmd-open") {
      s.buffers.cmdRaw.text = prevText;
      emitState();
      return;
    }
    s.buffers.cmdRaw.text = sourceText;
    emitState();
    return;
  }
  if (next === "array") {
    if (inferred === "array" && parseArrayInto(sourceText, s.buffers.array)) {
      emitState();
      return;
    }
    s.buffers.array.name = "";
    s.buffers.array.chars = "";
    emitState();
    return;
  }
  if (next === "array-ss") {
    if (
      inferred === "array-ss" &&
      parseArraySSInto(sourceText, s.buffers.arraySS)
    ) {
      emitState();
      return;
    }
    s.buffers.arraySS.name = "";
    s.buffers.arraySS.elements = [];
    emitState();
  }
}

function onValidationError(hasError: boolean) {
  hasValidationError.value = hasError;
  emit("validation-error", hasError);
}

defineExpose({
  composedText,
  hasValidationError,
});
</script>

<template>
  <div class="space-y-4">
    <!-- 类型 + 权重: 按内容宽度收缩, 整体居左 -->
    <!-- items-start + 两 label 都用 inline-flex h-5, 保证 label 行高一致, -->
    <!-- input 起始位置自然对齐 (Select/Input 都是 h-9, 顶/底自动同步)。 -->
    <div class="flex items-start gap-3">
      <div class="space-y-1.5">
        <label
          class="text-sm font-medium inline-flex items-center gap-1 h-5 leading-5"
        >
          <span>类型</span>
          <HintTooltip
            text="切换不兼容的类型会修改内容"
            aria-label="类型切换说明"
          />
        </label>
        <Select
          :model-value="state.editorType"
          @update:model-value="
            handleEditorTypeChange(($event as EditorType) || 'normal')
          "
        >
          <SelectTrigger class="w-[180px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent class="z-[1100]">
            <SelectItem value="normal">普通</SelectItem>
            <SelectItem value="cmd-open">命令·打开</SelectItem>
            <SelectItem value="cmd-raw">命令·手动</SelectItem>
            <SelectItem value="array">字符组</SelectItem>
            <SelectItem value="array-ss">字符串组</SelectItem>
          </SelectContent>
        </Select>
      </div>

      <div class="space-y-1.5">
        <label
          class="text-sm font-medium inline-flex items-center gap-1 h-5 leading-5"
        >
          <span>权重</span>
          <HintTooltip
            :head="WEIGHT_HINT_HEAD"
            :rows="WEIGHT_HINT_ROWS"
            aria-label="权重档位说明"
          />
        </label>
        <Input
          :model-value="state.weight"
          @update:model-value="setWeight(Number($event ?? 0))"
          type="number"
          :min="0"
          :max="10000"
          class="w-[120px]"
        />
      </div>
    </div>

    <!-- 编码: 独立一行 -->
    <div class="space-y-1.5">
      <label class="text-sm font-medium inline-flex items-baseline gap-2">
        <span>{{ codeLabel }}</span>
        <span v-if="codeHint" class="text-xs font-normal text-muted-foreground">
          {{ codeHint }}
        </span>
      </label>
      <div v-if="showCodeGen" class="flex gap-1.5 items-center">
        <Input
          :model-value="state.code"
          @update:model-value="setCode(String($event ?? ''))"
          @input="onCodeUserInput"
          :placeholder="codePlaceholder"
          :class="codeGenerating ? 'opacity-60' : ''"
          class="flex-1"
        />
        <Button
          type="button"
          variant="outline"
          size="sm"
          :disabled="codeGenerating || !hasContentText"
          title="重新生成编码"
          class="h-9"
          @click="onGenerateCode && onGenerateCode()"
        >
          ↺
        </Button>
      </div>
      <Input
        v-else
        :model-value="state.code"
        @update:model-value="setCode(String($event ?? ''))"
        @input="onCodeUserInput"
        :placeholder="codePlaceholder"
      />
    </div>

    <!-- 内容 -->
    <div class="space-y-1.5">
      <label class="text-sm font-medium">内容</label>
      <NormalEditor
        v-if="state.editorType === 'normal'"
        v-model="state.buffers.normal"
      />
      <CmdOpenEditor
        v-else-if="state.editorType === 'cmd-open'"
        v-model="state.buffers.cmdOpen"
      />
      <CmdRawEditor
        v-else-if="state.editorType === 'cmd-raw'"
        v-model="state.buffers.cmdRaw"
      />
      <ArrayEditor
        v-else-if="state.editorType === 'array'"
        v-model="state.buffers.array"
      />
      <ArraySSEditor
        v-else-if="state.editorType === 'array-ss'"
        v-model="state.buffers.arraySS"
      />

      <!-- 预览 (紧贴内容下方, 单行) -->
      <CmdbarValuePreview
        :value="composedText"
        @validation-error="onValidationError"
      />
    </div>
  </div>
</template>

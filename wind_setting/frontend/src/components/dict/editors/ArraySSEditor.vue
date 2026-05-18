<script setup lang="ts">
// 字符串数组编辑器: $SS("名称", elem1, elem2, ...) 的结构化输入。
// 元素可为字符串字面量或 $CC 命令 (CmdOpen 子集, 无 prefixVisible — 组级
// prefix 由外层 $SS modifier 控制)。
//
// 设计参考 docs/design/command-bar-followup.md §4.3。

import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from "@/components/ui/select";
import { Trash2, ChevronUp, ChevronDown, Plus } from "lucide-vue-next";
import { pickExePath, pickAnyPath } from "@/api/wails";
import {
  type ArraySSBuffer,
  type ArraySSElement,
  newArraySSStringElement,
  newArraySSCmdElement,
} from "../phraseForm";
import type { CmdOpenSubKind } from "./CmdOpenEditor.vue";

const props = defineProps<{ modelValue: ArraySSBuffer }>();
const emit = defineEmits<{
  (e: "update:modelValue", value: ArraySSBuffer): void;
}>();

function emitState() {
  emit("update:modelValue", { ...props.modelValue });
}

function updateName(name: string) {
  emit("update:modelValue", { ...props.modelValue, name });
}

function updateElementAt(i: number, patch: Partial<ArraySSElement>) {
  const elements = props.modelValue.elements.slice();
  // patch 字段必须与当前 kind 匹配; 这里允许 kind 切换由专门的函数处理
  elements[i] = { ...elements[i], ...patch } as ArraySSElement;
  emit("update:modelValue", { ...props.modelValue, elements });
}

function switchElementKind(i: number, kind: "string" | "cmd") {
  const elements = props.modelValue.elements.slice();
  if (kind === "string") {
    elements[i] = newArraySSStringElement("");
  } else {
    elements[i] = newArraySSCmdElement();
  }
  emit("update:modelValue", { ...props.modelValue, elements });
}

function addElement(kind: "string" | "cmd") {
  const elements = props.modelValue.elements.slice();
  elements.push(
    kind === "string" ? newArraySSStringElement("") : newArraySSCmdElement(),
  );
  emit("update:modelValue", { ...props.modelValue, elements });
}

function removeElement(i: number) {
  const elements = props.modelValue.elements.slice();
  elements.splice(i, 1);
  emit("update:modelValue", { ...props.modelValue, elements });
}

function moveElement(i: number, dir: -1 | 1) {
  const next = i + dir;
  if (next < 0 || next >= props.modelValue.elements.length) return;
  const elements = props.modelValue.elements.slice();
  [elements[i], elements[next]] = [elements[next], elements[i]];
  emit("update:modelValue", { ...props.modelValue, elements });
}

async function pickCmdApp(i: number) {
  try {
    const p = await pickExePath();
    if (p) updateElementAt(i, { target: p } as Partial<ArraySSElement>);
  } catch (_e) {
    // ignored
  }
}

async function pickCmdFile(i: number) {
  try {
    const p = await pickAnyPath();
    if (p) updateElementAt(i, { target: p } as Partial<ArraySSElement>);
  } catch (_e) {
    // ignored
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

// 为模板里 v-if 收窄做 helper (Vue/TS narrowing 在 v-for 里不靠 type guard 起作用)
function isStringEl(e: ArraySSElement): e is { kind: "string"; text: string } {
  return e.kind === "string";
}
function isCmdEl(
  e: ArraySSElement,
): e is {
  kind: "cmd";
  display: string;
  subKind: CmdOpenSubKind;
  target: string;
  args: string;
} {
  return e.kind === "cmd";
}

// 保留以避免 emitState 被 lint 标 unused (该函数预留给将来 batch 操作场景)
void emitState;
</script>

<template>
  <div class="space-y-3">
    <!-- 组名 -->
    <div class="grid grid-cols-[80px_1fr] items-center gap-2">
      <label class="text-sm font-medium text-right">名称</label>
      <Input
        :model-value="modelValue.name"
        @update:model-value="updateName(String($event ?? ''))"
        placeholder="如: 常用网址"
      />
    </div>

    <!-- 元素列表 -->
    <div class="space-y-2">
      <div class="text-sm font-medium">元素列表</div>
      <div
        v-if="modelValue.elements.length === 0"
        class="text-xs text-muted-foreground italic px-3 py-2 border border-dashed rounded"
      >
        （还没有元素，点下方按钮添加）
      </div>
      <div
        v-for="(el, i) in modelValue.elements"
        :key="i"
        class="border rounded p-2 space-y-2 bg-card"
      >
        <!-- 元素头部: 序号 + 类型 + 移动/删除 -->
        <div class="flex items-center gap-2">
          <span class="text-xs text-muted-foreground w-6">#{{ i + 1 }}</span>
          <Select
            :model-value="el.kind"
            @update:model-value="
              switchElementKind(i, ($event as 'string' | 'cmd') || 'string')
            "
          >
            <SelectTrigger class="w-28 h-8 text-xs">
              <SelectValue />
            </SelectTrigger>
            <SelectContent class="z-[1100]">
              <SelectItem value="string">字符串</SelectItem>
              <SelectItem value="cmd">命令</SelectItem>
            </SelectContent>
          </Select>
          <div class="flex-1"></div>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            class="h-7 w-7"
            :disabled="i === 0"
            @click="moveElement(i, -1)"
            title="上移"
          >
            <ChevronUp class="h-4 w-4" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            class="h-7 w-7"
            :disabled="i === modelValue.elements.length - 1"
            @click="moveElement(i, 1)"
            title="下移"
          >
            <ChevronDown class="h-4 w-4" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            class="h-7 w-7 text-destructive hover:bg-destructive/10"
            @click="removeElement(i)"
            title="删除"
          >
            <Trash2 class="h-4 w-4" />
          </Button>
        </div>

        <!-- 字符串元素 -->
        <template v-if="isStringEl(el)">
          <textarea
            :value="el.text"
            @input="
              updateElementAt(i, {
                text: ($event.target as HTMLTextAreaElement).value,
              } as Partial<ArraySSElement>)
            "
            rows="2"
            placeholder="此元素的字面文本 (会作为候选直接上屏)"
            class="flex w-full rounded-md border border-input bg-transparent px-3 py-2 text-sm shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring resize-y"
          />
        </template>

        <!-- 命令元素 -->
        <template v-else-if="isCmdEl(el)">
          <div class="space-y-1.5 pl-8">
            <div class="grid grid-cols-[64px_1fr] items-center gap-2">
              <label class="text-xs text-right text-muted-foreground"
                >显示名</label
              >
              <Input
                :model-value="el.display"
                @update:model-value="
                  updateElementAt(i, {
                    display: String($event ?? ''),
                  } as Partial<ArraySSElement>)
                "
                placeholder="如: 打开主页"
                class="h-8 text-sm"
              />
            </div>
            <div class="grid grid-cols-[64px_1fr] items-center gap-2">
              <label class="text-xs text-right text-muted-foreground"
                >子类型</label
              >
              <Select
                :model-value="el.subKind"
                @update:model-value="
                  updateElementAt(i, {
                    subKind: ($event as CmdOpenSubKind) || 'url',
                  } as Partial<ArraySSElement>)
                "
              >
                <SelectTrigger class="w-32 h-8 text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent class="z-[1100]">
                  <SelectItem value="url">URL</SelectItem>
                  <SelectItem value="app">程序</SelectItem>
                  <SelectItem value="file">文件</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div class="grid grid-cols-[64px_1fr] items-center gap-2">
              <label class="text-xs text-right text-muted-foreground">
                {{ el.subKind === "url" ? "URL" : "路径" }}
              </label>
              <div class="flex gap-2 items-center">
                <Input
                  :model-value="el.target"
                  @update:model-value="
                    updateElementAt(i, {
                      target: String($event ?? ''),
                    } as Partial<ArraySSElement>)
                  "
                  :placeholder="targetPlaceholder(el.subKind)"
                  class="h-8 text-sm flex-1"
                />
                <Button
                  v-if="el.subKind === 'app'"
                  type="button"
                  variant="outline"
                  size="sm"
                  class="h-8 text-xs"
                  @click="pickCmdApp(i)"
                >
                  选择...
                </Button>
                <Button
                  v-else-if="el.subKind === 'file'"
                  type="button"
                  variant="outline"
                  size="sm"
                  class="h-8 text-xs"
                  @click="pickCmdFile(i)"
                >
                  选择...
                </Button>
              </div>
            </div>
            <div
              v-if="el.subKind === 'app'"
              class="grid grid-cols-[64px_1fr] items-center gap-2"
            >
              <label class="text-xs text-right text-muted-foreground"
                >参数</label
              >
              <Input
                :model-value="el.args"
                @update:model-value="
                  updateElementAt(i, {
                    args: String($event ?? ''),
                  } as Partial<ArraySSElement>)
                "
                placeholder="可选, 启动参数"
                class="h-8 text-sm"
              />
            </div>
          </div>
        </template>
      </div>

      <!-- 加按钮 -->
      <div class="flex gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          class="h-8 text-xs"
          @click="addElement('string')"
        >
          <Plus class="h-3.5 w-3.5 mr-1" />加字符串
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          class="h-8 text-xs"
          @click="addElement('cmd')"
        >
          <Plus class="h-3.5 w-3.5 mr-1" />加命令
        </Button>
      </div>
    </div>
  </div>
</template>

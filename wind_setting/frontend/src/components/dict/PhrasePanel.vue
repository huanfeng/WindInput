<script setup lang="ts">
import { h, ref, computed, onMounted, onUnmounted } from "vue";
import type { ColumnDef } from "@tanstack/vue-table";
import {
  getPhraseList,
  addPhrase,
  updatePhrase,
  removePhrase,
  removePhrases,
  setPhraseEnabled,
  resetPhrasesToDefault,
  type PhraseItem,
} from "@/api/wails";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import DictDataTable from "./DictDataTable.vue";
import PhraseFormBody from "./PhraseFormBody.vue";
import { createEmptyPhraseFormState, type PhraseFormState } from "./phraseForm";
import { Checkbox } from "@/components/ui/checkbox";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
const { toast } = useToast();
const { confirm } = useConfirm();

const emit = defineEmits<{
  (e: "loading", value: boolean): void;
}>();

// ── State ──
const loading = ref(false);
const allPhrases = ref<PhraseItem[]>([]);
const selectedKeys = ref<Set<string>>(new Set());
const dialogVisible = ref(false);
const editingPhrase = ref<PhraseItem | null>(null);
const formState = ref<PhraseFormState>(createEmptyPhraseFormState());
const composedText = ref("");
const hasValidationError = ref(false);
// 连续添加: 仅在添加场景下生效, 勾选后保存不关闭对话框, 自动清空表单。
// 默认不勾选 (保存后关闭, 与编辑场景一致)。
const continuousAdd = ref(false);

function phraseKey(item: PhraseItem): string {
  return `${item.code}||${item.text || ""}`;
}

function itemContent(item: PhraseItem): string {
  return item.text || "";
}

// ── 类型推断: 用于编辑时加载到合适的 editor ──
function inferEditorTypeForLoad(text: string): PhraseFormState["editorType"] {
  const t = (text ?? "").trim();
  if (t.startsWith("$AA(")) return "array";
  if (t.startsWith("$CC1(") || t.startsWith("$CC(")) {
    const re =
      /^\$CC(1)?\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*(open|run)\(\s*"((?:[^"\\]|\\.)*)"(?:\s*,\s*"((?:[^"\\]|\\.)*)")?\s*\)\s*\)$/;
    if (re.test(t)) return "cmd-open";
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

function loadFormFromText(state: PhraseFormState, text: string) {
  const t = (text ?? "").trim();
  const kind = inferEditorTypeForLoad(t);
  state.editorType = kind;
  state.buffers.normal.text = text;
  if (kind === "cmd-open") {
    const re =
      /^\$CC(1)?\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*(open|run)\(\s*"((?:[^"\\]|\\.)*)"(?:\s*,\s*"((?:[^"\\]|\\.)*)")?\s*\)\s*\)$/;
    const m = t.match(re);
    if (m) {
      const prefix1 = !!m[1];
      const display = unquote(m[2] ?? "");
      const verb = m[3] as "open" | "run";
      const target = unquote(m[4] ?? "");
      const args = m[5] !== undefined ? unquote(m[5]) : "";
      state.buffers.cmdOpen.display = display;
      state.buffers.cmdOpen.target = target;
      state.buffers.cmdOpen.args = args;
      state.buffers.cmdOpen.prefixVisible = prefix1;
      if (/^https?:\/\//i.test(target)) {
        state.buffers.cmdOpen.subKind = "url";
      } else if (verb === "run") {
        state.buffers.cmdOpen.subKind = "app";
      } else {
        state.buffers.cmdOpen.subKind = "file";
      }
    }
    return;
  }
  if (kind === "cmd-raw") {
    state.buffers.cmdRaw.text = text;
    return;
  }
  if (kind === "array") {
    const arrayRE =
      /^\$AA\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)"\s*\)$/;
    const m = t.match(arrayRE);
    if (m) {
      state.buffers.array.name = unquote(m[1] ?? "");
      state.buffers.array.chars = unquote(m[2] ?? "");
    }
  }
}

// ── Columns ──
const columns: ColumnDef<PhraseItem, any>[] = [
  {
    id: "select",
    size: 32,
    enableSorting: false,
    header: ({ table }) =>
      h(Checkbox, {
        checked: table.getIsAllPageRowsSelected(),
        "onUpdate:checked": (val: boolean) =>
          table.toggleAllPageRowsSelected(val),
      }),
    cell: ({ row }) =>
      h(Checkbox, {
        checked: row.getIsSelected(),
        "onUpdate:checked": (val: boolean) => row.toggleSelected(val),
      }),
  },
  {
    accessorKey: "enabled",
    header: "启用",
    size: 56,
    enableSorting: false,
    cell: ({ row }) =>
      h(Switch, {
        checked: row.original.enabled,
        "onUpdate:checked": () => handleToggleEnabled(row.original),
        class: "scale-75",
      }),
  },
  {
    accessorKey: "code",
    header: "编码",
    // 编码列: 容纳常见短编码 (date / zzbd) 不被压缩; 长编码 break-all 换行。
    size: 100,
    minSize: 80,
    cell: ({ row }) =>
      h(
        "span",
        {
          class:
            "font-mono text-sm text-muted-foreground bg-secondary px-2 py-0.5 rounded inline-block break-all align-middle",
        },
        row.getValue("code"),
      ),
  },
  {
    id: "content",
    header: "内容",
    // 内容列适度收窄, 允许 break-words 多行换行, 保留 hover title。
    size: 220,
    minSize: 160,
    accessorFn: (row) => itemContent(row),
    cell: ({ row }) => {
      const text = row.original.text || "";
      return h(
        "span",
        {
          class: "text-sm block whitespace-normal break-words align-middle",
          title: text,
        },
        text,
      );
    },
  },
  {
    accessorKey: "weight",
    header: "权重",
    size: 80,
    minSize: 60,
    cell: ({ row }) => {
      const w =
        typeof row.original.effective_weight === "number"
          ? row.original.effective_weight
          : (row.original.weight ?? 0);
      return h("span", { class: "font-mono text-sm tabular-nums" }, String(w));
    },
  },
  {
    id: "actions",
    size: 140,
    minSize: 120,
    enableSorting: false,
    cell: ({ row }) =>
      h("div", { class: "flex gap-1" }, [
        h(
          Button,
          {
            variant: "ghost",
            size: "icon",
            class: "h-6 w-6 text-muted-foreground hover:text-foreground",
            title: "编辑",
            onClick: () => openEditDialog(row.original),
          },
          () => "✎",
        ),
        h(
          Button,
          {
            variant: "ghost",
            size: "icon",
            class: "h-6 w-6 text-muted-foreground hover:text-destructive",
            title: "删除",
            onClick: () => handleRemove(row.original),
          },
          () => "×",
        ),
      ]),
  },
];

// ── Data loading ──
async function loadData() {
  loading.value = true;
  emit("loading", true);
  try {
    const list = await getPhraseList();
    list.sort((a, b) => {
      if (a.code < b.code) return -1;
      if (a.code > b.code) return 1;
      return (b.weight ?? 0) - (a.weight ?? 0);
    });
    allPhrases.value = list;
    selectedKeys.value = new Set();
  } catch (e) {
    toast(`加载短语失败: ${e}`, "error");
  } finally {
    loading.value = false;
    emit("loading", false);
  }
}

// ── Dialog ──
function openAddDialog() {
  editingPhrase.value = null;
  formState.value = createEmptyPhraseFormState(1000);
  hasValidationError.value = false;
  composedText.value = "";
  continuousAdd.value = false;
  dialogVisible.value = true;
}

function openEditDialog(item: PhraseItem) {
  editingPhrase.value = item;
  const eff =
    typeof item.effective_weight === "number" && item.effective_weight > 0
      ? item.effective_weight
      : typeof item.weight === "number" && item.weight > 0
        ? item.weight
        : 1000;
  const next = createEmptyPhraseFormState(eff);
  next.code = item.code;
  loadFormFromText(next, item.text || "");
  formState.value = next;
  hasValidationError.value = false;
  composedText.value = item.text || "";
  dialogVisible.value = true;
}

function clampWeight(w: number): number {
  if (!Number.isFinite(w)) return 1000;
  return Math.max(0, Math.min(10000, Math.round(w)));
}

async function handleSave() {
  const code = formState.value.code;
  const weight = formState.value.weight;
  const text = composedText.value;
  if (!code.trim()) {
    toast("编码不能为空", "error");
    return;
  }
  if (hasValidationError.value) {
    toast("内容存在解析错误，请修正后再保存", "error");
    return;
  }
  const w = clampWeight(weight);
  try {
    if (editingPhrase.value) {
      const oldCode = editingPhrase.value.code;
      const oldText = editingPhrase.value.text || "";
      const newCode = code !== oldCode ? code : "";
      const newText = text;
      await updatePhrase(oldCode, oldText, newCode, newText, 0, w, null);
      toast("短语已更新");
    } else {
      // 2026-05-16 schema 简化后, 短语类型完全由 text 内容自描述
      // ($AA/$SS/$CC marker), 后端不需要 type 入参。
      await addPhrase(code, text, 0, w);
      toast("短语已添加");
    }
    // 编辑模式 / 非连续添加: 保存后关闭。
    // 连续添加 (仅添加场景): 清空表单, 保留对话框继续录入。
    if (editingPhrase.value || !continuousAdd.value) {
      dialogVisible.value = false;
    } else {
      formState.value = createEmptyPhraseFormState(1000);
      composedText.value = "";
      hasValidationError.value = false;
    }
    await loadData();
  } catch (e) {
    toast(`操作失败: ${e}`, "error");
  }
}

// ── Toggle enabled ──
async function handleToggleEnabled(item: PhraseItem) {
  try {
    await setPhraseEnabled(item.code, item.text || "", !item.enabled);
    await loadData();
  } catch (e) {
    toast(`操作失败: ${e}`, "error");
  }
}

// ── Delete single ──
async function handleRemove(item: PhraseItem) {
  const content = itemContent(item);
  const ok = await confirm(`确定删除短语「${item.code}」（${content}）吗？`);
  if (!ok) return;
  try {
    await removePhrase(item.code, item.text || "");
    toast("短语已删除");
    await loadData();
  } catch (e) {
    toast(`删除失败: ${e}`, "error");
  }
}

// ── Batch delete ──
async function handleBatchRemove() {
  const count = selectedKeys.value.size;
  if (count === 0) return;
  const ok = await confirm(`确定删除选中的 ${count} 条短语吗？`);
  if (!ok) return;
  const toDelete = allPhrases.value.filter((item) =>
    selectedKeys.value.has(phraseKey(item)),
  );
  try {
    await removePhrases(
      toDelete.map((item) => ({
        code: item.code,
        text: item.text || "",
      })),
    );
    toast(`已删除 ${toDelete.length} 条短语`);
    await loadData();
  } catch (e) {
    toast(`删除失败: ${e}`, "error");
  }
}

// ── Reset default ──
async function handleReset() {
  const ok = await confirm(
    "确定恢复所有短语为系统默认吗？\n自定义短语将会丢失。",
  );
  if (!ok) return;
  try {
    await resetPhrasesToDefault();
    toast("已恢复默认短语");
    await loadData();
  } catch (e) {
    toast(`操作失败: ${e}`, "error");
  }
}

onMounted(() => {
  loadData();
});

onUnmounted(() => {});

defineExpose({ loadData });
</script>

<template>
  <DictDataTable
    :columns="columns"
    :data="allPhrases"
    :loading="loading"
    :row-key="phraseKey"
    search-placeholder="搜索..."
    empty-text="暂无短语"
    search-empty-text="未找到匹配短语"
    :on-row-dblclick="openEditDialog"
    @update:selection="selectedKeys = $event"
  >
    <template #toolbar-start="{ selectedCount }">
      <Button size="sm" @click="openAddDialog">+ 添加</Button>
      <Button
        variant="destructive"
        size="sm"
        :disabled="selectedCount === 0"
        @click="handleBatchRemove"
      >
        删除{{ selectedCount > 0 ? ` (${selectedCount})` : "" }}
      </Button>
      <Button variant="outline" size="sm" @click="handleReset">
        恢复默认
      </Button>
    </template>
  </DictDataTable>

  <!-- 添加/编辑对话框 -->
  <Dialog v-model:open="dialogVisible">
    <DialogContent
      class="max-w-xl max-h-[85vh] flex flex-col p-0 gap-0 overflow-hidden"
    >
      <DialogHeader class="px-6 pt-6 pb-4 border-b shrink-0">
        <DialogTitle>
          {{ editingPhrase ? "编辑短语" : "添加短语" }}
        </DialogTitle>
      </DialogHeader>
      <div class="flex-1 overflow-y-auto px-6 py-4 min-h-0">
        <PhraseFormBody
          v-model="formState"
          :show-code-gen="false"
          code-label="编码"
          code-placeholder="如: zdy"
          @composed-text="composedText = $event"
          @validation-error="hasValidationError = $event"
        />
      </div>
      <DialogFooter
        class="px-6 py-4 border-t shrink-0 bg-background flex items-center"
      >
        <label
          v-if="!editingPhrase"
          class="mr-auto flex items-center gap-2 text-sm text-muted-foreground select-none"
        >
          <Checkbox
            :checked="continuousAdd"
            @update:checked="(v: boolean) => (continuousAdd = v)"
          />
          <span>连续添加</span>
        </label>
        <Button variant="outline" size="sm" @click="dialogVisible = false">
          取消
        </Button>
        <Button size="sm" :disabled="hasValidationError" @click="handleSave">
          保存
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>

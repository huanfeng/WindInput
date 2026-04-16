<script setup lang="ts">
import { h, ref, onMounted } from "vue";
import type { ColumnDef } from "@tanstack/vue-table";
import {
  getPhraseList,
  addPhrase,
  updatePhrase,
  removePhrase,
  setPhraseEnabled,
  resetPhrasesToDefault,
  importPhrases,
  exportPhrases,
  type PhraseItem,
} from "@/api/wails";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import DictDataTable from "./DictDataTable.vue";
import { Checkbox } from "@/components/ui/checkbox";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Input } from "@/components/ui/input";
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
const phraseIsArray = ref(false);
const newPhrase = ref({ code: "", text: "", texts: "", name: "", position: 1 });

function phraseKey(item: PhraseItem): string {
  return `${item.code}||${item.text || ""}||${item.name || ""}`;
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
    size: 100,
    cell: ({ row }) =>
      h(
        "span",
        {
          class:
            "font-mono text-sm text-muted-foreground bg-secondary px-2 py-0.5 rounded",
        },
        row.getValue("code"),
      ),
  },
  {
    id: "content",
    header: "内容",
    cell: ({ row }) =>
      row.original.type === "array"
        ? h("span", {}, row.original.name || row.original.code)
        : h("span", {}, row.original.text),
  },
  {
    id: "type",
    header: "类型",
    size: 90,
    cell: ({ row }) => {
      if (row.original.type === "array")
        return h(
          Badge,
          {
            variant: "secondary",
            class:
              "text-[10px] px-1.5 py-0 whitespace-nowrap bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-400 border-0",
          },
          () => "数组",
        );
      if (row.original.type === "dynamic")
        return h(
          Badge,
          {
            variant: "secondary",
            class:
              "text-[10px] px-1.5 py-0 whitespace-nowrap bg-blue-100 text-blue-800 dark:bg-blue-900/30 dark:text-blue-400 border-0",
          },
          () => "动态",
        );
      if (row.original.is_system)
        return h(
          Badge,
          {
            variant: "secondary",
            class: "text-[10px] px-1.5 py-0 whitespace-nowrap",
          },
          () => "系统",
        );
      return "";
    },
  },
  {
    accessorKey: "position",
    header: "位置",
    size: 60,
  },
  {
    id: "actions",
    size: 80,
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
          () => "\u270e",
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
          () => "\u00d7",
        ),
      ]),
  },
];

// ── Data loading ──
async function loadData() {
  loading.value = true;
  emit("loading", true);
  try {
    allPhrases.value = await getPhraseList();
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
  phraseIsArray.value = false;
  newPhrase.value = { code: "", text: "", texts: "", name: "", position: 1 };
  dialogVisible.value = true;
}

function openEditDialog(item: PhraseItem) {
  editingPhrase.value = item;
  phraseIsArray.value = item.type === "array";
  newPhrase.value = {
    code: item.code,
    text: item.text || "",
    texts: item.texts || "",
    name: item.name || "",
    position: item.position,
  };
  dialogVisible.value = true;
}

async function handleSave() {
  const { code, text, texts, name, position } = newPhrase.value;
  if (!code.trim()) {
    toast("编码不能为空", "error");
    return;
  }
  const type = phraseIsArray.value ? "array" : "static";
  try {
    if (editingPhrase.value) {
      const oldText = editingPhrase.value.text || "";
      const oldName = editingPhrase.value.name || "";
      const newText = phraseIsArray.value ? texts : text;
      await updatePhrase(code, oldText, oldName, newText, position, null);
      toast("短语已更新");
    } else {
      await addPhrase(code, text, texts, name, type, position);
      toast("短语已添加");
    }
    dialogVisible.value = false;
    await loadData();
  } catch (e) {
    toast(`操作失败: ${e}`, "error");
  }
}

// ── Toggle enabled ──
async function handleToggleEnabled(item: PhraseItem) {
  try {
    await setPhraseEnabled(
      item.code,
      item.text || "",
      item.name || "",
      !item.enabled,
    );
    await loadData();
  } catch (e) {
    toast(`操作失败: ${e}`, "error");
  }
}

// ── Delete single ──
async function handleRemove(item: PhraseItem) {
  const ok = await confirm(`确定删除短语「${item.code}」吗？`);
  if (!ok) return;
  try {
    await removePhrase(item.code, item.text || "", item.name || "");
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
    for (const item of toDelete) {
      await removePhrase(item.code, item.text || "", item.name || "");
    }
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
    <DialogContent class="sm:max-w-[450px]">
      <DialogHeader>
        <DialogTitle>
          {{ editingPhrase ? "编辑短语" : "添加短语" }}
        </DialogTitle>
      </DialogHeader>
      <div class="grid gap-4 py-4">
        <div class="grid grid-cols-[80px_1fr] items-center gap-2">
          <label class="text-sm font-medium text-right">编码</label>
          <Input
            v-model="newPhrase.code"
            :disabled="!!editingPhrase"
            placeholder="如: zdy"
          />
        </div>
        <div class="grid grid-cols-[80px_1fr] items-center gap-2">
          <label class="text-sm font-medium text-right">类型</label>
          <div class="flex gap-4">
            <label class="flex items-center gap-1.5 text-sm cursor-pointer">
              <input
                type="radio"
                :checked="!phraseIsArray"
                @change="phraseIsArray = false"
              />
              普通
            </label>
            <label class="flex items-center gap-1.5 text-sm cursor-pointer">
              <input
                type="radio"
                :checked="phraseIsArray"
                @change="phraseIsArray = true"
              />
              数组
            </label>
          </div>
        </div>
        <template v-if="phraseIsArray">
          <div class="grid grid-cols-[80px_1fr] items-center gap-2">
            <label class="text-sm font-medium text-right">名称</label>
            <Input v-model="newPhrase.name" placeholder="如: 特殊符号" />
          </div>
          <div class="grid grid-cols-[80px_1fr] items-start gap-2">
            <label class="text-sm font-medium text-right pt-2">字符列表</label>
            <textarea
              v-model="newPhrase.texts"
              rows="4"
              placeholder="每行一个字符或词"
              class="flex w-full rounded-md border border-input bg-transparent px-3 py-2 text-sm shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring resize-y"
            />
          </div>
        </template>
        <template v-else>
          <div class="grid grid-cols-[80px_1fr] items-start gap-2">
            <label class="text-sm font-medium text-right pt-2">文本</label>
            <textarea
              v-model="newPhrase.text"
              rows="3"
              placeholder="如: 我的地址是xxx 或 $Y-$MM-$DD&#10;支持多行文本"
              class="flex w-full rounded-md border border-input bg-transparent px-3 py-2 text-sm shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring resize-y"
            />
          </div>
        </template>
        <div class="grid grid-cols-[80px_1fr] items-center gap-2">
          <label class="text-sm font-medium text-right">位置</label>
          <Input
            v-model.number="newPhrase.position"
            type="number"
            min="1"
            class="w-20"
          />
        </div>
      </div>
      <DialogFooter>
        <Button variant="outline" @click="dialogVisible = false">取消</Button>
        <Button @click="handleSave">保存</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>

</template>

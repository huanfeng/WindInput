<script setup lang="ts">
import { h, ref, onMounted } from "vue";
import type { ColumnDef } from "@tanstack/vue-table";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import {
  getShadowBySchema,
  pinShadowWordForSchema,
  deleteShadowWordForSchema,
  removeShadowRuleForSchema,
  type ShadowRuleItem,
} from "@/api/wails";
import DictDataTable from "./DictDataTable.vue";
import { Checkbox } from "@/components/ui/checkbox";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from "@/components/ui/select";
const props = defineProps<{
  schemaId: string;
  readonly?: boolean;
}>();

const emit = defineEmits<{
  (e: "loading", value: boolean): void;
  (e: "schema-changed"): void;
}>();

defineExpose({ loadData });

const { toast } = useToast();
const { confirm } = useConfirm();

const shadowRules = ref<ShadowRuleItem[]>([]);
const selectedKeys = ref<Set<string>>(new Set());
const loading = ref(false);

// Dialog state
const dialogVisible = ref(false);
const dialogEditing = ref(false);
const form = ref({
  code: "",
  word: "",
  action: "pin" as "pin" | "delete",
  position: 0,
});

function itemKey(item: ShadowRuleItem) {
  return `${item.code}|${item.word}`;
}

const columns: ColumnDef<ShadowRuleItem, any>[] = [
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
    accessorKey: "word",
    header: "词条",
  },
  {
    id: "type",
    header: "操作类型",
    size: 100,
    cell: ({ row }) =>
      row.original.type === "pin"
        ? h(
            Badge,
            {
              class:
                "bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200",
            },
            () => "固定位置",
          )
        : h(Badge, { variant: "destructive" }, () => "隐藏"),
  },
  {
    id: "position",
    header: "位置",
    size: 60,
    cell: ({ row }) =>
      row.original.type === "pin" ? String(row.original.position ?? "") : "",
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
            onClick: () => openDialog(row.original),
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

function openDialog(item?: ShadowRuleItem) {
  if (item) {
    dialogEditing.value = true;
    form.value = {
      code: item.code,
      word: item.word,
      action: item.type === "pin" ? "pin" : "delete",
      position: item.position ?? 0,
    };
  } else {
    dialogEditing.value = false;
    form.value = { code: "", word: "", action: "pin", position: 0 };
  }
  dialogVisible.value = true;
}

async function handleSave() {
  if (!form.value.code.trim() || !form.value.word.trim()) {
    toast("编码和词条不能为空", "error");
    return;
  }
  try {
    if (dialogEditing.value) {
      await removeShadowRuleForSchema(
        props.schemaId,
        form.value.code,
        form.value.word,
      );
    }
    if (form.value.action === "pin") {
      await pinShadowWordForSchema(
        props.schemaId,
        form.value.code,
        form.value.word,
        form.value.position,
      );
    } else {
      await deleteShadowWordForSchema(
        props.schemaId,
        form.value.code,
        form.value.word,
      );
    }
    toast(dialogEditing.value ? "规则已保存" : "规则已添加");
    dialogVisible.value = false;
    await loadData();
    emit("schema-changed");
  } catch (e) {
    toast(`保存失败: ${e}`, "error");
  }
}

async function loadData() {
  loading.value = true;
  emit("loading", true);
  try {
    shadowRules.value = (await getShadowBySchema(props.schemaId)) || [];
    selectedKeys.value = new Set();
  } finally {
    loading.value = false;
    emit("loading", false);
  }
}

async function handleRemove(item: ShadowRuleItem) {
  const ok = await confirm(`确定删除「${item.word}」的调整规则？`);
  if (!ok) return;
  try {
    await removeShadowRuleForSchema(props.schemaId, item.code, item.word);
    toast(`已删除「${item.word}」的规则`);
    await loadData();
    emit("schema-changed");
  } catch (e) {
    toast(`删除失败: ${e}`, "error");
  }
}

async function handleBatchRemove() {
  // 首个 await 前同步快照：Wails 事件可能在 await 间触发 loadData() 清空 selectedKeys
  const itemsToDelete = shadowRules.value.filter((item) =>
    selectedKeys.value.has(itemKey(item)),
  );
  if (itemsToDelete.length === 0) return;
  const ok = await confirm(`确定删除选中的 ${itemsToDelete.length} 条调整规则？`);
  if (!ok) return;
  try {
    for (const item of itemsToDelete) {
      await removeShadowRuleForSchema(props.schemaId, item.code, item.word);
    }
    toast(`已删除 ${itemsToDelete.length} 条规则`);
    await loadData();
    emit("schema-changed");
  } catch (e) {
    toast(`删除失败: ${e}`, "error");
  }
}

async function handleClearAll() {
  if (shadowRules.value.length === 0) return;
  const ok = await confirm(
    "确定清空当前方案的所有候选调整规则吗？此操作不可撤销。",
  );
  if (!ok) return;
  const allItems = [...shadowRules.value];
  try {
    for (const item of allItems) {
      await removeShadowRuleForSchema(props.schemaId, item.code, item.word);
    }
    toast(`已清空 ${allItems.length} 条规则`, "success");
    await loadData();
    emit("schema-changed");
  } catch (e) {
    toast(`清空失败: ${e}`, "error");
  }
}

onMounted(() => {
  loadData();
});
</script>

<template>
  <DictDataTable
    :columns="columns"
    :data="shadowRules"
    :loading="loading"
    :row-key="(row: ShadowRuleItem) => `${row.code}|${row.word}`"
    search-placeholder="搜索..."
    empty-text="暂无调整规则"
    search-empty-text="未找到匹配规则"
    :on-row-dblclick="(item: ShadowRuleItem) => openDialog(item)"
    @update:selection="selectedKeys = $event"
  >
    <template #toolbar-start="{ selectedCount }">
      <Button size="sm" :disabled="readonly" @click="openDialog()">
        + 添加
      </Button>
      <Button
        variant="destructive"
        size="sm"
        :disabled="selectedCount === 0"
        @click="handleBatchRemove"
      >
        删除{{ selectedCount > 0 ? ` (${selectedCount})` : "" }}
      </Button>
      <Button
        variant="destructive"
        size="sm"
        :disabled="shadowRules.length === 0"
        @click="handleClearAll"
      >
        清空
      </Button>
    </template>
  </DictDataTable>

  <!-- 添加/编辑对话框 -->
  <Dialog v-model:open="dialogVisible">
    <DialogContent class="sm:max-w-[400px]">
      <DialogHeader>
        <DialogTitle>{{ dialogEditing ? "编辑规则" : "添加规则" }}</DialogTitle>
      </DialogHeader>
      <div class="grid gap-4 py-4">
        <div class="grid grid-cols-[80px_1fr] items-center gap-2">
          <label class="text-sm font-medium text-right">编码</label>
          <Input
            v-model="form.code"
            placeholder="如: sf"
            :disabled="dialogEditing"
          />
        </div>
        <div class="grid grid-cols-[80px_1fr] items-center gap-2">
          <label class="text-sm font-medium text-right">词条</label>
          <Input
            v-model="form.word"
            placeholder="如: 村"
            :disabled="dialogEditing"
          />
        </div>
        <div class="grid grid-cols-[80px_1fr] items-center gap-2">
          <label class="text-sm font-medium text-right">操作</label>
          <Select v-model="form.action">
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="pin">固定位置</SelectItem>
              <SelectItem value="delete">隐藏词条</SelectItem>
            </SelectContent>
          </Select>
        </div>
        <div
          v-if="form.action === 'pin'"
          class="grid grid-cols-[80px_1fr] items-center gap-2"
        >
          <label class="text-sm font-medium text-right">目标位置</label>
          <Input
            v-model.number="form.position"
            type="number"
            min="0"
            placeholder="0=首位"
          />
        </div>
      </div>
      <DialogFooter>
        <Button variant="outline" @click="dialogVisible = false">取消</Button>
        <Button @click="handleSave">
          {{ dialogEditing ? "保存" : "添加" }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>

</template>

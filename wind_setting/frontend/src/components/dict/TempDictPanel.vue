<script setup lang="ts">
import { h, ref, onMounted } from "vue";
import type { ColumnDef } from "@tanstack/vue-table";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import {
  getTempDictBySchema,
  removeTempWordForSchema,
  promoteTempWordForSchema,
  promoteAllTempWordsForSchema,
  clearTempDictForSchema,
  type TempWordItem,
} from "@/api/wails";
import DictDataTable from "./DictDataTable.vue";
import { Checkbox } from "@/components/ui/checkbox";
import { Button } from "@/components/ui/button";
const props = defineProps<{
  schemaId: string;
}>();

const emit = defineEmits<{
  (e: "loading", value: boolean): void;
  (e: "schema-changed"): void;
}>();

defineExpose({ loadData });

const { toast } = useToast();
const { confirm } = useConfirm();

const tempDict = ref<TempWordItem[]>([]);
const selectedKeys = ref<Set<string>>(new Set());
const loading = ref(false);

function itemKey(item: TempWordItem) {
  return `${item.code}|${item.text}`;
}

const columns: ColumnDef<TempWordItem, any>[] = [
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
    accessorKey: "text",
    header: "词条",
  },
  {
    accessorKey: "weight",
    header: "权重",
    size: 60,
  },
  {
    accessorKey: "count",
    header: "次数",
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
            class: "h-6 w-6 text-muted-foreground hover:text-primary",
            title: "转正",
            onClick: () => handlePromote(row.original),
          },
          () => "\u2191",
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

async function loadData() {
  loading.value = true;
  emit("loading", true);
  try {
    tempDict.value = await getTempDictBySchema(props.schemaId);
    selectedKeys.value = new Set();
  } finally {
    loading.value = false;
    emit("loading", false);
  }
}

async function handlePromote(item: TempWordItem) {
  try {
    await promoteTempWordForSchema(props.schemaId, item.code, item.text);
    toast(`已将「${item.text}」转正`);
    await loadData();
    emit("schema-changed");
  } catch (e) {
    toast(`转正失败: ${e}`, "error");
  }
}

async function handlePromoteAll() {
  const ok = await confirm(
    `确定将全部 ${tempDict.value.length} 条临时词条转正？`,
  );
  if (!ok) return;
  try {
    const count = await promoteAllTempWordsForSchema(props.schemaId);
    toast(`已将 ${count} 条词条转正`);
    await loadData();
    emit("schema-changed");
  } catch (e) {
    toast(`转正失败: ${e}`, "error");
  }
}

async function handleClear() {
  const ok = await confirm("确定清空当前方案的所有临时词库？此操作不可撤销。");
  if (!ok) return;
  try {
    const count = await clearTempDictForSchema(props.schemaId);
    toast(`已清空 ${count} 条临时词条`);
    await loadData();
    emit("schema-changed");
  } catch (e) {
    toast(`清空失败: ${e}`, "error");
  }
}

async function handleRemove(item: TempWordItem) {
  const ok = await confirm(`确定删除临时词条「${item.text}」？`);
  if (!ok) return;
  try {
    await removeTempWordForSchema(props.schemaId, item.code, item.text);
    toast(`已删除「${item.text}」`);
    await loadData();
    emit("schema-changed");
  } catch (e) {
    toast(`删除失败: ${e}`, "error");
  }
}

async function handleBatchRemove() {
  // 首个 await 前同步快照：Wails 事件可能在 await 间触发 loadData() 清空 selectedKeys
  const itemsToDelete = tempDict.value.filter((item) =>
    selectedKeys.value.has(itemKey(item)),
  );
  if (itemsToDelete.length === 0) return;
  const ok = await confirm(`确定删除选中的 ${itemsToDelete.length} 条临时词条？`);
  if (!ok) return;
  try {
    for (const item of itemsToDelete) {
      await removeTempWordForSchema(props.schemaId, item.code, item.text);
    }
    toast(`已删除 ${itemsToDelete.length} 条词条`);
    await loadData();
    emit("schema-changed");
  } catch (e) {
    toast(`删除失败: ${e}`, "error");
  }
}

onMounted(() => {
  loadData();
});
</script>

<template>
  <DictDataTable
    :columns="columns"
    :data="tempDict"
    :loading="loading"
    :page-size="100"
    :row-key="(row: TempWordItem) => `${row.code}|${row.text}`"
    search-placeholder="搜索..."
    empty-text="暂无临时词条"
    search-empty-text="未找到匹配词条"
    @update:selection="selectedKeys = $event"
  >
    <template #toolbar-start="{ selectedCount }">
      <Button
        size="sm"
        :disabled="tempDict.length === 0"
        @click="handlePromoteAll"
      >
        全部转正
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
        :disabled="tempDict.length === 0"
        @click="handleClear"
      >
        清空
      </Button>
    </template>
  </DictDataTable>

</template>

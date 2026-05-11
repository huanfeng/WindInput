<script setup lang="ts">
import { h, ref, watch, onMounted } from "vue";
import type { ColumnDef } from "@tanstack/vue-table";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import { getFreqList, deleteFreq, clearFreq } from "@/api/wails";
import type { FreqItem } from "@/api/wails";
import DictDataTable from "./DictDataTable.vue";
import { Checkbox } from "@/components/ui/checkbox";
import { Button } from "@/components/ui/button";
const props = defineProps<{
  schemaId: string;
  schemaName: string;
}>();

const emit = defineEmits<{
  (e: "loading", val: boolean): void;
}>();

defineExpose({ loadData });

const { toast } = useToast();
const { confirm } = useConfirm();

const tableRef = ref<{
  globalFilter: string;
  clearSelection: () => void;
  selectedCount: number;
} | null>(null);
const freqList = ref<FreqItem[]>([]);
const total = ref(0);
const page = ref(0);
const pageSize = 100;
const selectedKeys = ref<Set<string>>(new Set());
const loading = ref(false);

// Debounced server-side search
let searchTimer: ReturnType<typeof setTimeout> | null = null;

watch(
  () => tableRef.value?.globalFilter,
  () => {
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      page.value = 0;
      loadData();
    }, 300);
  },
);

function itemKey(item: FreqItem) {
  return `${item.code}|${item.text}`;
}

function formatLastUsed(ts: number): string {
  if (!ts) return "-";
  const d = new Date(ts * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

const columns: ColumnDef<FreqItem, any>[] = [
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
    accessorKey: "count",
    header: "次数",
    size: 60,
  },
  {
    accessorKey: "boost",
    header: "提升",
    size: 60,
  },
  {
    accessorKey: "last_used",
    header: "最后使用",
    size: 140,
    cell: ({ row }) => formatLastUsed(row.getValue("last_used")),
  },
  {
    id: "actions",
    size: 50,
    enableSorting: false,
    cell: ({ row }) =>
      h(
        Button,
        {
          variant: "ghost",
          size: "icon",
          class: "h-6 w-6 text-muted-foreground hover:text-destructive",
          onClick: () => handleDelete(row.original),
        },
        () => "\u00d7",
      ),
  },
];

async function loadData() {
  loading.value = true;
  emit("loading", true);
  try {
    const query = tableRef.value?.globalFilter ?? "";
    const result = await getFreqList(
      props.schemaId,
      query.trim(),
      pageSize,
      page.value * pageSize,
    );
    freqList.value = result.entries;
    total.value = result.total;
    selectedKeys.value = new Set();
  } catch {
    toast("加载词频失败", "error");
  } finally {
    loading.value = false;
    emit("loading", false);
  }
}

async function handleDelete(item: FreqItem) {
  const ok = await confirm(`确定删除词频记录「${item.text}」？`);
  if (!ok) return;
  try {
    await deleteFreq(props.schemaId, item.code, item.text);
    toast("已删除", "success");
    await loadData();
  } catch {
    toast("删除失败", "error");
  }
}

async function handleBatchDelete() {
  if (selectedKeys.value.size === 0) return;
  // 首个 await 前同步快照：Wails 事件可能在 await 间触发 loadData() 清空 selectedKeys
  const itemsToDelete = freqList.value.filter((item) =>
    selectedKeys.value.has(itemKey(item)),
  );
  if (itemsToDelete.length === 0) return;
  const ok = await confirm(
    `确定删除选中的 ${itemsToDelete.length} 条词频记录？`,
  );
  if (!ok) return;
  let failed = 0;
  for (const item of itemsToDelete) {
    try {
      await deleteFreq(props.schemaId, item.code, item.text);
    } catch {
      failed++;
    }
  }
  if (failed > 0) {
    toast(`删除完成，${failed} 个失败`, "error");
  } else {
    toast("已删除选中词频记录", "success");
  }
  await loadData();
}

async function handleClear() {
  const ok = await confirm(
    `确定清空方案「${props.schemaName || props.schemaId}」的所有词频记录？`,
  );
  if (!ok) return;
  try {
    const count = await clearFreq(props.schemaId);
    toast(`已清空 ${count} 条词频记录`, "success");
    page.value = 0;
    await loadData();
  } catch {
    toast("清空失败", "error");
  }
}

function onPageChange(p: number) {
  page.value = p;
  loadData();
}

onMounted(() => {
  loadData();
});
</script>

<template>
  <DictDataTable
    ref="tableRef"
    :columns="columns"
    :data="freqList"
    :loading="loading"
    :row-key="(row: FreqItem) => `${row.code}|${row.text}`"
    :server-pagination="{ total, pageSize, page }"
    search-placeholder="搜索..."
    empty-text="暂无词频记录"
    search-empty-text="未找到匹配词频记录"
    @update:selection="selectedKeys = $event"
    @page-change="onPageChange"
  >
    <template #toolbar-start="{ selectedCount }">
      <Button
        variant="destructive"
        size="sm"
        :disabled="selectedCount === 0"
        @click="handleBatchDelete"
      >
        删除{{ selectedCount > 0 ? ` (${selectedCount})` : "" }}
      </Button>
      <Button
        variant="destructive"
        size="sm"
        :disabled="total === 0"
        @click="handleClear"
      >
        清空
      </Button>
    </template>
  </DictDataTable>

</template>

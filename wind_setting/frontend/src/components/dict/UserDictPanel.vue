<script setup lang="ts">
import { h, ref, onMounted, watch } from "vue";
import type { ColumnDef } from "@tanstack/vue-table";
import { useToast } from "@/composables/useToast";
import { useConfirm } from "@/composables/useConfirm";
import AddWordPage from "@/pages/AddWordPage.vue";
import {
  getUserDictBySchemaPage,
  removeUserWordForSchema,
  clearUserDictForSchema,
  type UserWordItem,
  type PagedDictResult,
} from "@/api/wails";
import DictDataTable from "./DictDataTable.vue";
import { Checkbox } from "@/components/ui/checkbox";
import { Button } from "@/components/ui/button";

const PAGE_SIZE = 50;

const props = defineProps<{
  schemaId: string;
  readonly?: boolean;
}>();

const emit = defineEmits<{
  (e: "loading", val: boolean): void;
  (e: "schema-changed"): void;
}>();

defineExpose({ loadData });

const { toast } = useToast();
const { confirm } = useConfirm();

const userDict = ref<UserWordItem[]>([]);
const selectedKeys = ref<Set<string>>(new Set());
const loading = ref(false);
const addWordVisible = ref(false);
const editText = ref("");
const editCode = ref("");
const currentPage = ref(0);
const totalCount = ref(0);
const searchQuery = ref("");

function itemKey(item: UserWordItem) {
  return `${item.code}|${item.text}`;
}

const columns: ColumnDef<UserWordItem, any>[] = [
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
    enableSorting: false,
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
    enableSorting: false,
  },
  {
    accessorKey: "weight",
    header: "权重",
    size: 60,
    enableSorting: false,
    cell: ({ row }) => String(row.getValue("weight") ?? 0),
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
            onClick: () => handleDelete(row.original),
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
    const result: PagedDictResult = await getUserDictBySchemaPage(
      props.schemaId,
      searchQuery.value,
      searchQuery.value,
      PAGE_SIZE,
      currentPage.value * PAGE_SIZE,
    );
    userDict.value = result.words ?? [];
    totalCount.value = result.total;
    selectedKeys.value = new Set();
  } catch {
    toast("加载用户词库失败", "error");
  } finally {
    loading.value = false;
    emit("loading", false);
  }
}

function handlePageChange(page: number) {
  currentPage.value = page;
  loadData();
}

// DictDataTable 的搜索框由我们接管，通过 prefix 服务端搜索
let searchTimer: ReturnType<typeof setTimeout> | null = null;
function handleSearchInput(val: string) {
  searchQuery.value = val;
  currentPage.value = 0;
  if (searchTimer) clearTimeout(searchTimer);
  searchTimer = setTimeout(() => loadData(), 300);
}

function openAddDialog() {
  editText.value = "";
  editCode.value = "";
  addWordVisible.value = true;
}

function openEditDialog(item: UserWordItem) {
  editText.value = item.text;
  editCode.value = item.code;
  addWordVisible.value = true;
}

async function handleAddWordClose() {
  addWordVisible.value = false;
  await loadData();
  emit("schema-changed");
}

async function handleDelete(item: UserWordItem) {
  const ok = await confirm(`确定删除词条「${item.text}」？`);
  if (!ok) return;
  try {
    await removeUserWordForSchema(props.schemaId, item.code, item.text);
    toast("已删除", "success");
    await loadData();
    emit("schema-changed");
  } catch {
    toast("删除失败", "error");
  }
}

async function handleBatchDelete() {
  if (selectedKeys.value.size === 0) return;
  const ok = await confirm(
    `确定删除选中的 ${selectedKeys.value.size} 个词条？`,
  );
  if (!ok) return;
  let failed = 0;
  for (const item of userDict.value) {
    if (selectedKeys.value.has(itemKey(item))) {
      try {
        await removeUserWordForSchema(props.schemaId, item.code, item.text);
      } catch {
        failed++;
      }
    }
  }
  if (failed > 0) {
    toast(`删除完成，${failed} 个失败`, "error");
  } else {
    toast("已删除选中词条", "success");
  }
  await loadData();
  emit("schema-changed");
}

async function handleClearAll() {
  if (totalCount.value === 0) return;
  const ok = await confirm(
    `确定清空当前方案的全部 ${totalCount.value} 个用户词条？\n\n此操作不可恢复！`,
  );
  if (!ok) return;
  try {
    const count = await clearUserDictForSchema(props.schemaId);
    toast(`已清空 ${count} 个词条`, "success");
    await loadData();
    emit("schema-changed");
  } catch {
    toast("清空失败", "error");
  }
}

onMounted(() => {
  loadData();
});
</script>

<template>
  <DictDataTable
    :columns="columns"
    :data="userDict"
    :loading="loading"
    :row-key="(row: UserWordItem) => `${row.code}|${row.text}`"
    :searchable="true"
    search-placeholder="搜索..."
    empty-text="暂无用户词条"
    search-empty-text="未找到匹配词条"
    :server-pagination="{ total: totalCount, pageSize: PAGE_SIZE, page: currentPage }"
    @update:selection="selectedKeys = $event"
    @page-change="handlePageChange"
    @search="handleSearchInput"
  >
    <template #toolbar-start="{ selectedCount }">
      <Button size="sm" :disabled="readonly" @click="openAddDialog">
        + 添加
      </Button>
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
        :disabled="readonly || totalCount === 0"
        @click="handleClearAll"
      >
        清空
      </Button>
    </template>
  </DictDataTable>

  <!-- AddWordPage 对话框 -->
  <AddWordPage
    v-if="addWordVisible"
    :initialText="editText"
    :initialCode="editCode"
    :initialSchema="schemaId"
    @close="handleAddWordClose"
  />

</template>

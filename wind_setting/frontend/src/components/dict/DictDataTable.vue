<script setup lang="ts" generic="TData extends Record<string, any>">
import { ref, computed, watch } from "vue";
import {
  useVueTable,
  getCoreRowModel,
  getFilteredRowModel,
  getPaginationRowModel,
  getSortedRowModel,
  FlexRender,
  type ColumnDef,
  type SortingState,
  type RowSelectionState,
} from "@tanstack/vue-table";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

interface Props {
  columns: ColumnDef<TData, any>[];
  data: TData[];
  loading?: boolean;
  searchable?: boolean;
  searchPlaceholder?: string;
  selectable?: boolean;
  serverPagination?: {
    total: number;
    pageSize: number;
    page: number;
  };
  pageSize?: number;
  rowKey: (row: TData) => string;
  emptyText?: string;
  searchEmptyText?: string;
  onRowDblclick?: (row: TData) => void;
  onRowContextmenu?: (row: TData, event: MouseEvent) => void;
}

const props = withDefaults(defineProps<Props>(), {
  loading: false,
  searchable: true,
  searchPlaceholder: "搜索...",
  selectable: true,
  pageSize: 0,
  emptyText: "暂无数据",
  searchEmptyText: "未找到匹配数据",
});

const emit = defineEmits<{
  "update:selection": [keys: Set<string>];
  "page-change": [page: number];
  "search": [query: string];
}>();

const globalFilter = ref("");
const searchInput = ref("");

// 服务端分页模式下，搜索交给父组件处理
function handleSearchInput(val: string) {
  searchInput.value = val;
  if (props.serverPagination) {
    emit("search", val);
  } else {
    globalFilter.value = val;
  }
}
const sorting = ref<SortingState>([]);
const rowSelection = ref<RowSelectionState>({});

const table = useVueTable({
  get data() {
    return props.data;
  },
  get columns() {
    return props.columns;
  },
  state: {
    get globalFilter() {
      return globalFilter.value;
    },
    get sorting() {
      return sorting.value;
    },
    get rowSelection() {
      return rowSelection.value;
    },
  },
  onSortingChange: (updater) => {
    sorting.value =
      typeof updater === "function" ? updater(sorting.value) : updater;
  },
  onRowSelectionChange: (updater) => {
    rowSelection.value =
      typeof updater === "function" ? updater(rowSelection.value) : updater;
  },
  onGlobalFilterChange: (updater) => {
    globalFilter.value =
      typeof updater === "function" ? updater(globalFilter.value) : updater;
  },
  getCoreRowModel: getCoreRowModel(),
  getFilteredRowModel: getFilteredRowModel(),
  getSortedRowModel: getSortedRowModel(),
  getPaginationRowModel:
    props.pageSize > 0 ? getPaginationRowModel() : undefined,
  getRowId: (row) => props.rowKey(row as TData),
  enableRowSelection: props.selectable,
});

// Sync selection to parent
watch(
  rowSelection,
  () => {
    const keys = new Set(
      Object.keys(rowSelection.value).filter((k) => rowSelection.value[k]),
    );
    emit("update:selection", keys);
  },
  { deep: true },
);

const selectedCount = computed(
  () => Object.values(rowSelection.value).filter(Boolean).length,
);

function clearSelection() {
  rowSelection.value = {};
}

// Reset selection when data changes
watch(
  () => props.data,
  () => {
    rowSelection.value = {};
  },
);

const serverTotalPages = computed(() => {
  if (!props.serverPagination) return 0;
  return Math.ceil(props.serverPagination.total / props.serverPagination.pageSize);
});

function jumpToPage(val: string) {
  const page = parseInt(val, 10);
  if (isNaN(page) || !props.serverPagination) return;
  const target = Math.max(0, Math.min(page - 1, serverTotalPages.value - 1));
  if (target !== props.serverPagination.page) {
    emit("page-change", target);
  }
}

defineExpose({ table, globalFilter, clearSelection, selectedCount });
</script>

<template>
  <div class="flex flex-col flex-1 min-h-0 overflow-hidden">
    <!-- Toolbar -->
    <div class="flex items-center gap-2 pt-1 mb-2 shrink-0 flex-nowrap">
      <slot
        name="toolbar-start"
        :selected-count="selectedCount"
        :clear-selection="clearSelection"
      />

      <div class="flex-1 min-w-1" />

      <Input
        v-if="searchable"
        :model-value="searchInput"
        @update:model-value="handleSearchInput($event as string)"
        type="text"
        :placeholder="searchPlaceholder"
        class="w-[100px] min-w-[60px] shrink h-[var(--control-h-sm)]"
      />

      <span class="text-xs text-muted-foreground shrink-0 whitespace-nowrap">
        共 {{ serverPagination?.total ?? data.length }} 条
      </span>

      <slot name="toolbar-end" />
    </div>

    <!-- Table container -->
    <div
      class="relative flex flex-col flex-1 min-h-0 overflow-hidden border rounded-lg border-border"
    >
      <!-- Loading overlay -->
      <div
        v-if="loading"
        class="absolute inset-0 z-10 flex items-center justify-center rounded-lg bg-card/70"
      >
        <div
          class="h-8 w-8 rounded-full border-3 border-border border-t-primary animate-spin"
        />
      </div>

      <div class="overflow-y-auto flex-1 min-h-0">
        <Table>
          <TableHeader class="sticky top-0 z-[1] bg-secondary">
            <TableRow
              v-for="headerGroup in table.getHeaderGroups()"
              :key="headerGroup.id"
            >
              <TableHead
                v-for="header in headerGroup.headers"
                :key="header.id"
                :class="[
                  header.column.getCanSort()
                    ? 'cursor-pointer select-none hover:text-foreground'
                    : '',
                ]"
                :style="{
                  width:
                    header.getSize() !== 150
                      ? `${header.getSize()}px`
                      : undefined,
                }"
                @click="header.column.getToggleSortingHandler()?.($event)"
              >
                <FlexRender
                  v-if="!header.isPlaceholder"
                  :render="header.column.columnDef.header"
                  :props="header.getContext()"
                />
                <span
                  v-if="header.column.getIsSorted() === 'asc'"
                  class="ml-1 text-xs"
                  >▲</span
                >
                <span
                  v-else-if="header.column.getIsSorted() === 'desc'"
                  class="ml-1 text-xs"
                  >▼</span
                >
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <template v-if="table.getRowModel().rows.length > 0">
              <TableRow
                v-for="row in table.getRowModel().rows"
                :key="row.id"
                :class="{ 'bg-primary/5': row.getIsSelected() }"
                @dblclick="props.onRowDblclick?.(row.original as TData)"
                @contextmenu.prevent="props.onRowContextmenu?.(row.original as TData, $event)"
              >
                <TableCell v-for="cell in row.getVisibleCells()" :key="cell.id">
                  <FlexRender
                    :render="cell.column.columnDef.cell"
                    :props="cell.getContext()"
                  />
                </TableCell>
              </TableRow>
            </template>
            <template v-else>
              <TableRow>
                <TableCell
                  :colspan="columns.length"
                  class="h-24 text-center text-muted-foreground"
                >
                  {{ searchInput ? searchEmptyText : emptyText }}
                </TableCell>
              </TableRow>
            </template>
          </TableBody>
        </Table>
      </div>
    </div>

    <!-- Server-side pagination -->
    <div
      v-if="
        serverPagination && serverPagination.total > serverPagination.pageSize
      "
      class="flex items-center justify-center gap-2 pt-2 shrink-0"
    >
      <Button
        variant="outline"
        size="sm"
        :disabled="serverPagination.page === 0"
        @click="emit('page-change', serverPagination.page - 1)"
      >
        上一页
      </Button>
      <span class="text-xs text-muted-foreground whitespace-nowrap">
        {{ serverPagination.page * serverPagination.pageSize + 1 }}-{{
          Math.min(
            (serverPagination.page + 1) * serverPagination.pageSize,
            serverPagination.total,
          )
        }}
        / {{ serverPagination.total }}
      </span>
      <Input
        type="number"
        :model-value="serverPagination.page + 1"
        :min="1"
        :max="serverTotalPages"
        class="w-[52px] h-[var(--control-h-sm)] text-center text-xs"
        @keydown.enter="jumpToPage(($event.target as HTMLInputElement).value)"
        @blur="jumpToPage(($event.target as HTMLInputElement).value)"
      />
      <span class="text-xs text-muted-foreground">/ {{ serverTotalPages }}</span>
      <Button
        variant="outline"
        size="sm"
        :disabled="
          (serverPagination.page + 1) * serverPagination.pageSize >=
          serverPagination.total
        "
        @click="emit('page-change', serverPagination.page + 1)"
      >
        下一页
      </Button>
    </div>

    <!-- Client-side pagination -->
    <div
      v-else-if="pageSize > 0 && data.length > pageSize"
      class="flex items-center justify-center gap-3 pt-2 shrink-0"
    >
      <Button
        variant="outline"
        size="sm"
        :disabled="!table.getCanPreviousPage()"
        @click="table.previousPage()"
      >
        上一页
      </Button>
      <span class="text-xs text-muted-foreground">
        第 {{ table.getState().pagination.pageIndex + 1 }} /
        {{ table.getPageCount() }} 页
      </span>
      <Button
        variant="outline"
        size="sm"
        :disabled="!table.getCanNextPage()"
        @click="table.nextPage()"
      >
        下一页
      </Button>
    </div>
  </div>
</template>

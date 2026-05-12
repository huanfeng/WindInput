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
import { ChevronDown } from "lucide-vue-next";
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

const clientCurrentPage = computed(() => table.getState().pagination.pageIndex);
const clientTotalPages = computed(() => table.getPageCount());

function jumpClientPage(val: string) {
  const p = parseInt(val, 10);
  if (isNaN(p)) return;
  table.setPageIndex(Math.max(0, Math.min(p - 1, clientTotalPages.value - 1)));
}

// cellSizeStyle 根据 column.size 生成稳定列宽样式：
// 同时设置 width 与 max-width，让列内容（如过长编码）不再撑开列、挤压相邻列；
// 单元格高度允许随内容换行而增长（cell 渲染时用 break-all 等让长串可换行）。
// size === 150 是 tanstack-vue-table 的默认值，视为"未指定"，不强制限宽。
function cellSizeStyle(size: number): Record<string, string> | undefined {
  if (size === 150) return undefined;
  const w = `${size}px`;
  return { width: w, maxWidth: w };
}

function rowGlobalIndex(index: number): number {
  if (props.serverPagination) {
    return props.serverPagination.page * props.serverPagination.pageSize + index + 1;
  }
  if (props.pageSize > 0) {
    return table.getState().pagination.pageIndex * props.pageSize + index + 1;
  }
  return index + 1;
}

defineExpose({ table, globalFilter, clearSelection, selectedCount });
</script>

<template>
  <div class="flex flex-col flex-1 min-h-0 overflow-hidden pb-1">
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
                :style="cellSizeStyle(header.getSize())"
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
                v-for="(row, rowIndex) in table.getRowModel().rows"
                :key="row.id"
                :class="{ 'bg-primary/5': row.getIsSelected() }"
                :title="`序号 ${rowGlobalIndex(rowIndex)}`"
                @dblclick="props.onRowDblclick?.(row.original as TData)"
                @contextmenu.prevent="props.onRowContextmenu?.(row.original as TData, $event)"
              >
                <TableCell
                  v-for="cell in row.getVisibleCells()"
                  :key="cell.id"
                  :style="cellSizeStyle(cell.column.getSize())"
                  class="align-top"
                >
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
      v-if="serverPagination && serverPagination.total > serverPagination.pageSize"
      class="flex items-center justify-center gap-1 py-1.5 shrink-0"
    >
      <Button variant="outline" size="icon" class="h-7 w-7 text-xs"
        :disabled="serverPagination.page === 0"
        @click="emit('page-change', 0)" title="第一页">«</Button>
      <Button variant="outline" size="icon" class="h-7 w-7 text-xs"
        :disabled="serverPagination.page === 0"
        @click="emit('page-change', serverPagination.page - 1)" title="上一页">‹</Button>
      <div class="relative inline-flex items-center">
        <select
          :value="serverPagination.page + 1"
          class="h-7 w-16 appearance-none rounded-md border border-input bg-background pl-2 pr-5 text-xs ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-1 cursor-pointer"
          @change="jumpToPage(($event.target as HTMLSelectElement).value)"
        >
          <option v-for="p in serverTotalPages" :key="p" :value="p">{{ p }}</option>
        </select>
        <ChevronDown class="pointer-events-none absolute right-1 h-3 w-3 text-muted-foreground" />
      </div>
      <span class="text-xs text-muted-foreground whitespace-nowrap">/ {{ serverTotalPages }}</span>
      <Button variant="outline" size="icon" class="h-7 w-7 text-xs"
        :disabled="(serverPagination.page + 1) * serverPagination.pageSize >= serverPagination.total"
        @click="emit('page-change', serverPagination.page + 1)" title="下一页">›</Button>
      <Button variant="outline" size="icon" class="h-7 w-7 text-xs"
        :disabled="serverPagination.page === serverTotalPages - 1"
        @click="emit('page-change', serverTotalPages - 1)" title="最后页">»</Button>
      <span class="text-xs text-muted-foreground ml-1">{{ serverPagination.pageSize }}/页</span>
    </div>

    <!-- Client-side pagination -->
    <div
      v-else-if="pageSize > 0 && data.length > pageSize"
      class="flex items-center justify-center gap-1 py-1.5 shrink-0"
    >
      <Button variant="outline" size="icon" class="h-7 w-7 text-xs"
        :disabled="!table.getCanPreviousPage()"
        @click="table.setPageIndex(0)" title="第一页">«</Button>
      <Button variant="outline" size="icon" class="h-7 w-7 text-xs"
        :disabled="!table.getCanPreviousPage()"
        @click="table.previousPage()" title="上一页">‹</Button>
      <div class="relative inline-flex items-center">
        <select
          :value="clientCurrentPage + 1"
          class="h-7 w-16 appearance-none rounded-md border border-input bg-background pl-2 pr-5 text-xs ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-1 cursor-pointer"
          @change="jumpClientPage(($event.target as HTMLSelectElement).value)"
        >
          <option v-for="p in clientTotalPages" :key="p" :value="p">{{ p }}</option>
        </select>
        <ChevronDown class="pointer-events-none absolute right-1 h-3 w-3 text-muted-foreground" />
      </div>
      <span class="text-xs text-muted-foreground whitespace-nowrap">/ {{ clientTotalPages }}</span>
      <Button variant="outline" size="icon" class="h-7 w-7 text-xs"
        :disabled="!table.getCanNextPage()"
        @click="table.nextPage()" title="下一页">›</Button>
      <Button variant="outline" size="icon" class="h-7 w-7 text-xs"
        :disabled="!table.getCanNextPage()"
        @click="table.setPageIndex(clientTotalPages - 1)" title="最后页">»</Button>
      <span class="text-xs text-muted-foreground ml-1">{{ pageSize }}/页</span>
    </div>
  </div>
</template>

<template>
  <section class="section">
    <div class="section-header">
      <h2>高级设置</h2>
      <p class="section-desc">故障排查与调试工具</p>
    </div>

    <div class="settings-card">
      <div class="card-title">配置文件</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>配置文件目录</label>
          <p class="setting-hint">{{ configDirDisplay }}</p>
        </div>
        <div class="setting-control" style="display: flex; gap: 8px">
          <Button
            v-if="!isPortable"
            variant="outline"
            size="sm"
            @click="dataDirDialogVisible = true"
            >更改</Button
          >
          <Button variant="outline" size="sm" @click="$emit('openConfigFolder')"
            >打开文件夹</Button
          >
        </div>
      </div>
    </div>

    <div class="settings-card">
      <div class="card-title">日志设置</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>服务日志级别</label>
          <p class="setting-hint">重启输入法服务后生效</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.advanced.log_level"
            @update:model-value="formData.advanced.log_level = $event"
          >
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="debug">Debug（调试）</SelectItem>
              <SelectItem value="info">Info（信息）</SelectItem>
              <SelectItem value="warn">Warn（警告）</SelectItem>
              <SelectItem value="error">Error（错误）</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>TSF 日志输出方式</label>
          <p class="setting-hint">仅对新进程生效</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="props.tsfLogConfig.mode"
            @update:model-value="props.tsfLogConfig.mode = $event"
          >
            <SelectTrigger class="w-[200px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="none">None（关闭）</SelectItem>
              <SelectItem value="file">File（文件）</SelectItem>
              <SelectItem value="debugstring"
                >DebugString（调试输出）</SelectItem
              >
              <SelectItem value="all">All（文件 + 调试输出）</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>TSF 日志级别</label>
          <p class="setting-hint">请仅在调试问题时才使用 Debug / Trace</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="props.tsfLogConfig.level"
            @update:model-value="props.tsfLogConfig.level = $event"
          >
            <SelectTrigger class="w-[200px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="off">Off（关闭）</SelectItem>
              <SelectItem value="error">Error（错误）</SelectItem>
              <SelectItem value="warn">Warn（警告）</SelectItem>
              <SelectItem value="info">Info（信息）</SelectItem>
              <SelectItem value="debug">Debug（调试）</SelectItem>
              <SelectItem value="trace">Trace（详细跟踪）</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div v-if="showSensitiveLogWarning" class="setting-item">
        <div class="setting-info">
          <label>调试提示</label>
          <p class="setting-hint warning-text">
            当前已启用调试级别日志。日志中可能包含按键、上下文状态，极端情况下可能暴露输入内容，请仅在排障时临时开启，并注意日志文件的保存与分享范围。
          </p>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>日志目录</label>
          <p class="setting-hint">{{ logsDirDisplay }}</p>
        </div>
        <div class="setting-control">
          <Button variant="outline" size="sm" @click="$emit('openLogFolder')"
            >打开文件夹</Button
          >
        </div>
      </div>
    </div>

    <div class="settings-card">
      <div class="card-title">性能诊断</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>按键链路采样</label>
          <p class="setting-hint">
            开启后记录每次按键的引擎耗时等数据，用于性能分析
          </p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.advanced.perf_sampling ?? false"
            @update:checked="formData.advanced.perf_sampling = $event"
          />
        </div>
      </div>
      <div v-if="formData.advanced.perf_sampling" class="setting-item">
        <div class="setting-info">
          <label>隐私提示</label>
          <p class="setting-hint warning-text">
            采样数据包含用户输入内容（按键编码、候选词等），仅建议在排障或性能调优时临时开启。关闭后不再记录新数据，已有数据可通过导出保留。
          </p>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>采样状态</label>
          <p class="setting-hint">
            <template v-if="perfStats">
              已收集 {{ perfStats.count }}/{{ perfStats.capacity }} 条样本
            </template>
            <template v-else>加载中…</template>
          </p>
        </div>
        <div class="setting-control" style="display: flex; gap: 8px">
          <Button
            variant="outline"
            size="sm"
            :disabled="!perfStats || perfStats.count === 0"
            @click="handleViewPerf"
          >
            查看
          </Button>
          <Button
            variant="outline"
            size="sm"
            :disabled="!perfStats || perfStats.count === 0"
            @click="handleExportPerf"
          >
            导出
          </Button>
          <Button
            variant="outline"
            size="sm"
            :disabled="!perfStats || perfStats.count === 0"
            @click="handleClearPerf"
          >
            清空
          </Button>
        </div>
      </div>
    </div>

    <Dialog v-model:open="viewDialogOpen">
      <DialogContent class="max-w-3xl max-h-[80vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>性能诊断数据</DialogTitle>
        </DialogHeader>
        <pre class="perf-content">{{ viewContent }}</pre>
        <DialogFooter>
          <Button variant="outline" size="sm" @click="viewDialogOpen = false"
            >关闭</Button
          >
        </DialogFooter>
      </DialogContent>
    </Dialog>

    <DataDirDialog
      :visible="dataDirDialogVisible"
      @update:visible="dataDirDialogVisible = $event"
      @changed="onDataDirChanged"
    />
  </section>
</template>

<script setup lang="ts">
import { computed, ref, onMounted } from "vue";
import type { Config, TSFLogConfig } from "../api/settings";
import * as wailsApi from "../api/wails";
import type { PerfStatsResult } from "../api/wails";
import { useToast } from "../composables/useToast";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from "@/components/ui/select";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import DataDirDialog from "@/components/DataDirDialog.vue";

const props = defineProps<{
  formData: Config;
  tsfLogConfig: TSFLogConfig;
  isWailsEnv: boolean;
}>();

const emit = defineEmits<{
  openLogFolder: [];
  openConfigFolder: [];
}>();

const configDirDisplay = ref("%APPDATA%\\WindInput");
const logsDirDisplay = ref("%LOCALAPPDATA%\\WindInput\\logs\\");
const isPortable = ref(false);
const dataDirDialogVisible = ref(false);

// ── 性能诊断 ──
const perfStats = ref<PerfStatsResult | null>(null);
const viewDialogOpen = ref(false);
const viewContent = ref("");
const { toast } = useToast();

async function refreshPerfStats() {
  if (!props.isWailsEnv) return;
  try {
    perfStats.value = await wailsApi.getPerfStats();
  } catch {
    // 服务未运行时静默忽略
  }
}

async function handleViewPerf() {
  try {
    const result = await wailsApi.readPerfFile();
    if (result.count === 0) {
      toast("暂无性能数据", "error");
      return;
    }
    viewContent.value = result.content;
    viewDialogOpen.value = true;
  } catch (e: any) {
    toast("读取失败: " + (e.message || e), "error");
  }
}

async function handleExportPerf() {
  try {
    const result = await wailsApi.exportPerfData();
    if (result.cancelled) return;
    toast(`已导出 ${result.count} 条样本`);
    await refreshPerfStats();
  } catch (e: any) {
    toast("导出失败: " + (e.message || e), "error");
  }
}

async function handleClearPerf() {
  try {
    await wailsApi.dumpPerf("", true);
    toast("已清空性能数据");
    await refreshPerfStats();
  } catch (e: any) {
    toast("清空失败: " + (e.message || e), "error");
  }
}

onMounted(async () => {
  if (props.isWailsEnv) {
    try {
      const info = await wailsApi.getPathInfo();
      configDirDisplay.value = info.config_dir_display;
      logsDirDisplay.value = info.logs_dir_display;
      isPortable.value = info.is_portable;
    } catch (e) {
      console.warn("Failed to get path info:", e);
    }
    await refreshPerfStats();
  }
});

async function onDataDirChanged() {
  // 刷新显示的路径
  try {
    const info = await wailsApi.getPathInfo();
    configDirDisplay.value = info.config_dir_display;
  } catch {
    // ignore
  }
}

const showSensitiveLogWarning = computed(() => {
  const serviceLevel = props.formData.advanced.log_level;
  const tsfLevel = props.tsfLogConfig.level;
  return (
    serviceLevel === "debug" || tsfLevel === "debug" || tsfLevel === "trace"
  );
});
</script>

<style scoped>
.warning-text {
  color: hsl(var(--warning));
}
.perf-content {
  font-family: monospace;
  font-size: 0.8em;
  line-height: 1.4;
  overflow: auto;
  background: hsl(var(--muted));
  border-radius: 6px;
  padding: 12px;
  max-height: 50vh;
  white-space: pre;
  word-break: normal;
}
</style>

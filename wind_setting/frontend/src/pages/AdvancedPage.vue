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
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from "@/components/ui/select";
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
</style>

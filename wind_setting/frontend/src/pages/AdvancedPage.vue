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
          <p class="setting-hint">%APPDATA%\WindInput</p>
        </div>
        <div class="setting-control">
          <button class="btn btn-sm" @click="$emit('openConfigFolder')">打开文件夹</button>
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
          <select v-model="formData.advanced.log_level" class="select">
            <option value="debug">Debug（调试）</option>
            <option value="info">Info（信息）</option>
            <option value="warn">Warn（警告）</option>
            <option value="error">Error（错误）</option>
          </select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>TSF 日志输出方式</label>
          <p class="setting-hint">写入 tsf_log_config，新的 TSF 宿主进程会按此方式输出</p>
        </div>
        <div class="setting-control">
          <select v-model="props.tsfLogConfig.mode" class="select">
            <option value="none">None（关闭）</option>
            <option value="file">File（文件）</option>
            <option value="debugstring">DebugString（调试输出）</option>
            <option value="all">All（文件 + 调试输出）</option>
          </select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>TSF 日志级别</label>
          <p class="setting-hint">建议平时用 Info，需要追踪兼容性问题时再切到 Debug / Trace</p>
        </div>
        <div class="setting-control">
          <select v-model="props.tsfLogConfig.level" class="select">
            <option value="off">Off（仅关闭输出）</option>
            <option value="error">Error（错误）</option>
            <option value="warn">Warn（警告）</option>
            <option value="info">Info（信息）</option>
            <option value="debug">Debug（调试）</option>
            <option value="trace">Trace（详细跟踪）</option>
          </select>
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
          <p class="setting-hint">{{ logPath }}</p>
          <p class="setting-hint">TSF 配置文件：{{ tsfConfigPath }}</p>
        </div>
        <div class="setting-control">
          <button class="btn btn-sm" @click="$emit('openLogFolder')">打开文件夹</button>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { Config, TSFLogConfig } from "../api/settings";

const props = defineProps<{
  formData: Config;
  tsfLogConfig: TSFLogConfig;
  isWailsEnv: boolean;
}>();

const emit = defineEmits<{
  openLogFolder: [];
  openConfigFolder: [];
}>();

const logPath = "%LOCALAPPDATA%\\WindInput\\logs\\";
const tsfConfigPath = "%LOCALAPPDATA%\\WindInput\\logs\\tsf_log_config";
const showSensitiveLogWarning = computed(() => {
  const serviceLevel = props.formData.advanced.log_level;
  const tsfLevel = props.tsfLogConfig.level;
  return serviceLevel === "debug" || tsfLevel === "debug" || tsfLevel === "trace";
});
</script>

<style scoped>
.warning-text {
  color: #a84f00;
}
</style>

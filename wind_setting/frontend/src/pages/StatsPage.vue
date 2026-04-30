<script setup lang="ts">
import { ref, onMounted, computed } from "vue";
import * as wailsApi from "../api/wails";
import type { StatsSummary, DailyStatItem, StatsConfig } from "../api/wails";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { provideToast } from "../composables/useToast";
import { useConfirm } from "../composables/useConfirm";

defineProps<{
  isWailsEnv: boolean;
}>();

const { toast } = provideToast();
const { confirm } = useConfirm();

const loading = ref(true);
const summary = ref<StatsSummary | null>(null);
const heatmapData = ref<DailyStatItem[]>([]);
const statsConfig = ref<StatsConfig>({
  enabled: true,
  retain_days: 0,
  track_english: true,
});
const clearBeforeDays = ref("180");
const tooltip = ref({
  visible: false,
  text: "",
  x: 0,
  y: 0,
});

// 格式化数字
function formatNum(n: number): string {
  if (n >= 10000) return (n / 10000).toFixed(1) + "万";
  return n.toLocaleString();
}

function dateKey(date: Date): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, "0");
  const d = String(date.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

function parseDateKey(key: string): Date {
  const [y, m, d] = key.split("-").map(Number);
  return new Date(y, m - 1, d);
}

function formatDateLabel(key: string): string {
  const date = parseDateKey(key);
  return `${date.getMonth() + 1}月${date.getDate()}日`;
}

const WEEKDAY_CN = ["日", "一", "二", "三", "四", "五", "六"];
function weekdayLabel(key: string): string {
  return `星期${WEEKDAY_CN[parseDateKey(key).getDay()]}`;
}

function speedOfDay(chars: number, activeSeconds: number): number {
  if (!activeSeconds || activeSeconds <= 0 || chars <= 0) return 0;
  return Math.round((chars / activeSeconds) * 60);
}

function showTooltip(event: MouseEvent, text: string) {
  tooltip.value = {
    visible: true,
    text,
    x: event.clientX,
    y: event.clientY,
  };
}

function moveTooltip(event: MouseEvent) {
  if (!tooltip.value.visible) return;
  tooltip.value.x = event.clientX;
  tooltip.value.y = event.clientY;
}

function hideTooltip() {
  tooltip.value.visible = false;
}

interface HeatmapDay {
  date: string;
  chars: number;
  weekday: number;
  cc: number;
  ec: number;
  pc: number;
  oc: number;
  as: number;
}

function dayTooltip(day: HeatmapDay): string {
  const lines = [
    `${formatDateLabel(day.date)} ${weekdayLabel(day.date)}`,
    `总字 ${formatNum(day.chars)}`,
  ];
  if (day.chars > 0) {
    const parts: string[] = [];
    if (day.cc) parts.push(`中 ${formatNum(day.cc)}`);
    if (day.ec) parts.push(`英 ${formatNum(day.ec)}`);
    if (day.pc) parts.push(`符 ${formatNum(day.pc)}`);
    if (day.oc) parts.push(`数 ${formatNum(day.oc)}`);
    if (parts.length) lines.push(parts.join("  "));
    const sp = speedOfDay(day.chars, day.as);
    if (sp > 0) lines.push(`速度 ${sp} 字/分`);
  }
  return lines.join("\n");
}

function hourTooltip(bar: { hour: number; value: number }): string {
  const nextHour = String(bar.hour).padStart(2, "0");
  const todayTotal = summary.value?.today_chars || 0;
  const lines = [
    `${nextHour}:00 - ${nextHour}:59`,
    `${formatNum(bar.value)} 字`,
  ];
  if (bar.value > 0 && todayTotal > 0) {
    const pct = ((bar.value / todayTotal) * 100).toFixed(1);
    lines.push(`占今日 ${pct}%`);
  }
  return lines.join("\n");
}

// 热力图相关
const heatmapWeeks = computed(() => {
  const weeks: HeatmapDay[][] = [];
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const startDate = new Date(today);
  startDate.setDate(startDate.getDate() - 180); // 近6个月
  // 对齐到周一
  startDate.setDate(startDate.getDate() - ((startDate.getDay() + 6) % 7));

  const dataMap = new Map<string, DailyStatItem>();
  for (const d of heatmapData.value) {
    dataMap.set(d.d, d);
  }

  let currentWeek: HeatmapDay[] = [];
  const current = new Date(startDate);
  while (current <= today) {
    const dateStr = dateKey(current);
    const weekday = (current.getDay() + 6) % 7; // 0=Mon, 6=Sun
    const item = dataMap.get(dateStr);
    currentWeek.push({
      date: dateStr,
      chars: item?.tc || 0,
      weekday,
      cc: item?.cc || 0,
      ec: item?.ec || 0,
      pc: item?.pc || 0,
      oc: item?.oc || 0,
      as: item?.as || 0,
    });
    if (weekday === 6 || dateKey(current) === dateKey(today)) {
      weeks.push(currentWeek);
      currentWeek = [];
    }
    current.setDate(current.getDate() + 1);
  }
  if (currentWeek.length > 0) weeks.push(currentWeek);
  return weeks;
});

const heatmapCells = computed(() =>
  heatmapWeeks.value.flatMap((week, weekIndex) =>
    week.map((day) => ({
      ...day,
      weekIndex,
    })),
  ),
);

const heatColors = ["#ebedf0", "#9be9a8", "#40c463", "#30a14e", "#216e39"];

function heatColor(chars: number): string {
  if (chars === 0) return heatColors[0];
  if (chars < 500) return heatColors[1];
  if (chars < 2000) return heatColors[2];
  if (chars < 5000) return heatColors[3];
  return heatColors[4];
}

// 今日字符分类细分
const todayBreakdown = computed(() => {
  const todayStr = dateKey(new Date());
  const item = heatmapData.value.find((d) => d.d === todayStr);
  return {
    cc: item?.cc ?? summary.value?.today_chinese ?? 0,
    ec: item?.ec ?? summary.value?.today_english ?? 0,
    pc: item?.pc ?? 0,
    oc: item?.oc ?? 0,
  };
});

// 时段柱状图（始终返回24项，无数据时显示空柱）
const hourBars = computed(() => {
  const todayStr = dateKey(new Date());
  const todayData = heatmapData.value.find((d) => d.d === todayStr);
  const hours = todayData?.h || new Array(24).fill(0);
  const max = Math.max(...hours, 1);
  return hours.map((v: number, i: number) => ({
    hour: i,
    value: v,
    height: v > 0 ? Math.max(Math.round((v / max) * 100), 4) : 0,
  }));
});

// 字符分类分布（中/英/标点/数字符号），统计窗口内累计
const charCategoryBars = computed(() => {
  if (!heatmapData.value.length) return [];
  let cc = 0,
    ec = 0,
    pc = 0,
    oc = 0;
  for (const d of heatmapData.value) {
    cc += d.cc || 0;
    ec += d.ec || 0;
    pc += d.pc || 0;
    oc += d.oc || 0;
  }
  const total = cc + ec + pc + oc;
  if (total === 0) return [];
  const items = [
    { label: "中文", count: cc },
    { label: "英文", count: ec },
    { label: "标点", count: pc },
    { label: "数字符号", count: oc },
  ];
  return items
    .filter((it) => it.count > 0)
    .map((it) => ({
      label: it.label,
      count: it.count,
      pct: Math.round((it.count / total) * 100),
    }));
});

// 码长分布
const codeLenBars = computed(() => {
  if (!heatmapData.value.length) return [];
  let dist = [0, 0, 0, 0, 0, 0];
  for (const d of heatmapData.value) {
    if (d.cld) for (let i = 0; i < 6; i++) dist[i] += d.cld[i] || 0;
  }
  const total = dist.reduce((a, b) => a + b, 0);
  if (total === 0) return [];
  const labels = ["1码", "2码", "3码", "4码", "5码", "6码+"];
  return dist.map((v, i) => ({
    label: labels[i],
    count: v,
    pct: Math.round((v / total) * 100),
  }));
});

// 方案占比
const schemaBars = computed(() => {
  if (!heatmapData.value.length) return [];
  const map = new Map<string, number>();
  for (const d of heatmapData.value) {
    if (d.bs) {
      for (const [k, v] of Object.entries(d.bs)) {
        map.set(k, (map.get(k) || 0) + v.tc);
      }
    }
  }
  const total = Array.from(map.values()).reduce((a, b) => a + b, 0);
  if (total === 0) return [];
  const schemaNames: Record<string, string> = {
    wubi86: "五笔86",
    pinyin: "拼音",
    shuangpin: "双拼",
    wubi86_pinyin: "五笔拼音混输",
  };
  return Array.from(map.entries())
    .sort((a, b) => b[1] - a[1])
    .map(([k, v]) => ({
      label: schemaNames[k] || k,
      count: v,
      pct: Math.round((v / total) * 100),
    }));
});

const clearBeforeOptions = [
  { value: "30", label: "30 天前" },
  { value: "90", label: "90 天前" },
  { value: "180", label: "180 天前" },
  { value: "365", label: "1 年前" },
  { value: "730", label: "2 年前" },
];

async function loadData() {
  loading.value = true;
  try {
    const [s, cfg] = await Promise.all([
      wailsApi.getStatsSummary(),
      wailsApi.getStatsConfig(),
    ]);
    summary.value = s;
    statsConfig.value = {
      enabled: cfg.enabled ?? true,
      retain_days: 0,
      track_english: cfg.track_english ?? true,
    };

    // 加载近6个月的热力图数据
    const today = new Date();
    const from = new Date(today);
    from.setDate(from.getDate() - 180);
    const days = await wailsApi.getDailyStats(dateKey(from), dateKey(today));
    heatmapData.value = days || [];
  } catch (e) {
    console.error("加载统计数据失败", e);
  } finally {
    loading.value = false;
  }
}

async function saveConfig() {
  try {
    const cfg = {
      enabled: !!statsConfig.value.enabled,
      retain_days: 0,
      track_english: !!statsConfig.value.track_english,
    };
    statsConfig.value = cfg;
    await wailsApi.saveStatsConfig(cfg);
    toast("统计设置已保存");
  } catch (e: any) {
    toast(e.message || "保存失败", "error");
  }
}

async function handleStatsEnabledChange(checked: boolean) {
  statsConfig.value.enabled = checked;
  await saveConfig();
}

async function handleTrackEnglishChange(checked: boolean) {
  statsConfig.value.track_english = checked;
  await saveConfig();
}

async function handleClearStats() {
  const ok = await confirm("确定要清空所有统计数据吗？此操作不可恢复。");
  if (!ok) return;
  try {
    await wailsApi.clearStats();
    toast("统计数据已清空");
    await loadData();
  } catch (e: any) {
    toast(e.message || "清空失败", "error");
  }
}

async function handleClearOldStats() {
  const days = parseInt(clearBeforeDays.value);
  const ok = await confirm(
    `确定要清理 ${days} 天前的统计数据吗？此操作不可恢复。`,
  );
  if (!ok) return;
  try {
    const result = await wailsApi.clearStatsBefore(days);
    toast(`已清理 ${result.count} 天统计数据`);
    await loadData();
  } catch (e: any) {
    toast(e.message || "清理失败", "error");
  }
}

onMounted(loadData);
</script>

<template>
  <section class="section">
    <div class="section-header">
      <h2>
        输入统计
        <span class="beta-tag" title="测试版本，数据统计不一定完全准确"
          >(beta)</span
        >
      </h2>
      <p class="section-desc">查看输入习惯和效率数据</p>
    </div>

    <div v-if="loading" class="loading-hint">加载中...</div>

    <template v-else-if="summary">
      <!-- 数字卡片 -->
      <div class="stat-cards">
        <div class="stat-card">
          <div class="stat-value">{{ formatNum(summary.today_chars) }}</div>
          <div class="stat-label">今日输入</div>
          <div
            class="stat-detail"
            v-if="
              todayBreakdown.cc ||
              todayBreakdown.ec ||
              todayBreakdown.pc ||
              todayBreakdown.oc
            "
          >
            <div>
              中{{ formatNum(todayBreakdown.cc) }} 英{{
                formatNum(todayBreakdown.ec)
              }}
            </div>
            <div>
              符{{ formatNum(todayBreakdown.pc) }} 数{{
                formatNum(todayBreakdown.oc)
              }}
            </div>
          </div>
        </div>
        <div class="stat-card">
          <div class="stat-value">
            {{ formatNum(Number(summary.total_chars)) }}
          </div>
          <div class="stat-label">累计输入</div>
          <div class="stat-detail">{{ summary.active_days }} 天</div>
        </div>
        <div class="stat-card">
          <div class="stat-value">{{ formatNum(summary.daily_avg) }}</div>
          <div class="stat-label">日均输入</div>
        </div>
        <div class="stat-card">
          <div class="stat-value">{{ summary.streak_current }}</div>
          <div class="stat-label">连续天数</div>
          <div class="stat-detail">最长 {{ summary.streak_max }} 天</div>
        </div>
      </div>

      <!-- 日历热力图 -->
      <div class="settings-card">
        <div class="card-title">输入日历</div>
        <div class="heatmap-container" @mouseleave="hideTooltip">
          <div class="heatmap-scroll">
            <div class="heatmap-body">
              <div class="weekday-labels">
                <span>一</span>
                <span>二</span>
                <span>三</span>
                <span>四</span>
                <span>五</span>
                <span>六</span>
                <span>日</span>
              </div>
              <div class="heatmap-right">
                <div class="heatmap-grid">
                  <div
                    v-for="day in heatmapCells"
                    :key="day.date"
                    class="heatmap-cell"
                    :style="{
                      backgroundColor: heatColor(day.chars),
                      gridRow: day.weekday + 1,
                      gridColumn: day.weekIndex + 1,
                    }"
                    @mouseenter="showTooltip($event, dayTooltip(day))"
                    @mousemove="moveTooltip"
                    @mouseleave="hideTooltip"
                  ></div>
                </div>
                <div class="heatmap-legend">
                  <span class="legend-label">少</span>
                  <span
                    v-for="(c, i) in heatColors"
                    :key="i"
                    class="legend-box"
                    :style="{ backgroundColor: c }"
                  ></span>
                  <span class="legend-label">多</span>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- 今日时段分布 -->
      <div class="settings-card">
        <div class="card-title">今日时段分布</div>
        <div class="hour-chart-wrapper">
          <div v-if="summary.today_chars === 0" class="empty-hint">
            暂无数据
          </div>
          <div class="hour-bars-area">
            <div
              v-for="bar in hourBars"
              :key="bar.hour"
              class="hour-bar-col"
              :title="hourTooltip(bar)"
              @mouseenter="showTooltip($event, hourTooltip(bar))"
              @mousemove="moveTooltip"
              @mouseleave="hideTooltip"
            >
              <div
                class="hour-bar"
                :style="{ height: (bar.height || 2) + '%' }"
                :class="{ 'hour-bar-zero': bar.value === 0 }"
              ></div>
            </div>
          </div>
          <div class="hour-labels">
            <span
              v-for="h in [0, 3, 6, 9, 12, 15, 18, 21]"
              :key="h"
              class="hour-label"
              >{{ h }}</span
            >
          </div>
        </div>
      </div>

      <!-- 输入详情 -->
      <div class="settings-card">
        <div class="card-title">输入详情</div>
        <div class="detail-grid">
          <div class="detail-item">
            <span class="detail-label">本周</span>
            <span class="detail-value"
              >{{ formatNum(summary.week_chars) }} 字</span
            >
          </div>
          <div class="detail-item">
            <span class="detail-label">本月</span>
            <span class="detail-value"
              >{{ formatNum(summary.month_chars) }} 字</span
            >
          </div>
          <div class="detail-item" v-if="summary.max_day_chars > 0">
            <span class="detail-label">最高日</span>
            <span class="detail-value"
              >{{ formatNum(summary.max_day_chars) }} 字 ({{
                summary.max_day_date?.slice(5)
              }})</span
            >
          </div>
          <div class="detail-item" v-if="summary.avg_code_len > 0">
            <span class="detail-label">平均码长</span>
            <span class="detail-value">{{
              summary.avg_code_len.toFixed(2)
            }}</span>
          </div>
          <div class="detail-item" v-if="summary.first_select_rate > 0">
            <span class="detail-label">首选率</span>
            <span class="detail-value"
              >{{ (summary.first_select_rate * 100).toFixed(1) }}%</span
            >
          </div>
          <div class="detail-item" v-if="summary.today_speed > 0">
            <span class="detail-label">今日速度</span>
            <span class="detail-value">{{ summary.today_speed }} 字/分钟</span>
          </div>
          <div class="detail-item" v-if="summary.overall_speed > 0">
            <span class="detail-label">平均速度</span>
            <span class="detail-value"
              >{{ summary.overall_speed }} 字/分钟</span
            >
          </div>
          <div class="detail-item" v-if="summary.max_speed > 0">
            <span class="detail-label">历史最快</span>
            <span class="detail-value">{{ summary.max_speed }} 字/分钟</span>
          </div>
        </div>

        <!-- 字符分类 -->
        <template v-if="charCategoryBars.length > 0">
          <div class="sub-title">字符分类</div>
          <div class="bar-chart-h">
            <div
              v-for="bar in charCategoryBars"
              :key="bar.label"
              class="bar-row"
            >
              <span class="bar-label">{{ bar.label }}</span>
              <div class="bar-track">
                <div
                  class="bar-fill char-fill"
                  :style="{ width: bar.pct + '%' }"
                ></div>
              </div>
              <span class="bar-pct">{{ bar.pct }}%</span>
            </div>
          </div>
        </template>

        <!-- 码长分布 -->
        <template v-if="codeLenBars.length > 0">
          <div class="sub-title">码长分布</div>
          <div class="bar-chart-h">
            <div v-for="bar in codeLenBars" :key="bar.label" class="bar-row">
              <span class="bar-label">{{ bar.label }}</span>
              <div class="bar-track">
                <div class="bar-fill" :style="{ width: bar.pct + '%' }"></div>
              </div>
              <span class="bar-pct">{{ bar.pct }}%</span>
            </div>
          </div>
        </template>

        <!-- 方案占比 -->
        <template v-if="schemaBars.length > 1">
          <div class="sub-title">方案占比</div>
          <div class="bar-chart-h">
            <div v-for="bar in schemaBars" :key="bar.label" class="bar-row">
              <span class="bar-label">{{ bar.label }}</span>
              <div class="bar-track">
                <div
                  class="bar-fill schema-fill"
                  :style="{ width: bar.pct + '%' }"
                ></div>
              </div>
              <span class="bar-pct">{{ bar.pct }}%</span>
            </div>
          </div>
        </template>
      </div>

      <!-- 统计设置 -->
      <div class="settings-card">
        <div class="card-title">统计设置</div>
        <div class="setting-item">
          <div class="setting-info">
            <label>启用输入统计</label>
          </div>
          <div class="setting-control">
            <Switch
              :checked="statsConfig.enabled"
              @update:checked="handleStatsEnabledChange"
            />
          </div>
        </div>
        <div class="setting-item">
          <div class="setting-info">
            <label>统计英文模式</label>
          </div>
          <div class="setting-control">
            <Switch
              :checked="statsConfig.track_english"
              @update:checked="handleTrackEnglishChange"
            />
          </div>
        </div>
        <div class="setting-item">
          <div class="setting-info">
            <label>数据清理</label>
            <p class="setting-hint">手动删除指定范围的历史统计数据</p>
          </div>
          <div class="setting-control control-row">
            <Select v-model="clearBeforeDays">
              <SelectTrigger class="w-[140px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem
                  v-for="opt in clearBeforeOptions"
                  :key="opt.value"
                  :value="opt.value"
                >
                  {{ opt.label }}
                </SelectItem>
              </SelectContent>
            </Select>
            <Button variant="destructive" size="sm" @click="handleClearOldStats"
              >清理</Button
            >
            <Button variant="destructive" size="sm" @click="handleClearStats"
              >清空全部</Button
            >
          </div>
        </div>
      </div>
    </template>

    <Teleport to="body">
      <div
        v-if="tooltip.visible"
        class="stats-tooltip"
        :style="{ left: tooltip.x + 'px', top: tooltip.y + 'px' }"
      >
        {{ tooltip.text }}
      </div>
    </Teleport>
  </section>
</template>

<style scoped>
.loading-hint {
  text-align: center;
  padding: 40px;
  color: var(--text-secondary);
}

/* 数字卡片 */
.stat-cards {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 12px;
  margin-bottom: 16px;
}

.stat-card {
  background: var(--card-bg, #fff);
  border: 1px solid var(--border-color, #e5e7eb);
  border-radius: 8px;
  padding: 16px;
  text-align: center;
}

.stat-value {
  font-size: 24px;
  font-weight: 700;
  color: var(--text-primary);
  line-height: 1.2;
}

.stat-label {
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 4px;
}

.stat-detail {
  font-size: 11px;
  color: var(--text-tertiary, #999);
  margin-top: 2px;
}

/* 热力图 */
.heatmap-container {
  padding: 8px 0;
}

.heatmap-scroll {
  overflow-x: auto;
  padding-bottom: 8px;
}

.heatmap-body {
  display: inline-flex;
  align-items: flex-start;
  gap: 6px;
  min-width: max-content;
}

.weekday-labels {
  display: grid;
  grid-template-rows: repeat(7, 12px);
  gap: 3px;
  padding-top: 0;
  width: 14px;
  flex-shrink: 0;
}

.weekday-labels span {
  font-size: 10px;
  line-height: 12px;
  color: var(--text-tertiary, #8b949e);
}

.heatmap-right {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: 4px;
}

.heatmap-grid {
  display: grid;
  grid-template-rows: repeat(7, 12px);
  grid-auto-columns: 12px;
  grid-auto-flow: column;
  gap: 3px;
}

.heatmap-cell {
  width: 12px;
  height: 12px;
  border-radius: 2px;
  cursor: default;
  border: 1px solid rgba(27, 31, 36, 0.06);
  box-sizing: border-box;
}

.heatmap-legend {
  display: inline-flex;
  align-items: center;
  align-self: flex-end;
  gap: 4px;
  font-size: 11px;
  color: var(--text-secondary);
  white-space: nowrap;
  margin-top: 2px;
}

.legend-box {
  width: 12px;
  height: 12px;
  border-radius: 2px;
}

.legend-label {
  font-size: 11px;
}

/* 时段柱状图 */
.hour-chart-wrapper {
  position: relative;
  padding: 8px 0 0;
}

.empty-hint {
  position: absolute;
  top: 40%;
  left: 50%;
  transform: translate(-50%, -50%);
  font-size: 13px;
  color: var(--text-tertiary, #bbb);
  pointer-events: none;
  z-index: 1;
}

.hour-bars-area {
  display: flex;
  align-items: flex-end;
  height: 80px;
  gap: 2px;
}

.hour-bar-col {
  flex: 1;
  height: 100%;
  display: flex;
  align-items: flex-end;
  cursor: default;
}

.hour-bar {
  width: 100%;
  min-height: 2px;
  background: #30a14e;
  border-radius: 2px 2px 0 0;
  transition: height 0.3s;
}

.hour-bar-zero {
  background: var(--border-color, #e5e7eb);
  opacity: 0.5;
}

.hour-labels {
  display: flex;
  justify-content: space-between;
  padding: 4px 0 0;
}

.hour-label {
  font-size: 10px;
  color: var(--text-secondary);
  width: calc(100% / 8);
  text-align: left;
}

.hour-label {
  font-size: 10px;
  color: var(--text-secondary);
  margin-top: 2px;
}

/* 详情网格 */
.detail-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
  gap: 8px;
  padding: 4px 0;
}

.detail-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 6px 8px;
  background: var(--bg-secondary, #f9fafb);
  border-radius: 6px;
}

.detail-label {
  font-size: 13px;
  color: var(--text-secondary);
}

.detail-value {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-primary);
}

/* 水平条形图 */
.sub-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-secondary);
  margin-top: 12px;
  margin-bottom: 6px;
}

.bar-chart-h {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.bar-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.bar-label {
  width: 64px;
  font-size: 12px;
  color: var(--text-secondary);
  text-align: right;
  flex-shrink: 0;
}

.bar-track {
  flex: 1;
  height: 16px;
  background: var(--bg-secondary, #f3f4f6);
  border-radius: 4px;
  overflow: hidden;
}

.bar-fill {
  height: 100%;
  background: #30a14e;
  border-radius: 4px;
  transition: width 0.3s;
}

.bar-fill.schema-fill {
  background: #6366f1;
}

.bar-fill.char-fill {
  background: #f59e0b;
}

.beta-tag {
  font-size: 12px;
  font-weight: 500;
  color: #f59e0b;
  margin-left: 4px;
  vertical-align: middle;
  cursor: help;
}

.bar-pct {
  width: 36px;
  font-size: 12px;
  color: var(--text-secondary);
  text-align: right;
  flex-shrink: 0;
}

.control-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  justify-content: flex-end;
}

.stats-tooltip {
  position: fixed;
  z-index: 10000;
  transform: translate(-50%, calc(-100% - 10px));
  padding: 6px 8px;
  border-radius: 6px;
  background: #24292f;
  color: #fff;
  font-size: 12px;
  line-height: 1.4;
  white-space: pre-line;
  pointer-events: none;
  box-shadow: 0 8px 24px rgba(140, 149, 159, 0.28);
}

.stats-tooltip::after {
  content: "";
  position: absolute;
  left: 50%;
  bottom: -5px;
  width: 10px;
  height: 10px;
  background: #24292f;
  transform: translateX(-50%) rotate(45deg);
}
</style>

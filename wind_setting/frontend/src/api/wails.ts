// Wails API 封装层 - 使用 Wails 绑定调用 Go 后端

// 导入 Wails 生成的绑定和模型
import * as App from "../../wailsjs/go/main/App";
import { main, rpcapi } from "../../wailsjs/go/models";
import type { ConfigSetItem } from "../lib/configDiff";
import {
  getDefaultConfig as getHTTPDefaultConfig,
  getDefaultTSFLogConfig as getHTTPTSFLogConfig,
  type Config,
  type TSFLogConfig,
} from "./settings";
import { WailsEvent } from "../lib/enums";

// 重新导出类型
export type UserWordItem = main.UserWordItem;
export type ShadowRuleItem = main.ShadowRuleItem;
export type FileChangeStatus = main.FileChangeStatus;
export type ServiceStatus = rpcapi.SystemStatusReply;
export type ThemeInfo = main.ThemeInfo;
export type SchemaInfo = main.SchemaInfo;
export type SchemaConfig = main.SchemaConfig;
export interface SystemFontInfo {
  family: string;
  display_name: string;
}

// ── 短语类型 ──
//
// 2026-05-16 schema 简化: 短语统一为 (code, text, weight) 三元组,
// text 自描述分类 (含 $AA / $SS / $CC marker → 对应类型, 否则普通短语)。
export interface PhraseItem {
  code: string;
  text?: string;
  position: number;
  weight?: number;
  // 后端计算的生效权重 (weight>0 → 自身; weight==0 && position>0 → 10000-position; 否则默认 1000)
  // 仅供表格 / 对话框展示, 保存时仍按 weight 字段提交 (允许 0)。
  effective_weight: number;
  enabled: boolean;
  is_system: boolean;
}

// ── 短语 value 校验类型 ──
export type PhraseValueKind =
  | "command"
  | "command-prefix"
  | "array"
  | "template"
  | "literal"
  | "error";

export interface PhraseValidateValueReply {
  kind: PhraseValueKind;
  display?: string;
  actions_count?: number;
  error_msg?: string;
}

// ── 词频类型 ──
export interface FreqItem {
  code: string;
  text: string;
  count: number;
  last_used: number;
  streak: number;
  boost: number;
}

// ── 方案状态类型 ──
export interface SchemaStatusItem {
  schema_id: string;
  schema_name: string;
  engine_type: string; // codetable | pinyin | mixed
  is_mixed: boolean;
  is_shuangpin: boolean; // 双拼方案：用户词库的 code 仍以全拼存储
  shuangpin_layout?: string; // 双拼布局 ID（xiaohe / ziranma / mspy / sogou / abc / ziguang）
  data_schema_id?: string; // 实际存储桶 ID（与 schema_id 不同时表示该方案共享其它方案的数据桶）
  status: "enabled" | "disabled" | "orphaned";
  user_words: number;
  temp_words: number;
  shadow_rules: number;
  freq_records: number;
}

// ── 事件类型 ──
export interface DictEvent {
  type: string;
  schema_id: string;
  action: string;
}

export interface StatsEvent {
  type: string;
  action: string;
}

export interface SystemEvent {
  type: string;
  action: string;
}

// ===== Schema API =====

export async function getAvailableSchemas(): Promise<SchemaInfo[]> {
  return App.GetAvailableSchemas();
}

export async function getSchemaConfig(schemaID: string): Promise<SchemaConfig> {
  return App.GetSchemaConfig(schemaID);
}

export async function saveSchemaConfig(
  schemaID: string,
  cfg: SchemaConfig,
): Promise<void> {
  return App.SaveSchemaConfig(schemaID, cfg);
}

export async function resetSchemaConfig(schemaID: string): Promise<void> {
  return (window as any).go.main.App.ResetSchemaConfig(schemaID);
}

export async function setDictEnabled(
  schemaID: string,
  dictID: string,
  enabled: boolean,
): Promise<void> {
  return App.SetDictEnabled(schemaID, dictID, enabled);
}

export async function switchActiveSchema(schemaID: string): Promise<void> {
  return App.SwitchActiveSchema(schemaID);
}

// 方案引用关系
export interface SchemaReference {
  primary_schema?: string;
  secondary_schema?: string;
  temp_pinyin_schema?: string;
  referenced_by?: string[];
}

export async function getSchemaReferences(): Promise<
  Record<string, SchemaReference>
> {
  return App.GetSchemaReferences() as any;
}

export async function getReferencedSchemaIDs(): Promise<string[]> {
  return App.GetReferencedSchemaIDs() as any;
}

// ===== Schema Import/Export/Delete =====

export interface ImportPreviewSchema {
  id: string;
  name: string;
  version: string;
  author: string;
  description: string;
  engine_type: string;
  dict_count: number;
  conflict: boolean;
  conflict_src: string;
}

export interface ImportPreview {
  zip_path: string;
  schemas: ImportPreviewSchema[];
  file_count: number;
}

export async function exportSchema(schemaID: string): Promise<string> {
  return App.ExportSchema(schemaID);
}

export async function exportSchemas(schemaIDs: string[]): Promise<string> {
  return App.ExportSchemas(schemaIDs);
}

export async function previewImportSchema(): Promise<ImportPreview | null> {
  return App.PreviewImportSchema() as unknown as ImportPreview | null;
}

export async function confirmImportSchema(
  zipPath: string,
): Promise<SchemaInfo | null> {
  return App.ConfirmImportSchema(zipPath) as unknown as SchemaInfo | null;
}

export async function deleteSchema(schemaID: string): Promise<void> {
  return App.DeleteSchema(schemaID);
}

// 词库统计类型
export interface DictStats {
  word_count: number;
  phrase_count: number;
  shadow_count: number;
}

// 方案词库统计类型
export interface SchemaDictStatsItem {
  schema_id: string;
  schema_name: string;
  icon_label: string;
  engine_type: string;
  data_schema_id: string;
  alias_ids: string[];
  word_count: number;
  shadow_count: number;
  temp_word_count: number;
}

// 主题预览数据类型
export interface ThemePreview {
  meta: {
    name: string;
    version: string;
    author: string;
  };
  candidate_window: {
    background_color: string;
    border_color: string;
    text_color: string;
    index_color: string;
    index_bg_color: string;
    hover_bg_color: string;
    selected_bg_color: string;
    input_bg_color: string;
    input_text_color: string;
    comment_color: string;
    shadow_color: string;
  };
  toolbar: {
    background_color: string;
    border_color: string;
    grip_color: string;
    mode_chinese_bg_color: string;
    mode_english_bg_color: string;
    mode_text_color: string;
    full_width_off_bg_color: string;
    full_width_off_color: string;
    punct_english_bg_color: string;
    punct_english_color: string;
    settings_bg_color: string;
    settings_icon_color: string;
  };
  style?: {
    index_style: string;
    accent_bar_color?: string;
    index_labels?: string;
    is_v25?: boolean;
  };
  is_dark?: {
    active: boolean;
  };
  // 背景图（仅当 palette 配置了 background 时出现）
  background?: {
    mode: string;
    opacity: number;
    has_image: boolean;
  };
}

// 配置管理
export async function getConfig(): Promise<Config> {
  return (await App.GetConfig()) as any;
}

export interface SaveConfigResult {
  requires_restart: boolean;
}

export async function setConfigItems(
  items: ConfigSetItem[],
): Promise<SaveConfigResult> {
  return (await App.SetConfigItems(items as any)) as any;
}

export async function getTSFLogConfig(): Promise<TSFLogConfig> {
  return (await (window as any).go.main.App.GetTSFLogConfig()) as any;
}

export async function saveTSFLogConfig(cfg: TSFLogConfig): Promise<void> {
  return (window as any).go.main.App.SaveTSFLogConfig(cfg as any);
}

export async function reloadConfig(): Promise<void> {
  return App.ReloadConfig();
}

// ── 短语 API ──
export async function getPhraseList(): Promise<PhraseItem[]> {
  return App.GetPhrases() as Promise<PhraseItem[]>;
}

export async function addPhrase(
  code: string,
  text: string,
  position: number,
  weight: number = 0,
): Promise<void> {
  return App.AddPhrase(code, text, position, weight);
}

export async function updatePhrase(
  code: string,
  text: string,
  newCode: string,
  newText: string,
  newPosition: number,
  newWeight: number | null,
  enabled: boolean | null,
): Promise<void> {
  return App.UpdatePhrase(
    code,
    text,
    newCode,
    newText,
    newPosition,
    newWeight,
    enabled,
  );
}

// 校验短语 value (cmdbar 解析预览, debounce 调用)。
export async function validatePhraseValue(
  value: string,
): Promise<PhraseValidateValueReply> {
  return (await App.ValidatePhraseValue(
    value,
  )) as unknown as PhraseValidateValueReply;
}

export async function removePhrase(code: string, text: string): Promise<void> {
  return App.RemovePhrase(code, text);
}

export interface PhraseDeleteArg {
  code: string;
  text: string;
}

// 批量删除短语 (单事务 + 单次 reload, 显著优于循环单删)
export async function removePhrases(items: PhraseDeleteArg[]): Promise<number> {
  return App.RemovePhrases(items as any);
}

export async function setPhraseEnabled(
  code: string,
  text: string,
  enabled: boolean,
): Promise<void> {
  return App.SetPhraseEnabled(code, text, enabled);
}

export async function resetPhrasesToDefault(): Promise<void> {
  return App.ResetPhrasesToDefault();
}

// 短语 cmd-open 子编辑器: 弹出原生文件选择对话框 (仅 .exe)。
// 后端方法尚未出现在 wails 生成的 d.ts 中, 通过 window.go 兜底调用。
export async function pickExePath(): Promise<string> {
  return (await (window as any).go.main.App.PickExePath()) as string;
}

// 短语 cmd-open 子编辑器: 弹出原生文件选择对话框 (不过滤类型)。
export async function pickAnyPath(): Promise<string> {
  return (await (window as any).go.main.App.PickAnyPath()) as string;
}

export async function importPhrases(): Promise<ImportExportResult> {
  return App.ImportPhrases() as Promise<ImportExportResult>;
}

export async function exportPhrases(): Promise<ImportExportResult> {
  return App.ExportPhrases() as Promise<ImportExportResult>;
}

// ── 词频 API ──
export async function getFreqList(
  schemaID: string,
  prefix: string,
  limit: number,
  offset: number,
): Promise<{ entries: FreqItem[]; total: number }> {
  return App.GetFreqList(schemaID, prefix, limit, offset) as Promise<{
    entries: FreqItem[];
    total: number;
  }>;
}

export async function deleteFreq(
  schemaID: string,
  code: string,
  text: string,
): Promise<void> {
  return App.DeleteFreq(schemaID, code, text);
}

export async function clearFreq(schemaID: string): Promise<number> {
  return App.ClearFreq(schemaID);
}

// ── 方案列表 API ──
export async function getAllSchemaStatuses(): Promise<SchemaStatusItem[]> {
  return App.GetAllSchemaStatuses() as Promise<SchemaStatusItem[]>;
}

// ── 事件监听 ──
export interface ConfigEvent {
  type: string;
  action: string;
}

export function onConfigEvent(callback: (event: ConfigEvent) => void): void {
  (window as any).runtime.EventsOn(WailsEvent.Config, callback);
}

export function offConfigEvent(): void {
  (window as any).runtime.EventsOff(WailsEvent.Config);
}

export function onDictEvent(callback: (event: DictEvent) => void): void {
  (window as any).runtime.EventsOn(WailsEvent.Dict, callback);
}

export function offDictEvent(): void {
  (window as any).runtime.EventsOff(WailsEvent.Dict);
}

export function onStatsEvent(callback: (event: StatsEvent) => void): void {
  (window as any).runtime.EventsOn(WailsEvent.Stats, callback);
}

export function offStatsEvent(): void {
  (window as any).runtime.EventsOff(WailsEvent.Stats);
}

export function onSystemEvent(callback: (event: SystemEvent) => void): void {
  (window as any).runtime.EventsOn(WailsEvent.System, callback);
}

export function offSystemEvent(): void {
  (window as any).runtime.EventsOff(WailsEvent.System);
}

// 用户词库管理
export async function getUserDict(): Promise<UserWordItem[]> {
  return App.GetUserDict();
}

export async function addUserWord(
  code: string,
  text: string,
  weight: number = 0,
): Promise<void> {
  return App.AddUserWord(code, text, weight);
}

export async function removeUserWord(
  code: string,
  text: string,
): Promise<void> {
  return App.RemoveUserWord(code, text);
}

export async function updateUserWord(
  code: string,
  text: string,
  weight: number = 0,
): Promise<void> {
  return App.UpdateUserWord(code, text, weight);
}

export async function searchUserDict(
  query: string,
  limit: number = 100,
): Promise<UserWordItem[]> {
  return App.SearchUserDict(query, limit);
}

export async function getUserDictStats(): Promise<DictStats> {
  const stats = await App.GetUserDictStats();
  return {
    word_count: stats["word_count"] || 0,
    phrase_count: stats["phrase_count"] || 0,
    shadow_count: stats["shadow_count"] || 0,
  };
}

export async function reloadUserDict(): Promise<void> {
  return App.ReloadUserDict();
}

export async function getUserDictSchemaID(): Promise<string> {
  return App.GetUserDictSchemaID();
}

export async function switchUserDictSchema(schemaID: string): Promise<void> {
  return App.SwitchUserDictSchema(schemaID);
}

// 导入导出结果类型
export interface ImportExportResult {
  cancelled: boolean;
  count: number;
  total?: number;
  path?: string;
}

export async function importUserDict(): Promise<ImportExportResult> {
  return App.ImportUserDict() as unknown as ImportExportResult;
}

export async function exportUserDict(): Promise<ImportExportResult> {
  return App.ExportUserDict() as unknown as ImportExportResult;
}

// ===== 按方案操作词库 =====

export async function getEnabledSchemasWithDictStats(): Promise<
  SchemaDictStatsItem[]
> {
  return App.GetEnabledSchemasWithDictStats() as unknown as SchemaDictStatsItem[];
}

export async function clearUserDictForSchema(
  schemaID: string,
): Promise<number> {
  return App.ClearUserDictForSchema(schemaID);
}

export async function generatePinyinCode(text: string): Promise<string> {
  return App.GeneratePinyinCode(text);
}

export async function encodeWordForSchema(
  schemaID: string,
  text: string,
): Promise<string> {
  return App.EncodeWordForSchema(schemaID, text);
}

export async function getUserDictBySchema(
  schemaID: string,
): Promise<UserWordItem[]> {
  return App.GetUserDictBySchema(schemaID);
}

export interface PagedDictResult {
  words: UserWordItem[];
  total: number;
}

export async function getUserDictBySchemaPage(
  schemaID: string,
  prefix: string,
  textQuery: string,
  limit: number,
  offset: number,
): Promise<PagedDictResult> {
  return App.GetUserDictBySchemaPage(
    schemaID,
    prefix,
    textQuery,
    limit,
    offset,
  ) as unknown as PagedDictResult;
}

export async function addUserWordForSchema(
  schemaID: string,
  code: string,
  text: string,
  weight: number = 0,
): Promise<void> {
  return App.AddUserWordForSchema(schemaID, code, text, weight);
}

export async function removeUserWordForSchema(
  schemaID: string,
  code: string,
  text: string,
): Promise<void> {
  return App.RemoveUserWordForSchema(schemaID, code, text);
}

export async function updateUserWordForSchema(
  schemaID: string,
  code: string,
  text: string,
  weight: number = 0,
): Promise<void> {
  return App.UpdateUserWordForSchema(schemaID, code, text, weight);
}

export async function searchUserDictBySchema(
  schemaID: string,
  query: string,
  limit: number = 100,
): Promise<UserWordItem[]> {
  return App.SearchUserDictBySchema(schemaID, query, limit);
}

export async function importUserDictForSchema(
  schemaID: string,
): Promise<ImportExportResult> {
  return App.ImportUserDictForSchema(schemaID) as unknown as ImportExportResult;
}

export async function exportUserDictForSchema(
  schemaID: string,
): Promise<ImportExportResult> {
  return App.ExportUserDictForSchema(schemaID) as unknown as ImportExportResult;
}

// ===== 词库导入导出（新版） =====

export interface DictImportPreview {
  schema_id: string;
  schema_name: string;
  generator: string;
  exported_at: string;
  sections: Record<string, number>;
  source_file: string;
}

export interface TextListPreviewResult {
  total: number;
  success_count: number;
  fail_count: number;
  results: EncodeResultItem[];
}

export interface EncodeResultItem {
  word: string;
  code: string;
  status: "ok" | "no_code" | "no_rule";
  error?: string;
}

export interface ZipImportPreview {
  schemas: ZipSchemaPreviewItem[];
  has_phrases: boolean;
  phrase_count: number;
}

export interface ZipSchemaPreviewItem {
  schema_id: string;
  schema_name: string;
  sections: Record<string, number>;
}

// 导入 - 文件选择
export async function selectImportFile(format: string): Promise<string> {
  return App.SelectImportFile(format);
}

// 导入 - 预览
export async function previewImportFile(
  format: string,
  filePath: string,
): Promise<DictImportPreview> {
  return App.PreviewImportFile(
    format,
    filePath,
  ) as unknown as DictImportPreview;
}

export async function previewTextList(
  filePath: string,
  schemaID: string,
): Promise<TextListPreviewResult> {
  return App.PreviewTextList(
    filePath,
    schemaID,
  ) as unknown as TextListPreviewResult;
}

export async function previewZipImport(
  filePath: string,
): Promise<ZipImportPreview> {
  return App.PreviewZipImport(filePath) as unknown as ZipImportPreview;
}

// 导入 - 执行
export async function executeImport(
  filePath: string,
  format: string,
  schemaID: string,
  sections: string[],
  strategies: Record<string, string>,
): Promise<ImportExportResult> {
  return App.ExecuteImport(
    filePath,
    format,
    schemaID,
    sections,
    strategies,
  ) as unknown as ImportExportResult;
}

export async function executeTextListImport(
  schemaID: string,
  words: EncodeResultItem[],
  weight: number,
): Promise<ImportExportResult> {
  return App.ExecuteTextListImport(
    schemaID,
    words,
    weight,
  ) as unknown as ImportExportResult;
}

export async function executeZipImport(
  filePath: string,
  selectedSchemas: string[],
  includePhrases: boolean,
  strategies: Record<string, string>,
): Promise<ImportExportResult> {
  return App.ExecuteZipImport(
    filePath,
    selectedSchemas,
    includePhrases,
    strategies,
  ) as unknown as ImportExportResult;
}

// 导出
export async function exportSchemaData(
  schemaID: string,
  sections: string[],
  schemaName: string,
): Promise<ImportExportResult> {
  return App.ExportSchemaData(
    schemaID,
    sections,
    schemaName,
  ) as unknown as ImportExportResult;
}

export async function exportPhrasesFile(
  format: string,
): Promise<ImportExportResult> {
  return App.ExportPhrasesFile(format) as unknown as ImportExportResult;
}

export async function exportFullBackup(
  schemaIDs: string[],
  schemaNames: Record<string, string>,
  includePhrases: boolean,
): Promise<ImportExportResult> {
  return App.ExportFullBackup(
    schemaIDs,
    schemaNames,
    includePhrases,
  ) as unknown as ImportExportResult;
}

// ===== 临时词库管理 =====

export interface TempWordItem {
  code: string;
  text: string;
  weight: number;
  count: number;
}

export async function getTempDictBySchema(
  schemaID: string,
): Promise<TempWordItem[]> {
  return App.GetTempDictBySchema(schemaID) as unknown as TempWordItem[];
}

export async function clearTempDictForSchema(
  schemaID: string,
): Promise<number> {
  return App.ClearTempDictForSchema(schemaID);
}

export async function promoteTempWordForSchema(
  schemaID: string,
  code: string,
  text: string,
): Promise<void> {
  return App.PromoteTempWordForSchema(schemaID, code, text);
}

export async function promoteAllTempWordsForSchema(
  schemaID: string,
): Promise<number> {
  return App.PromoteAllTempWordsForSchema(schemaID);
}

export async function removeTempWordForSchema(
  schemaID: string,
  code: string,
  text: string,
): Promise<void> {
  return App.RemoveTempWordForSchema(schemaID, code, text);
}

export async function getShadowBySchema(
  schemaID: string,
): Promise<ShadowRuleItem[]> {
  return App.GetShadowBySchema(schemaID);
}

export async function pinShadowWordForSchema(
  schemaID: string,
  code: string,
  word: string,
  candID: string,
  position: number,
): Promise<void> {
  return App.PinShadowWordForSchema(schemaID, code, word, candID, position);
}

export async function deleteShadowWordForSchema(
  schemaID: string,
  code: string,
  word: string,
  candID: string,
): Promise<void> {
  return App.DeleteShadowWordForSchema(schemaID, code, word, candID);
}

export async function removeShadowRuleForSchema(
  schemaID: string,
  code: string,
  word: string,
  candID: string,
): Promise<void> {
  return App.RemoveShadowRuleForSchema(schemaID, code, word, candID);
}

// Shadow 管理（旧接口保留）
export async function getShadowRules(): Promise<ShadowRuleItem[]> {
  return App.GetShadowRules();
}

export async function pinShadowWord(
  code: string,
  word: string,
  candID: string,
  position: number,
): Promise<void> {
  return App.PinShadowWord(code, word, candID, position);
}

export async function deleteShadowWord(
  code: string,
  word: string,
  candID: string,
): Promise<void> {
  return App.DeleteShadowWord(code, word, candID);
}

export async function removeShadowRule(
  code: string,
  word: string,
  candID: string,
): Promise<void> {
  return App.RemoveShadowRule(code, word, candID);
}

// 服务通信
export async function notifyReload(target: string): Promise<void> {
  return App.NotifyReload(target);
}

export interface PathInfo {
  config_dir: string;
  config_dir_display: string;
  logs_dir: string;
  logs_dir_display: string;
  is_portable: boolean;
}

export async function getPathInfo(): Promise<PathInfo> {
  return App.GetPathInfo();
}

export async function openLogFolder(): Promise<void> {
  return App.OpenLogFolder();
}

export async function openConfigFolder(): Promise<void> {
  return App.OpenConfigFolder();
}

export async function openExternalURL(url: string): Promise<void> {
  return App.OpenExternalURL(url);
}

export async function checkServiceRunning(): Promise<boolean> {
  return App.CheckServiceRunning();
}

export async function getServiceStatus(): Promise<ServiceStatus | null> {
  return App.GetServiceStatus();
}

// ── 性能诊断 API ──

export interface PerfDumpResult {
  path: string;
  count: number;
  summary: string;
}

export interface PerfStatsResult {
  count: number;
  capacity: number;
  summary: string;
}

export async function dumpPerf(
  path: string = "",
  clear: boolean = false,
): Promise<PerfDumpResult> {
  return App.DumpPerf(path, clear) as unknown as PerfDumpResult;
}

export async function getPerfStats(): Promise<PerfStatsResult> {
  return App.GetPerfStats() as unknown as PerfStatsResult;
}

export async function readPerfFile(): Promise<
  PerfDumpResult & { content: string }
> {
  return App.ReadPerfFile() as unknown as PerfDumpResult & { content: string };
}

export async function exportPerfData(): Promise<
  PerfDumpResult & { cancelled: boolean }
> {
  return App.ExportPerfData() as unknown as PerfDumpResult & {
    cancelled: boolean;
  };
}

// ── 内存诊断 ──

export interface MemStatsResult {
  heap_alloc: number;
  heap_sys: number;
  heap_idle: number;
  heap_inuse: number;
  heap_released: number;
  heap_objects: number;
  stack_inuse: number;
  stack_sys: number;
  sys: number;
  num_gc: number;
  pause_total_ns: number;
  gc_sys: number;
  other_sys: number;
}

export interface DumpHeapProfileResult {
  path: string;
  error?: string;
}

export async function getMemStats(): Promise<MemStatsResult> {
  return App.GetMemStats() as unknown as MemStatsResult;
}

export async function dumpHeapProfile(): Promise<DumpHeapProfileResult> {
  return App.DumpHeapProfile() as unknown as DumpHeapProfileResult;
}

export async function dumpGoroutineProfile(): Promise<DumpHeapProfileResult> {
  return App.DumpGoroutineProfile() as unknown as DumpHeapProfileResult;
}

// 重置用户数据
export async function resetUserData(schemaID: string = ""): Promise<void> {
  return App.ResetUserData(schemaID);
}

export async function deleteSchemaData(schemaID: string): Promise<void> {
  return App.DeleteSchemaData(schemaID);
}

// 文件变化检测
export async function reloadAllFiles(): Promise<void> {
  return App.ReloadAllFiles();
}

// 主题管理
export async function getAvailableThemes(): Promise<ThemeInfo[]> {
  return App.GetAvailableThemes();
}

export interface ImportThemeResult {
  success: boolean;
  cancelled: boolean;
  theme_name: string;
  conflict: boolean;
  error_msg: string;
  file_path: string;
}

export async function importThemeFromFile(
  force: boolean,
): Promise<ImportThemeResult> {
  return (window as any).go.main.App.ImportThemeFromFile(
    force,
  ) as ImportThemeResult;
}

export async function importThemeFromFilePath(
  path: string,
  force: boolean,
): Promise<ImportThemeResult> {
  return (window as any).go.main.App.ImportThemeFromFilePath(
    path,
    force,
  ) as ImportThemeResult;
}

export async function importThemeFromText(
  yamlContent: string,
  force: boolean,
): Promise<ImportThemeResult> {
  return (window as any).go.main.App.ImportThemeFromText(
    yamlContent,
    force,
  ) as ImportThemeResult;
}

export async function importThemeFromURL(
  url: string,
  force: boolean,
): Promise<ImportThemeResult> {
  return (window as any).go.main.App.ImportThemeFromURL(
    url,
    force,
  ) as ImportThemeResult;
}

export async function deleteTheme(themeName: string): Promise<void> {
  return (window as any).go.main.App.DeleteTheme(themeName);
}

export async function openThemesFolder(): Promise<void> {
  return (window as any).go.main.App.OpenThemesFolder();
}

export async function getThemePreview(
  themeName: string,
  themeStyle: string = "system",
): Promise<ThemePreview> {
  const preview = await App.GetThemePreview(themeName, themeStyle);
  return preview as unknown as ThemePreview;
}

export async function getSystemFonts(): Promise<SystemFontInfo[]> {
  return (await (
    window as any
  ).go.main.App.GetSystemFonts()) as SystemFontInfo[];
}

// 启动页面
export async function getStartPage(): Promise<string> {
  return App.GetStartPage();
}

// 加词参数
export interface AddWordParams {
  text: string;
  code: string;
  schema_id: string;
}
export async function getAddWordParams(): Promise<AddWordParams> {
  return App.GetAddWordParams();
}

// 版本号
export async function getVersion(): Promise<string> {
  return App.GetVersion();
}

// 运行平台（runtime.GOOS，如 "windows" / "darwin"）
// Wails 环境走 Go 绑定；非 Wails（HTTP 开发模式）回退到 navigator 推断。
export async function getPlatform(): Promise<string> {
  const app = (window as any).go?.main?.App;
  if (app?.GetPlatform) {
    return (await app.GetPlatform()) as string;
  }
  const ua = (
    (navigator as any).userAgentData?.platform ||
    navigator.platform ||
    ""
  ).toLowerCase();
  if (ua.includes("mac")) return "darwin";
  if (ua.includes("win")) return "windows";
  if (ua.includes("linux")) return "linux";
  return "";
}

// 默认配置（从后端获取系统默认值：代码默认 + data/config.toml 合并）
// Go Config 不含 dictionary/engine（由方案单独管理），前端用硬编码默认值补齐
export async function fetchSystemDefaultConfig(): Promise<Config> {
  const goDefaults =
    (await App.GetDefaultConfig()) as unknown as Partial<Config>;
  const httpDefaults = getHTTPDefaultConfig();
  return {
    ...httpDefaults,
    ...goDefaults,
    input: { ...httpDefaults.input, ...goDefaults.input },
  };
}

// 硬编码默认配置（仅用于初始化，后续应使用 fetchSystemDefaultConfig）
export function getDefaultConfig(): Config {
  return getHTTPDefaultConfig();
}

export function getDefaultTSFLogConfig(): TSFLogConfig {
  return getHTTPTSFLogConfig();
}

// 数据目录管理
export interface DataDirInfo {
  current_dir: string;
  size_bytes: number;
  size_text: string;
  file_count: number;
}

export interface DataDirValidation {
  valid: boolean;
  warning: string;
  is_empty: boolean;
  is_same: boolean;
}

export interface ChangeDataDirRequest {
  new_path: string;
  migrate: boolean;
  overwrite: boolean;
  delete_old_data: boolean;
}

export async function getDataDirInfo(): Promise<DataDirInfo> {
  return App.GetDataDirInfo() as unknown as DataDirInfo;
}

export async function validateDataDirPath(
  path: string,
): Promise<DataDirValidation> {
  return App.ValidateDataDirPath(path) as unknown as DataDirValidation;
}

export async function selectDataDir(): Promise<string> {
  return App.SelectDataDir();
}

export interface ChangeDataDirResult {
  success: boolean;
  warnings: string[];
}

export async function changeUserDataDir(
  req: ChangeDataDirRequest,
): Promise<ChangeDataDirResult> {
  return App.ChangeUserDataDir(req) as unknown as ChangeDataDirResult;
}

// ── 统计 API ──

export interface StatsSummary {
  today_chars: number;
  today_chinese: number;
  today_english: number;
  total_chars: number;
  active_days: number;
  daily_avg: number;
  streak_current: number;
  streak_max: number;
  week_chars: number;
  month_chars: number;
  max_day_chars: number;
  max_day_date: string;
  avg_code_len: number;
  first_select_rate: number;
  today_speed: number;
  overall_speed: number;
  max_speed: number;
}

export interface DailyStatItem {
  d: string;
  tc: number;
  cc: number;
  ec: number;
  pc: number;
  oc: number;
  h: number[];
  cn: number;
  cls: number;
  clc: number;
  cld: number[];
  cpd: number[];
  as: number;
  bs?: Record<
    string,
    { tc: number; cn: number; cls: number; clc: number; cpd: number[] }
  >;
  src: number[];
}

export async function getStatsSummary(): Promise<StatsSummary> {
  return App.GetStatsSummary() as unknown as StatsSummary;
}

export async function getDailyStats(
  from: string,
  to: string,
): Promise<DailyStatItem[]> {
  const result = await App.GetDailyStats(from, to);
  return (result as any)?.days || [];
}

export async function clearStats(): Promise<void> {
  return App.ClearStats();
}

export interface StatsPruneResult {
  count: number;
  before: string;
}

export async function clearStatsBefore(
  days: number,
): Promise<StatsPruneResult> {
  return (window as any).go.main.App.ClearStatsBefore(
    days,
  ) as Promise<StatsPruneResult>;
}

// ===== 数据备份/还原/重置 =====

export interface SchemaBackupStats {
  schema_id: string;
  user_word_count: number;
  temp_word_count: number;
  freq_count: number;
  phrase_count: number;
}

export interface BackupPreview {
  schemas: SchemaBackupStats[];
  global_phrases: number;
  theme_count: number;
  stats_days: number;
  estimated_size: number;
}

export interface RestorePreview {
  created_at: string;
  app_version: string;
  data_dir_mode: string;
  schemas: SchemaBackupStats[];
  global_phrases: number;
  theme_count: number;
  stats_days: number;
  total_size: number;
}

export interface BackupPreviewResult {
  preview?: BackupPreview;
  error?: string;
}

export interface RestorePreviewResult {
  cancelled: boolean;
  zip_path?: string;
  preview?: RestorePreview;
  error?: string;
}

export interface BackupResult {
  cancelled: boolean;
  error?: string;
}

export async function getBackupPreview(): Promise<BackupPreviewResult> {
  return App.GetBackupPreview() as unknown as BackupPreviewResult;
}

export async function backupData(): Promise<BackupResult> {
  return App.BackupData() as unknown as BackupResult;
}

export async function getRestorePreview(): Promise<RestorePreviewResult> {
  return App.GetRestorePreview() as unknown as RestorePreviewResult;
}

export async function restoreData(zipPath: string): Promise<string> {
  return App.RestoreData(zipPath) as unknown as string;
}

export async function resetData(): Promise<string> {
  return App.ResetData() as unknown as string;
}

export interface RebuildDictCacheResult {
  deleted: number;
  error?: string;
}

export async function rebuildDictCache(): Promise<RebuildDictCacheResult> {
  return App.RebuildDictCache() as unknown as RebuildDictCacheResult;
}

// ========== 在线主题编辑服务 ==========

export interface ThemeServerStatus {
  running: boolean;
  port: number;
  url: string;
}

export async function startThemeServer(): Promise<ThemeServerStatus> {
  return (window as any).go.main.App.StartThemeServer();
}

export async function stopThemeServer(): Promise<void> {
  return (window as any).go.main.App.StopThemeServer();
}

export async function getThemeServerStatus(): Promise<ThemeServerStatus> {
  return (window as any).go.main.App.GetThemeServerStatus();
}

// ===== URL Schema 协议导入 =====
export interface ProtocolRequest {
  kind: string;
  url: string;
  name?: string;
}
export interface ProtocolImportPayload {
  ok: boolean;
  error?: string;
  request?: ProtocolRequest;
}
export interface ProtocolRegStatus {
  registered: boolean;
  command: string;
  managed: boolean;
}
export interface ThemeURLPreview {
  ok: boolean;
  name: string;
  author: string;
  version: string;
  description: string;
  source_url: string;
  yaml: string;
  error_msg: string;
}

export function consumePendingProtocol(): Promise<ProtocolImportPayload | null> {
  return (window as any).go.main.App.ConsumePendingProtocol();
}
export function getProtocolStatus(): Promise<ProtocolRegStatus> {
  return (window as any).go.main.App.GetProtocolStatus();
}
export function setProtocolRegistered(enabled: boolean): Promise<void> {
  return (window as any).go.main.App.SetProtocolRegistered(enabled);
}
export function previewThemeFromURL(url: string): Promise<ThemeURLPreview> {
  return (window as any).go.main.App.PreviewThemeFromURL(url);
}

// Wails API 封装层 - 使用 Wails 绑定调用 Go 后端

// 导入 Wails 生成的绑定和模型
import * as App from "../../wailsjs/go/main/App";
import { main, rpcapi } from "../../wailsjs/go/models";
import {
  getDefaultConfig as getHTTPDefaultConfig,
  getDefaultTSFLogConfig as getHTTPTSFLogConfig,
  type Config,
  type TSFLogConfig,
} from "./settings";

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
export interface PhraseItem {
  code: string;
  text?: string;
  texts?: string;
  name?: string;
  type: "static" | "dynamic" | "array";
  position: number;
  enabled: boolean;
  is_system: boolean;
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
    full_width_on_bg_color: string;
    full_width_off_bg_color: string;
    full_width_on_color: string;
    full_width_off_color: string;
    punct_chinese_bg_color: string;
    punct_english_bg_color: string;
    punct_chinese_color: string;
    punct_english_color: string;
    settings_bg_color: string;
    settings_icon_color: string;
  };
  style?: {
    index_style: string;
    accent_bar_color: string;
  };
  is_dark?: {
    active: boolean;
  };
}

// 配置管理
export async function getConfig(): Promise<Config> {
  return (await App.GetConfig()) as any;
}

export async function saveConfig(cfg: Config): Promise<void> {
  return App.SaveConfig(cfg as any);
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
  texts: string,
  name: string,
  type: string,
  position: number,
): Promise<void> {
  return App.AddPhrase(code, text, texts, name, type, position);
}

export async function updatePhrase(
  code: string,
  text: string,
  name: string,
  newText: string,
  newPosition: number,
  enabled: boolean | null,
): Promise<void> {
  return App.UpdatePhrase(code, text, name, newText, newPosition, enabled);
}

export async function removePhrase(
  code: string,
  text: string,
  name: string,
): Promise<void> {
  return App.RemovePhrase(code, text, name);
}

export async function setPhraseEnabled(
  code: string,
  text: string,
  name: string,
  enabled: boolean,
): Promise<void> {
  return App.SetPhraseEnabled(code, text, name, enabled);
}

export async function resetPhrasesToDefault(): Promise<void> {
  return App.ResetPhrasesToDefault();
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
export function onDictEvent(callback: (event: DictEvent) => void): void {
  (window as any).runtime.EventsOn("dict-event", callback);
}

export function offDictEvent(): void {
  (window as any).runtime.EventsOff("dict-event");
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

export async function getUserDictBySchema(
  schemaID: string,
): Promise<UserWordItem[]> {
  return App.GetUserDictBySchema(schemaID);
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
  position: number,
): Promise<void> {
  return App.PinShadowWordForSchema(schemaID, code, word, position);
}

export async function deleteShadowWordForSchema(
  schemaID: string,
  code: string,
  word: string,
): Promise<void> {
  return App.DeleteShadowWordForSchema(schemaID, code, word);
}

export async function removeShadowRuleForSchema(
  schemaID: string,
  code: string,
  word: string,
): Promise<void> {
  return App.RemoveShadowRuleForSchema(schemaID, code, word);
}

// Shadow 管理（旧接口保留）
export async function getShadowRules(): Promise<ShadowRuleItem[]> {
  return App.GetShadowRules();
}

export async function pinShadowWord(
  code: string,
  word: string,
  position: number,
): Promise<void> {
  return App.PinShadowWord(code, word, position);
}

export async function deleteShadowWord(
  code: string,
  word: string,
): Promise<void> {
  return App.DeleteShadowWord(code, word);
}

export async function removeShadowRule(
  code: string,
  word: string,
): Promise<void> {
  return App.RemoveShadowRule(code, word);
}

// 服务通信
export async function notifyReload(target: string): Promise<void> {
  return App.NotifyReload(target);
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

export async function getServiceStatus(): Promise<ServiceStatus | null> {
  return App.GetServiceStatus();
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

// 默认配置（从后端获取系统默认值：代码默认 + data/config.yaml 合并）
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

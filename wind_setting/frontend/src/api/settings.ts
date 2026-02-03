// Settings API 调用层

const API_BASE = 'http://127.0.0.1:18923';

// API 响应类型
export interface APIResponse<T = any> {
  success: boolean;
  data?: T;
  error?: string;
}

// 启动/默认状态配置
export interface StartupConfig {
  remember_last_state: boolean;
  default_chinese_mode: boolean;
  default_full_width: boolean;
  default_chinese_punct: boolean;
}

// 词库配置
export interface DictionaryConfig {
  system_dict: string;
  user_dict: string;
  pinyin_dict: string;
}

// 拼音配置
export interface PinyinConfig {
  show_wubi_hint: boolean;
}

// 五笔配置
export interface WubiConfig {
  auto_commit_at_4: boolean;
  clear_on_empty_at_4: boolean;
  top_code_commit: boolean;
  punct_commit: boolean;
}

// 引擎配置
export interface EngineConfig {
  type: string;
  filter_mode: string;
  pinyin: PinyinConfig;
  wubi: WubiConfig;
}

// 快捷键配置
export interface HotkeyConfig {
  toggle_mode_keys: string[];
  commit_on_switch: boolean;
  switch_engine: string;
  toggle_full_width: string;
  toggle_punct: string;
}

// UI配置
export interface UIConfig {
  font_size: number;
  candidates_per_page: number;
  font_path: string;
  inline_preedit: boolean;
  hide_candidate_window: boolean;
  candidate_layout: string;
  status_indicator_duration: number;
  status_indicator_offset_x: number;
  status_indicator_offset_y: number;
}

// 工具栏配置
export interface ToolbarConfig {
  visible: boolean;
  position_x: number;
  position_y: number;
}

// 输入配置
export interface InputConfig {
  full_width: boolean;
  chinese_punctuation: boolean;
  punct_follow_mode: boolean;
  select_key_groups: string[];
  page_keys: string[];
}

// 高级配置
export interface AdvancedConfig {
  log_level: string;
}

// 完整配置
export interface Config {
  startup: StartupConfig;
  dictionary: DictionaryConfig;
  engine: EngineConfig;
  hotkeys: HotkeyConfig;
  ui: UIConfig;
  toolbar: ToolbarConfig;
  input: InputConfig;
  advanced: AdvancedConfig;
}

// 状态类型
export interface ServiceStatus {
  name: string;
  version: string;
  uptime: string;
  uptimeSec: number;
}

export interface EngineStatus {
  type: string;
  displayName: string;
  info: string;
}

export interface MemoryStatus {
  alloc: number;
  sys: number;
  allocMB: string;
  sysMB: string;
}

export interface Status {
  service: ServiceStatus;
  engine: EngineStatus;
  memory: MemoryStatus;
}

// 引擎信息
export interface EngineInfo {
  type: string;
  displayName: string;
  description: string;
  isActive: boolean;
}

// 配置更新响应
export interface ConfigUpdateResponse {
  applied: string[];
  needReload: string[];
  needRestart: boolean;
  conflicts?: string[];
}

// API 调用函数
async function request<T>(method: string, path: string, body?: any): Promise<APIResponse<T>> {
  try {
    const options: RequestInit = {
      method,
      headers: {
        'Content-Type': 'application/json',
      },
    };

    if (body) {
      options.body = JSON.stringify(body);
    }

    const response = await fetch(`${API_BASE}${path}`, options);
    const data = await response.json();
    return data;
  } catch (error) {
    return {
      success: false,
      error: error instanceof Error ? error.message : '网络请求失败',
    };
  }
}

// 健康检查
export async function checkHealth(): Promise<APIResponse> {
  return request('GET', '/api/health');
}

// 获取配置
export async function getConfig(): Promise<APIResponse<Config>> {
  return request('GET', '/api/config');
}

// 更新配置
export async function updateConfig(config: Partial<Config>): Promise<APIResponse<ConfigUpdateResponse>> {
  return request('PATCH', '/api/config', config);
}

// 获取状态
export async function getStatus(): Promise<APIResponse<Status>> {
  return request('GET', '/api/status');
}

// 获取引擎列表
export async function getEngineList(): Promise<APIResponse<{ engines: EngineInfo[]; current: string }>> {
  return request('GET', '/api/engine/list');
}

// 切换引擎
export async function switchEngine(type: string): Promise<APIResponse<{ previous: string; current: string; displayName: string }>> {
  return request('POST', '/api/engine/switch', { type });
}

// 重载配置
export async function reloadConfig(): Promise<APIResponse<{ reloaded: string[]; errors: string[] }>> {
  return request('POST', '/api/config/reload');
}

// 测试转换
export async function testConvert(
  input: string,
  engine: string = 'current',
  filterMode: string = 'current'
): Promise<APIResponse<{ candidates: any[]; engine: string; filterMode: string }>> {
  return request('POST', '/api/test/convert', { input, engine, filterMode });
}

// 日志条目
export interface LogEntry {
  time: string;
  level: string;
  message: string;
}

// 获取日志
export async function getLogs(level: string = 'all', filter: string = ''): Promise<APIResponse<{ logs: LogEntry[]; total: number }>> {
  const params = new URLSearchParams();
  if (level && level !== 'all') params.append('level', level);
  if (filter) params.append('filter', filter);
  const query = params.toString();
  return request('GET', `/api/logs${query ? '?' + query : ''}`);
}

// 清空日志
export async function clearLogs(): Promise<APIResponse> {
  return request('DELETE', '/api/logs');
}

// 默认配置值（用于前端初始化）
export function getDefaultConfig(): Config {
  return {
    startup: {
      remember_last_state: false,
      default_chinese_mode: true,
      default_full_width: false,
      default_chinese_punct: true,
    },
    dictionary: {
      system_dict: 'dict/pinyin/pinyin.txt',
      user_dict: 'user_dict.txt',
      pinyin_dict: 'dict/pinyin/pinyin.txt',
    },
    engine: {
      type: 'pinyin',
      filter_mode: 'smart',
      pinyin: {
        show_wubi_hint: true,
      },
      wubi: {
        auto_commit_at_4: false,
        clear_on_empty_at_4: false,
        top_code_commit: true,
        punct_commit: true,
      },
    },
    hotkeys: {
      toggle_mode_keys: ['lshift', 'rshift'],
      commit_on_switch: true,
      switch_engine: 'ctrl+`',
      toggle_full_width: 'shift+space',
      toggle_punct: 'ctrl+.',
    },
    ui: {
      font_size: 18,
      candidates_per_page: 9,
      font_path: '',
      inline_preedit: true,
      hide_candidate_window: false,
      candidate_layout: 'horizontal',
      status_indicator_duration: 800,
      status_indicator_offset_x: 0,
      status_indicator_offset_y: -30,
    },
    toolbar: {
      visible: false,
      position_x: 0,
      position_y: 0,
    },
    input: {
      full_width: false,
      chinese_punctuation: true,
      punct_follow_mode: false,
      select_key_groups: ['semicolon_quote'],
      page_keys: ['pageupdown', 'minus_equal'],
    },
    advanced: {
      log_level: 'info',
    },
  };
}

// Settings API 调用层

const API_BASE = 'http://127.0.0.1:18923';

// API 响应类型
export interface APIResponse<T = any> {
  success: boolean;
  data?: T;
  error?: string;
}

// 配置类型
export interface GeneralConfig {
  start_in_chinese_mode: boolean;
  log_level: string;
}

export interface DictionaryConfig {
  system_dict: string;
  user_dict: string;
  pinyin_dict: string;
}

export interface PinyinConfig {
  show_wubi_hint: boolean;
}

export interface WubiConfig {
  auto_commit: string;
  empty_code: string;
  top_code_commit: boolean;
  punct_commit: boolean;
}

export interface EngineConfig {
  type: string;
  pinyin: PinyinConfig;
  wubi: WubiConfig;
}

export interface HotkeyConfig {
  toggle_mode: string;
  switch_engine: string;
}

export interface UIConfig {
  font_size: number;
  candidates_per_page: number;
  font_path: string;
}

export interface Config {
  general: GeneralConfig;
  dictionary: DictionaryConfig;
  engine: EngineConfig;
  hotkeys: HotkeyConfig;
  ui: UIConfig;
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

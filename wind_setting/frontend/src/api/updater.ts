import {
  GetPendingUpdate,
  GetUpdateConfig,
  SaveUpdateConfig,
  CheckUpdate,
  StartDownload,
  CancelDownload,
  InstallRelease,
} from '../../wailsjs/go/main/App'
import { EventsOn, EventsOff } from '../../wailsjs/runtime/runtime'

export interface UpdateConfig {
  network_consent: boolean
  auto_check: boolean
  auto_install: boolean
}

export interface CheckResult {
  has_update: boolean
  latest_version: string
  current_version: string
  release_notes: string
  release_url: string
  asset_name: string
  asset_size: number
  download_url: string
}

export interface DownloadProgress {
  downloaded: number
  total: number
  percent: number
}

export async function getPendingUpdate(): Promise<CheckResult | null> {
  return GetPendingUpdate() as Promise<CheckResult | null>
}

export async function getUpdateConfig(): Promise<UpdateConfig> {
  return GetUpdateConfig() as Promise<UpdateConfig>
}

export async function saveUpdateConfig(cfg: UpdateConfig): Promise<void> {
  return SaveUpdateConfig(cfg)
}

export async function checkUpdate(): Promise<CheckResult> {
  return CheckUpdate() as Promise<CheckResult>
}

export function startDownload(downloadURL: string, assetName: string, expectedSize: number): void {
  StartDownload(downloadURL, assetName, expectedSize)
}

export function cancelDownload(): void {
  CancelDownload()
}

export async function installRelease(installerPath: string, silent: boolean): Promise<void> {
  return InstallRelease(installerPath, silent)
}

export function onDownloadProgress(callback: (p: DownloadProgress) => void): void {
  EventsOn('update:progress', callback)
}

export function onDownloadDone(callback: (path: string) => void): void {
  EventsOn('update:done', callback)
}

export function onDownloadError(callback: (msg: string) => void): void {
  EventsOn('update:error', callback)
}

export function onUpdateAvailable(callback: (result: CheckResult) => void): void {
  EventsOn('update:available', callback)
}

export function offUpdateEvents(): void {
  EventsOff('update:progress', 'update:done', 'update:error', 'update:available')
}

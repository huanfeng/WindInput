import { ref } from 'vue'
import { onUpdateAvailable, getPendingUpdate, type CheckResult } from '@/api/updater'

const pendingUpdate = ref<CheckResult | null>(null)
let listenerRegistered = false

export function useUpdate() {
  return { pendingUpdate }
}

export async function initUpdateListener() {
  if (listenerRegistered) return
  listenerRegistered = true

  // 先注册事件，再主动拉取已存结果，消除 Go emit 比 EventsOn 更早的时序竞争
  onUpdateAvailable((result) => {
    pendingUpdate.value = result
  })

  try {
    const result = await getPendingUpdate()
    if (result?.has_update) {
      pendingUpdate.value = result
    }
  } catch {
    // 非 Wails 环境或尚未检查，忽略
  }
}

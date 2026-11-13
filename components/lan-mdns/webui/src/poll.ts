import { onBeforeUnmount, toValue, watch } from 'vue'
import type { MaybeRefOrGetter } from 'vue'

export interface PollingHandle {
  start: () => void
  stop: () => void
  /** One manual refresh that honours the in-flight / hidden-tab guards. */
  tick: () => Promise<void>
}

/**
 * Visibility-aware polling. The console refreshes quietly in the background:
 * no entrance animation is ever triggered by a refresh (PRD 3.7 anti-jitter
 * rule) and polling stops while the tab is hidden or the pane is inactive.
 */
export function usePolling(
  task: () => Promise<void> | void,
  interval: number,
  active?: MaybeRefOrGetter<boolean>,
): PollingHandle {
  let timer: ReturnType<typeof setInterval> | null = null
  let running = false

  const tick = async (): Promise<void> => {
    if (running || document.hidden) return
    running = true
    try {
      await task()
    } catch {
      /* surfaced by the caller's own error state */
    } finally {
      running = false
    }
  }

  const start = (): void => {
    stop()
    void tick()
    timer = setInterval(() => void tick(), interval)
  }

  const stop = (): void => {
    if (timer !== null) {
      clearInterval(timer)
      timer = null
    }
  }

  if (active !== undefined) {
    watch(() => toValue(active), (value) => (value ? start() : stop()), { immediate: true })
  } else {
    start()
  }
  onBeforeUnmount(stop)

  return { start, stop, tick }
}

export function formatUptime(seconds: number): string {
  const total = Math.max(0, Math.floor(Number(seconds) || 0))
  const days = Math.floor(total / 86400)
  const hours = Math.floor((total % 86400) / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const secs = total % 60
  const parts: string[] = []
  if (days) parts.push(`${days} 天`)
  if (days || hours) parts.push(`${hours} 时`)
  parts.push(`${minutes} 分`, `${secs} 秒`)
  return parts.join(' ')
}

export function formatBytes(value: number): string {
  const size = Number(value) || 0
  if (size < 1024) return `${size} B`
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KiB`
  if (size < 1024 * 1024 * 1024) return `${(size / 1024 / 1024).toFixed(1)} MiB`
  return `${(size / 1024 / 1024 / 1024).toFixed(2)} GiB`
}

export function formatTime(seconds: number): string {
  const value = Number(seconds)
  if (!value) return '—'
  return new Date(value * 1000).toLocaleString('zh-CN', { hour12: false })
}

export function formatDate(iso: string): string {
  if (!iso) return '—'
  const parsed = new Date(iso)
  if (Number.isNaN(parsed.getTime())) return iso
  return parsed.toLocaleString('zh-CN', { hour12: false })
}

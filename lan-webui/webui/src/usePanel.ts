/**
 * Shared panel lifecycle for the capability-driven widgets (M7).
 *
 * One composable backs every widget: it fetches the panel's endpoint (text or
 * JSON), exposes the result, and runs an action through the component proxy.
 * Widgets add only their presentation, so behaviour stays identical across all
 * eight module surfaces.
 */
import { onMounted, ref } from 'vue'
import { componentApi, errorMessage } from './api'
import type { CapabilityAction, CapabilityPanel } from './capabilities'

export interface UsePanel {
  data: ReturnType<typeof ref<unknown>>
  error: ReturnType<typeof ref<string>>
  busy: ReturnType<typeof ref<boolean>>
  loaded: ReturnType<typeof ref<boolean>>
  load: () => Promise<void>
  runAction: (action: CapabilityAction, values?: Record<string, string>) => Promise<void>
}

export function usePanel(component: string, panel: CapabilityPanel): UsePanel {
  const api = componentApi(component)
  const data = ref<unknown>(null)
  const error = ref<string>('')
  const busy = ref<boolean>(false)
  const loaded = ref<boolean>(false)

  async function load(): Promise<void> {
    error.value = ''
    busy.value = true
    try {
      if (panel.widget === 'text-view') {
        data.value = await api.text(panel.endpoint.path)
      } else {
        data.value = await api.get<unknown>(panel.endpoint.path)
      }
      loaded.value = true
    } catch (caught) {
      error.value = errorMessage(caught)
      data.value = null
    } finally {
      busy.value = false
    }
  }

  async function runAction(
    action: CapabilityAction,
    values?: Record<string, string>,
  ): Promise<void> {
    error.value = ''
    busy.value = true
    try {
      if (action.method === 'GET') {
        await api.get(action.path)
      } else {
        await api.post(action.path, values ?? {})
      }
      await load()
    } catch (caught) {
      error.value = errorMessage(caught)
      throw caught
    } finally {
      busy.value = false
    }
  }

  onMounted(load)
  return { data, error, busy, loaded, load, runAction }
}

/** Best-effort label from a camel/snake key. */
export function humanizeKey(key: string): string {
  const spaced = key
    .replace(/_/g, ' ')
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .trim()
  return spaced.charAt(0).toUpperCase() + spaced.slice(1)
}

/** Flatten a JSON object into key/value rows for the KvGrid / MetricGrid. */
export function toRows(value: unknown): Array<{ key: string; value: string }> {
  if (!value || typeof value !== 'object') return []
  const out: Array<{ key: string; value: string }> = []
  for (const [key, raw] of Object.entries(value as Record<string, unknown>)) {
    let display: string
    if (raw === null || raw === undefined) display = '—'
    else if (typeof raw === 'object') display = JSON.stringify(raw)
    else display = String(raw)
    out.push({ key: humanizeKey(key), value: display })
  }
  return out
}

/** Pull the first array-valued property (or the value itself) out of a payload. */
export function firstArray(value: unknown): unknown[] {
  if (Array.isArray(value)) return value
  if (value && typeof value === 'object') {
    for (const raw of Object.values(value as Record<string, unknown>)) {
      if (Array.isArray(raw)) return raw
    }
  }
  return []
}

/** Pull a string list out of a payload (`logs` key, else the first array). */
export function toLines(value: unknown): string[] {
  if (Array.isArray(value)) {
    return value.map((item) => (typeof item === 'string' ? item : JSON.stringify(item)))
  }
  if (value && typeof value === 'object') {
    const obj = value as Record<string, unknown>
    if (Array.isArray(obj['logs'])) {
      return (obj['logs'] as unknown[]).map((item) =>
        typeof item === 'string' ? item : JSON.stringify(item),
      )
    }
    return firstArray(value).map((item) =>
      typeof item === 'string' ? item : JSON.stringify(item),
    )
  }
  return []
}

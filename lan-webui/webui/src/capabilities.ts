/**
 * Capability contract of the unified console (M7).
 *
 * The backend (`lan-webui`) injects `window.__LAN_CAPABILITIES__` into the page
 * at serve time — the aggregated manifest built from `modules.yaml` + the
 * per-component `capabilities.<name>.sample.json`. The frontend renders ONLY
 * what the manifest declares; a module the backend does not list never reaches
 * the DOM. When the page is opened standalone (no backend injection, e.g. the
 * offline render-check or a direct file open) a committed copy of the aggregate
 * is used as a fallback so the SPA is never blank.
 *
 * `getManifest()` returns the injected manifest when present, else the fallback.
 * `visibleModules()` drops any module gated behind a `requires_lib` the current
 * build does not ship.
 */
import aggregateRaw from '../../capabilities.json?raw'

export type WidgetKind =
  | 'kv-grid'
  | 'table'
  | 'form'
  | 'code-editor'
  | 'metric-grid'
  | 'log-list'
  | 'text-view'
  | 'action-list'

export interface CapabilityColumn {
  key: string
  label: string
}

export interface CapabilityActionField {
  key: string
  label: string
  type?: string
  required?: boolean
}

export interface CapabilityAction {
  id: string
  label: string
  method: string
  path: string
  write?: boolean
  fields?: CapabilityActionField[]
}

export interface CapabilityPanel {
  id: string
  title: string
  widget: WidgetKind
  endpoint: { method: string; path: string }
  columns?: CapabilityColumn[]
  actions?: CapabilityAction[]
}

export interface CapabilityModule {
  id: string
  title: string
  icon: string
  tone: string
  area: string
  requires_lib?: string
  panels: CapabilityPanel[]
}

export interface CapabilityComponent {
  name: string
  label: string
  port: number
  icon: string
  tone: string
  modules: CapabilityModule[]
}

export interface CapabilityManifest {
  schema: number
  component: string
  runtime: string
  libs: string[]
  components: CapabilityComponent[]
}

const FALLBACK: CapabilityManifest = JSON.parse(aggregateRaw) as CapabilityManifest

/** Injected manifest wins; committed aggregate is the safety net. */
export function getManifest(): CapabilityManifest {
  const injected = (window as unknown as { __LAN_CAPABILITIES__?: unknown }).__LAN_CAPABILITIES__
  if (injected) {
    if (typeof injected === 'string') {
      try {
        return JSON.parse(injected) as CapabilityManifest
      } catch {
        return FALLBACK
      }
    }
    return injected as CapabilityManifest
  }
  return FALLBACK
}

/** Drop modules gated behind a `requires_lib` the build does not ship. */
export function visibleModules(
  manifest: CapabilityManifest,
  component: CapabilityComponent,
): CapabilityModule[] {
  if (!component.modules) return []
  return component.modules.filter(
    (module) => !module.requires_lib || manifest.libs.includes(module.requires_lib),
  )
}

/** Whether the active manifest would render a given component at all. */
export function hasComponent(manifest: CapabilityManifest, name: string): boolean {
  return manifest.components.some((component) => component.name === name)
}

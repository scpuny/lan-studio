// Canonical YAML round-trip for the configuration form (F-E04).
//
// The backend's parser (`common-config::parse_flat`) accepts a small YAML
// subset: nested block mappings, `- key: value` object lists and `- scalar`
// lists, quoted or plain scalars, `[]` for an empty list, `#` comments.
// This module mirrors exactly that subset so form mode and source mode always
// describe the same document. Keep the two implementations in sync.

import type {
  AlertsModel,
  ConfigModel,
  InterfaceMode,
  LogLevel,
  RecordModel,
  RecordType,
  ServiceModel,
} from './types'

const IDENT = /^[A-Za-z0-9_.-]+$/
const PLAIN = /^[A-Za-z0-9_./=:@+-]+$/
const SCALARISH = /^(true|false|null|yes|no|on|off|-?\d+(\.\d+)?)$/i

export const LEVELS: readonly LogLevel[] = ['debug', 'info', 'warning', 'error']
export const RECORD_TYPES: readonly RecordType[] = ['A', 'AAAA', 'PTR', 'SRV', 'TXT']
export const INTERFACE_MODES: readonly InterfaceMode[] = ['auto', 'all', 'specified']

export function isLogLevel(value: string): value is LogLevel {
  return (LEVELS as readonly string[]).includes(value)
}

export function isRecordType(value: string): value is RecordType {
  return (RECORD_TYPES as readonly string[]).includes(value)
}

export function isInterfaceMode(value: string): value is InterfaceMode {
  return (INTERFACE_MODES as readonly string[]).includes(value)
}

/** One open indentation level while flattening. */
interface Frame {
  indent: number
  prefix: string
  nextIndex: number
}

function top(frames: readonly Frame[]): Frame {
  const frame = frames[frames.length - 1]
  if (!frame) throw new Error('flatten: empty frame stack')
  return frame
}

function stripComment(line: string): string {
  let inSingle = false
  let inDouble = false
  for (let i = 0; i < line.length; i += 1) {
    const ch = line.charAt(i)
    if (ch === "'" && !inDouble) inSingle = !inSingle
    else if (ch === '"' && !inSingle) inDouble = !inDouble
    else if (ch === '#' && !inSingle && !inDouble) return line.slice(0, i)
  }
  return line
}

function unquote(value: string): string {
  const text = value.trim()
  if (text.length >= 2) {
    const first = text.charAt(0)
    const last = text.charAt(text.length - 1)
    if ((first === '"' && last === '"') || (first === "'" && last === "'")) {
      return text.slice(1, -1)
    }
  }
  return text
}

/** Flatten a YAML subset document into dotted keys, exactly like the backend. */
export function flatten(text: string): Map<string, string> {
  const out = new Map<string, string>()
  const stack: Frame[] = [{ indent: 0, prefix: '', nextIndex: 0 }]

  for (const rawLine of text.split(/\r?\n/)) {
    const line = stripComment(rawLine).replace(/\s+$/, '')
    if (!line.trim()) continue
    const indent = line.length - line.trimStart().length
    const body = line.trim()
    while (stack.length > 1 && indent <= top(stack).indent) stack.pop()
    const parent = top(stack).prefix

    if (body.startsWith('- ')) {
      const item = body.slice(2)
      const frame = top(stack)
      const index = frame.nextIndex
      frame.nextIndex += 1
      const itemPrefix = `${frame.prefix}[${index}]`
      const colon = item.indexOf(':')
      let mapping: { key: string; rest: string } | null = null
      if (colon > 0) {
        const key = item.slice(0, colon).trim()
        const rest = item.slice(colon + 1)
        if (IDENT.test(key) && (rest === '' || rest.startsWith(' '))) {
          mapping = { key, rest: rest.trim() }
        }
      }
      if (mapping) {
        const full = `${itemPrefix}.${mapping.key}`
        if (mapping.rest === '') {
          stack.push({ indent, prefix: full, nextIndex: 0 })
          continue
        }
        out.set(full, unquote(mapping.rest))
      } else {
        out.set(itemPrefix, unquote(item))
      }
      stack.push({ indent, prefix: itemPrefix, nextIndex: 0 })
      continue
    }

    const colon = body.indexOf(':')
    if (colon > 0) {
      const key = body.slice(0, colon).trim()
      const rest = body.slice(colon + 1).trim()
      const full = parent ? `${parent}.${key}` : key
      if (rest === '') stack.push({ indent, prefix: full, nextIndex: 0 })
      else if (rest === '[]' || rest === '{}') out.set(full, '')
      else out.set(full, unquote(rest))
    }
  }
  return out
}

function quote(value: string): string {
  if (value === '') return '""'
  if (SCALARISH.test(value) || !PLAIN.test(value)) {
    if (!value.includes("'")) return `'${value}'`
    return `"${value.replace(/"/g, '\\"')}"`
  }
  return value
}

function pushScalarList(lines: string[], key: string, values: readonly string[], indent: number): void {
  const pad = ' '.repeat(indent)
  if (values.length === 0) {
    lines.push(`${pad}${key}: []`)
    return
  }
  lines.push(`${pad}${key}:`)
  for (const value of values) lines.push(`${pad}  - ${quote(value)}`)
}

function num(value: number, fallback: number): number {
  return Number.isFinite(value) ? value : fallback
}

/** Canonical document for the whole model. Comments are intentionally dropped. */
export function serializeConfig(model: ConfigModel): string {
  const { server, observability, logging, alerts, mdns } = model
  const lines: string[] = []

  lines.push(`version: ${num(model.version, 1)}`)
  lines.push(`component: ${quote(model.component)}`)
  lines.push('')
  lines.push('server:')
  lines.push(`  listen: ${quote(server.listen)}`)
  lines.push(`  port: ${num(server.port, 8080)}`)
  lines.push(`  webui: ${server.webui ? 'true' : 'false'}`)
  lines.push(`  admin_token: ${quote(server.adminToken)}`)
  lines.push(`  log_level: ${quote(server.logLevel)}`)
  lines.push('')
  lines.push('observability:')
  lines.push(`  metrics: ${observability.metrics ? 'true' : 'false'}`)
  lines.push(`  alerts: ${observability.alerts ? 'true' : 'false'}`)
  lines.push(`  history_limit: ${num(observability.historyLimit, 30)}`)
  lines.push('')
  lines.push('logging:')
  lines.push(`  max_bytes: ${num(logging.maxBytes, 1048576)}`)
  lines.push(`  backup_count: ${num(logging.backupCount, 3)}`)
  lines.push(`  level: ${quote(logging.level)}`)
  lines.push('')
  lines.push('alerts:')
  lines.push(`  enabled: ${alerts.enabled ? 'true' : 'false'}`)
  pushScalarList(lines, 'webhooks', alerts.webhooks.filter(Boolean), 2)
  lines.push(`  cooldown_seconds: ${num(alerts.cooldownSeconds, 300)}`)
  lines.push(`  timeout_seconds: ${num(alerts.timeoutSeconds, 5)}`)
  lines.push(`  max_retries: ${num(alerts.maxRetries, 2)}`)
  lines.push('')
  lines.push('mdns:')
  lines.push(`  group: ${quote(mdns.group)}`)
  lines.push(`  port: ${num(mdns.port, 5353)}`)
  lines.push(`  ttl: ${num(mdns.ttl, 120)}`)
  lines.push(`  announce_count: ${num(mdns.announceCount, 3)}`)
  lines.push(`  announce_interval_ms: ${num(mdns.announceIntervalMs, 250)}`)
  lines.push(`  announce_period_s: ${num(mdns.announcePeriodS, 60)}`)
  lines.push(`  reverse_ptr: ${mdns.reversePtr ? 'true' : 'false'}`)
  lines.push(`  interface_mode: ${quote(mdns.interfaceMode)}`)
  // Changing the interface set rebuilds the multicast socket, so the list is
  // only meaningful for `specified`; `auto`/`all` ignore it (F-A12).
  pushScalarList(lines, 'interfaces', mdns.interfaces, 2)

  if (mdns.healthTargets.length === 0) {
    lines.push('  health_targets: []')
  } else {
    lines.push('  health_targets:')
    for (const target of mdns.healthTargets) {
      lines.push(`    - alias: ${quote(target.alias)}`)
      lines.push(`      host: ${quote(target.host)}`)
      lines.push(`      port: ${num(target.port, 0)}`)
    }
  }

  if (mdns.records.length === 0) {
    lines.push('  records: []')
  } else {
    lines.push('  records:')
    for (const record of mdns.records) {
      lines.push(`    - name: ${quote(record.name)}`)
      lines.push(`      type: ${quote(record.type)}`)
      lines.push(`      value: ${quote(record.value)}`)
      if (num(record.ttl, 0) > 0) lines.push(`      ttl: ${num(record.ttl, 0)}`)
    }
  }

  if (mdns.services.length === 0) {
    lines.push('  services: []')
  } else {
    lines.push('  services:')
    for (const service of mdns.services) {
      lines.push(`    - instance: ${quote(service.instance)}`)
      lines.push(`      type: ${quote(service.type)}`)
      lines.push(`      host: ${quote(service.host)}`)
      lines.push(`      port: ${num(service.port, 80)}`)
      lines.push(`      proxy: ${service.proxy ? 'true' : 'false'}`)
      const txt = service.txt.filter(Boolean)
      if (txt.length === 0) {
        lines.push('      txt: {}')
      } else {
        lines.push('      txt:')
        for (let index = 0; index < txt.length; index += 1) {
          const entry = txt[index] ?? ''
          const at = entry.indexOf('=')
          const key = at > 0 ? entry.slice(0, at) : `key${index}`
          const value = at > 0 ? entry.slice(at + 1) : ''
          lines.push(`        ${key}: ${quote(value)}`)
        }
      }
    }
  }

  lines.push('  gateway:')
  lines.push(`    enabled: ${mdns.gateway.enabled ? 'true' : 'false'}`)
  lines.push(`    port: ${num(mdns.gateway.port, 80)}`)
  lines.push(`    listen: ${quote(mdns.gateway.listen)}`)
  lines.push(`    admin_host: ${quote(mdns.gateway.adminHost)}`)
  lines.push(`    timeout_seconds: ${num(mdns.gateway.timeoutSeconds, 6)}`)
  if (mdns.gateway.hosts.length === 0) {
    lines.push('    hosts: []')
  } else {
    lines.push('    hosts:')
    for (const host of mdns.gateway.hosts) {
      lines.push(`      - host: ${quote(host.host)}`)
      lines.push(`        target: ${quote(host.target)}`)
      lines.push(`        proxy: ${host.proxy ? 'true' : 'false'}`)
    }
  }

  return `${lines.join('\n')}\n`
}

function flag(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined || value === '') return fallback
  return value === 'true' || value === '1' || value.toLowerCase() === 'true'
}

/** Build the form model from a flat map; mirrors the server's view builder. */
export function modelFromFlat(flat: ReadonlyMap<string, string>): ConfigModel {
  const str = (key: string, fallback = ''): string => {
    const value = flat.get(key)
    return value === undefined || value === '' ? fallback : value
  }
  const int = (key: string, fallback = 0): number => {
    const parsed = Number.parseInt(str(key), 10)
    return Number.isFinite(parsed) ? parsed : fallback
  }
  const float = (key: string, fallback: number): number => {
    const parsed = Number(str(key))
    return Number.isFinite(parsed) ? parsed : fallback
  }
  const level = (key: string, fallback: LogLevel): LogLevel => {
    const value = str(key, fallback)
    return isLogLevel(value) ? value : fallback
  }
  // Same derivation as `common_config::list_len`: scan `prefix[i]...` keys so
  // object lists, scalar lists and mixed lists all enumerate correctly.
  const list = (prefix: string): number[] => {
    const marker = `${prefix}[`
    const indices = new Set<number>()
    for (const key of flat.keys()) {
      if (!key.startsWith(marker)) continue
      const rest = key.slice(marker.length)
      const end = rest.indexOf(']')
      if (end <= 0) continue
      const index = Number.parseInt(rest.slice(0, end), 10)
      if (Number.isFinite(index)) indices.add(index)
    }
    return [...indices].sort((a, b) => a - b)
  }
  const slice = (prefix: string): string[] => {
    const values: string[] = []
    for (const index of list(prefix)) values.push(str(`${prefix}[${index}]`))
    return values
  }

  const records: RecordModel[] = list('mdns.records').map((index) => {
    const base = `mdns.records[${index}]`
    const type = str(`${base}.type`, 'A')
    return {
      name: str(`${base}.name`),
      type: isRecordType(type) ? type : 'A',
      value: str(`${base}.value`),
      ttl: int(`${base}.ttl`, 0),
    }
  })

  const services: ServiceModel[] = list('mdns.services').map((index) => {
    const base = `mdns.services[${index}]`
    const txtPrefix = `${base}.txt.`
    const txt: string[] = []
    for (const [key, value] of flat.entries()) {
      if (key.startsWith(txtPrefix)) txt.push(`${key.slice(txtPrefix.length)}=${value}`)
    }
    txt.sort()
    return {
      instance: str(`${base}.instance`),
      type: str(`${base}.type`),
      host: str(`${base}.host`),
      port: int(`${base}.port`, 80),
      proxy: flag(flat.get(`${base}.proxy`), true),
      txt,
    }
  })

  const healthTargets = list('mdns.health_targets').map((index) => {
    const base = `mdns.health_targets[${index}]`
    return {
      alias: str(`${base}.alias`),
      host: str(`${base}.host`),
      port: int(`${base}.port`, 0),
    }
  })

  const gatewayHosts = list('mdns.gateway.hosts').map((index) => {
    const base = `mdns.gateway.hosts[${index}]`
    return {
      host: str(`${base}.host`),
      target: str(`${base}.target`),
      proxy: flag(flat.get(`${base}.proxy`), true),
    }
  })

  const alerts: AlertsModel = {
    enabled: flag(flat.get('alerts.enabled'), true),
    webhooks: slice('alerts.webhooks').filter(Boolean),
    cooldownSeconds: int('alerts.cooldown_seconds', 300),
    timeoutSeconds: float('alerts.timeout_seconds', 5),
    maxRetries: int('alerts.max_retries', 2),
  }

  const mode = str('mdns.interface_mode', 'auto')

  return {
    version: int('version', 1),
    component: str('component', 'lan-mdns'),
    server: {
      listen: str('server.listen', '127.0.0.1'),
      port: int('server.port', 8080),
      webui: flag(flat.get('server.webui'), true),
      // The token lives in the same document the source tab shows, so the form
      // must round-trip it verbatim or saving would silently drop it.
      adminToken: str('server.admin_token', ''),
      logLevel: level('server.log_level', 'info'),
    },
    observability: {
      metrics: flag(flat.get('observability.metrics'), true),
      alerts: flag(flat.get('observability.alerts'), true),
      historyLimit: int('observability.history_limit', 30),
    },
    logging: {
      maxBytes: int('logging.max_bytes', 1048576),
      backupCount: int('logging.backup_count', 3),
      level: level('logging.level', 'info'),
    },
    alerts,
    mdns: {
      group: str('mdns.group', '224.0.0.251'),
      port: int('mdns.port', 5353),
      ttl: int('mdns.ttl', 120),
      announceCount: int('mdns.announce_count', 3),
      announceIntervalMs: int('mdns.announce_interval_ms', 250),
      announcePeriodS: int('mdns.announce_period_s', 60),
      reversePtr: flag(flat.get('mdns.reverse_ptr'), true),
      interfaceMode: isInterfaceMode(mode) ? mode : 'auto',
      interfaces: slice('mdns.interfaces').filter(Boolean),
      records,
      services,
      healthTargets,
      gateway: {
        enabled: flag(flat.get('mdns.gateway.enabled'), false),
        port: int('mdns.gateway.port', 80),
        listen: str('mdns.gateway.listen', '0.0.0.0'),
        adminHost: str('mdns.gateway.admin_host', 'mdns.local'),
        timeoutSeconds: float('mdns.gateway.timeout_seconds', 6),
        hosts: gatewayHosts,
      },
    },
  }
}

/** Best-effort parse of arbitrary source text into the form model. */
export function parseConfigText(text: string): ConfigModel | null {
  const flat = flatten(text)
  if (flat.size === 0) return null
  return modelFromFlat(flat)
}

export function emptyModel(): ConfigModel {
  return modelFromFlat(new Map<string, string>())
}

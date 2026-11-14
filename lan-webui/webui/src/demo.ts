/**
 * Demo mode — `?demo=1` only.
 *
 * Installs a global `fetch` + `EventSource` shim so the console renders with
 * realistic fixtures instead of a live backend. This is purely for visual
 * review: pages and the `api` layer are untouched, and the shim is a no-op
 * unless the URL carries `?demo=1`, so production builds and the jsdom
 * render-check are unaffected.
 */

const now = (): number => Math.floor(Date.now() / 1000)

interface DemoRoute {
  /** Upper-cased HTTP method this route answers. */
  method: string
  /** True when the (possibly query-bearing) path belongs to this route. */
  match: (path: string) => boolean
  /** Returns the `data` payload; the envelope `{ ok, data, request_id }` is added by the shim. */
  body: () => unknown
  /** When true, the response body is returned as raw text (metrics / CSV export). */
  text?: boolean
}

/* --------------------------------------------------------------- fixtures */

const RAW_CONFIG = `version: 1
component: lan-mdns
server:
  listen: "127.0.0.1"
  port: 8080
  webui: true
  log_level: info
observability:
  metrics: true
  alerts: true
  history_limit: 50
logging:
  max_bytes: 1048576
  backup_count: 3
  level: info
alerts:
  enabled: true
  webhooks:
    - "https://hooks.example.com/lan-studio/a"
    - "https://hooks.example.com/lan-studio/b"
  cooldown_seconds: 300
  timeout_seconds: 5
  max_retries: 3
mdns:
  group: 224.0.0.251
  port: 5353
  ttl: 4500
  announce_count: 3
  announce_interval_ms: 1000
  announce_period_s: 60
  reverse_ptr: true
  interface_mode: auto
  records:
    - { name: nas.local, type: A, value: 192.168.1.20, ttl: 4500 }
    - { name: grafana.local, type: A, value: 192.168.1.30, ttl: 4500 }
    - { name: printer.local, type: A, value: 192.168.1.40, ttl: 4500 }
    - { name: ci.local, type: A, value: 192.168.1.50, ttl: 4500 }
    - { name: "_http._tcp.local", type: PTR, value: "nas._http._tcp.local", ttl: 4500 }
    - { name: "_airplay._tcp.local", type: PTR, value: "tv._airplay._tcp.local", ttl: 4500 }
  services:
    - { instance: nas, type: "_http._tcp.local", host: nas.local, port: 5000, proxy: false, txt: ["path=/", "model=Synology"] }
    - { instance: grafana, type: "_http._tcp.local", host: grafana.local, port: 3000, proxy: false, txt: ["path=/dashboards"] }
    - { instance: ci, type: "_http._tcp.local", host: ci.local, port: 8080, proxy: true, txt: ["vcs=git"] }
  health_targets:
    - { alias: "NAS 存储", host: nas.local, port: 5000 }
    - { alias: "监控面板", host: grafana.local, port: 3000 }
    - { alias: "CI 网关", host: ci.local, port: 8080 }
    - { alias: "旧打印机", host: printer.local, port: 9100 }
  gateway:
    enabled: true
    port: 80
    listen: "0.0.0.0"
    admin_host: mdns.local
    timeout_seconds: 5
    hosts:
      - { host: nas.local, target: "192.168.1.20:5000", proxy: false }
      - { host: grafana.local, target: "192.168.1.30:3000", proxy: false }
      - { host: ci.local, target: "192.168.1.50:8080", proxy: true }
`

function structuredConfig(): unknown {
  return {
    version: 1,
    component: 'lan-mdns',
    server: {
      listen: '127.0.0.1',
      port: 8080,
      webui: true,
      admin_token_set: true,
      log_level: 'info',
    },
    observability: { metrics: true, alerts: true, history_limit: 50 },
    logging: { max_bytes: 1048576, backup_count: 3, level: 'info' },
    alerts: {
      enabled: true,
      webhooks: [
        'https://hooks.example.com/lan-studio/a',
        'https://hooks.example.com/lan-studio/b',
      ],
      cooldown_seconds: 300,
      timeout_seconds: 5,
      max_retries: 3,
    },
    mdns: {
      group: '224.0.0.251',
      port: 5353,
      ttl: 4500,
      announce_count: 3,
      announce_interval_ms: 1000,
      announce_period_s: 60,
      reverse_ptr: true,
      interface_mode: 'auto',
      interfaces: [],
      records: [
        { name: 'nas.local', type: 'A', value: '192.168.1.20', ttl: 4500 },
        { name: 'grafana.local', type: 'A', value: '192.168.1.30', ttl: 4500 },
        { name: 'printer.local', type: 'A', value: '192.168.1.40', ttl: 4500 },
        { name: 'ci.local', type: 'A', value: '192.168.1.50', ttl: 4500 },
        { name: '_http._tcp.local', type: 'PTR', value: 'nas._http._tcp.local', ttl: 4500 },
        {
          name: '_airplay._tcp.local',
          type: 'PTR',
          value: 'tv._airplay._tcp.local',
          ttl: 4500,
        },
      ],
      services: [
        {
          instance: 'nas',
          type: '_http._tcp.local',
          host: 'nas.local',
          port: 5000,
          proxy: false,
          txt: ['path=/', 'model=Synology'],
        },
        {
          instance: 'grafana',
          type: '_http._tcp.local',
          host: 'grafana.local',
          port: 3000,
          proxy: false,
          txt: ['path=/dashboards'],
        },
        {
          instance: 'ci',
          type: '_http._tcp.local',
          host: 'ci.local',
          port: 8080,
          proxy: true,
          txt: ['vcs=git'],
        },
      ],
      health_targets: [
        { alias: 'NAS 存储', host: 'nas.local', port: 5000 },
        { alias: '监控面板', host: 'grafana.local', port: 3000 },
        { alias: 'CI 网关', host: 'ci.local', port: 8080 },
        { alias: '旧打印机', host: 'printer.local', port: 9100 },
      ],
      gateway: {
        enabled: true,
        port: 80,
        listen: '0.0.0.0',
        admin_host: 'mdns.local',
        timeout_seconds: 5,
        hosts: [
          { host: 'nas.local', target: '192.168.1.20:5000', proxy: false },
          { host: 'grafana.local', target: '192.168.1.30:3000', proxy: false },
          { host: 'ci.local', target: '192.168.1.50:8080', proxy: true },
        ],
      },
    },
  }
}

function status(): unknown {
  return {
    component: 'lan-mdns',
    version: '0.4.2',
    running: true,
    uptime: 273600,
    protocol: 'mDNS / DNS-SD',
    records: 6,
    group: '224.0.0.251',
    mdns_port: 5353,
    interface_mode: 'auto',
    queries: 48213,
    responses: 47190,
    ingested: 128,
    announces: 36,
    malformed: 7,
    bytes_in: 9123456,
    bytes_out: 8456789,
    security: { token_required: false, password_set: true, write_protection: true },
    history: { versions: 9, limit: 50 },
    health: { up: 3, down: 1, unknown: 0 },
    conflicts: 2,
    config_saves: 11,
    config_reloads: 4,
    alerts: { enabled: true, webhooks: 2, queued: 0, sent: 18, failed: 1 },
    gateway: { enabled: true, port: 80, routes: 3, requests: 1204, forwarded: 1190 },
    logs: { buffered: 256, capacity: 1000, disk_bytes: 524288, disk_limit_bytes: 5242880 },
  }
}

function healthTargets(): unknown {
  const t = now()
  return {
    up: 3,
    down: 1,
    unknown: 0,
    total: 4,
    targets: [
      { alias: 'NAS 存储', host: 'nas.local', port: 5000, state: 'up', checked_at: t - 12, latency_ms: 1.3 },
      { alias: '监控面板', host: 'grafana.local', port: 3000, state: 'up', checked_at: t - 10, latency_ms: 4.8 },
      { alias: 'CI 网关', host: 'ci.local', port: 8080, state: 'up', checked_at: t - 8, latency_ms: 2.1 },
      { alias: '旧打印机', host: 'printer.local', port: 9100, state: 'down', checked_at: t - 30, latency_ms: 0 },
    ],
  }
}

function records(): unknown {
  return {
    total: 6,
    records: [
      { name: 'nas.local', type: 'A', ttl: 4500, value: '192.168.1.20', cache_flush: false },
      { name: 'grafana.local', type: 'A', ttl: 4500, value: '192.168.1.30', cache_flush: false },
      { name: 'printer.local', type: 'A', ttl: 4500, value: '192.168.1.40', cache_flush: false },
      { name: 'ci.local', type: 'A', ttl: 4500, value: '192.168.1.50', cache_flush: false },
      { name: '_http._tcp.local', type: 'PTR', ttl: 4500, value: 'nas._http._tcp.local', cache_flush: false },
      {
        name: '_airplay._tcp.local',
        type: 'PTR',
        ttl: 4500,
        value: 'tv._airplay._tcp.local',
        cache_flush: false,
      },
    ],
  }
}

function browse(): unknown {
  const t = now()
  return {
    total: 6,
    service_types: 2,
    browse: [
      { ts: t - 4, source: '192.168.1.20', name: 'nas', type: '_http._tcp.local', value: 'nas.local.:5000', ttl: 4500 },
      { ts: t - 4, source: '192.168.1.30', name: 'grafana', type: '_http._tcp.local', value: 'grafana.local.:3000', ttl: 4500 },
      { ts: t - 4, source: '192.168.1.50', name: 'ci', type: '_http._tcp.local', value: 'ci.local.:8080', ttl: 4500 },
      { ts: t - 9, source: '192.168.1.60', name: 'tv', type: '_airplay._tcp.local', value: 'tv.local.:7000', ttl: 4500 },
    ],
    services: [
      {
        service_type: '_http._tcp.local',
        count: 3,
        instances: [
          { name: 'nas', value: 'nas.local.:5000', source: 'self', ts: t - 4 },
          { name: 'grafana', value: 'grafana.local.:3000', source: 'self', ts: t - 4 },
          { name: 'ci', value: 'ci.local.:8080', source: 'self', ts: t - 4 },
        ],
      },
      {
        service_type: '_airplay._tcp.local',
        count: 1,
        instances: [{ name: 'tv', value: 'tv.local.:7000', source: '192.168.1.60', ts: t - 9 }],
      },
    ],
  }
}

function queries(): unknown {
  const t = now()
  const names = [
    'nas.local',
    'grafana.local',
    'printer.local',
    'ci.local',
    '_http._tcp.local',
    'tv.local',
    '_airplay._tcp.local',
    'router.local',
  ]
  const types = ['A', 'AAAA', 'PTR', 'SRV', 'TXT']
  const sources = ['192.168.1.10', '192.168.1.11', '192.168.1.22', '192.168.1.35', '192.168.1.42']
  const rows = Array.from({ length: 24 }, (_, i) => ({
    ts: t - i * 7,
    source: sources[i % sources.length]!,
    name: names[i % names.length]!,
    qtype: types[i % types.length]!,
  }))
  return { total: 48213, limit: 200, queries: rows }
}

function conflicts(): unknown {
  return {
    total: 2,
    conflicts: [
      {
        name: 'printer.local',
        type: 'A',
        rtype: 1,
        values: ['192.168.1.40', '192.168.1.41'],
        sources: ['192.168.1.40', '192.168.1.41'],
      },
      {
        name: '_http._tcp.local',
        type: 'PTR',
        rtype: 12,
        values: ['nas._http._tcp.local', 'oldnas._http._tcp.local'],
        sources: ['192.168.1.20', '192.168.1.55'],
      },
    ],
  }
}

function alerts(): unknown {
  return {
    enabled: true,
    webhooks: 2,
    queue_capacity: 100,
    queued: 0,
    sent: 18,
    failed: 1,
    dropped: 0,
    suppressed: 0,
    retries: 2,
    cooldown_seconds: 300,
    by_kind: [
      { kind: 'health_target', result: 'sent', count: 12 },
      { kind: 'health_target', result: 'failed', count: 1 },
      { kind: 'conflict', result: 'sent', count: 6 },
    ],
    gateway: { enabled: true, routes: 3, requests: 1204, forwarded: 1190, unreachable: 14 },
  }
}

function interfaces(): unknown {
  return {
    interfaces: [
      { name: 'lo', addr: '127.0.0.1', joined: true },
      { name: 'eth0', addr: '192.168.1.5', joined: true },
      { name: 'wlan0', addr: '192.168.1.7', joined: false },
    ],
    total: 3,
    mode: 'auto',
    selected: ['192.168.1.5'],
    default_interface: 'eth0',
    restart_required: false,
  }
}

function logs(): unknown {
  const t = now()
  const mk = (level: string, source: string, msg: string, extra?: Record<string, unknown>): string =>
    JSON.stringify({ ts: t - Math.floor(Math.random() * 600), level, source, msg, extra })
  const lines = [
    mk('info', 'engine', 'mDNS 引擎已启动', { group: '224.0.0.251:5353' }),
    mk('info', 'engine', '已加入组播组', { interfaces: ['eth0'] }),
    mk('info', 'state', '配置已加载', { saves: 11, reloads: 4 }),
    mk('info', 'state', '主动宣告已发出', { count: 3 }),
    mk('debug', 'engine', '收到查询', { name: 'nas.local', qtype: 'A' }),
    mk('debug', 'engine', '发出应答', { name: 'nas.local', value: '192.168.1.20' }),
    mk('warning', 'health', '健康目标探测失败', { alias: '旧打印机', host: 'printer.local:9100' }),
    mk('warning', 'conflict', '检测到名称冲突', { name: 'printer.local', sources: 2 }),
    mk('info', 'gateway', '网关请求转发', { host: 'nas.local', target: '192.168.1.20:5000' }),
    mk('error', 'alert', 'Webhook 投递失败', { webhook: 'https://hooks.example.com/b', attempt: 3 }),
    mk('info', 'state', '配置已保存', { snapshot: 'snap-2026-11-13-08' }),
  ]
  return {
    logs: lines,
    total: 256,
    capacity: 1000,
    disk_bytes: 524288,
    disk_limit_bytes: 5242880,
  }
}

function history(): unknown {
  const reasons = [
    'manual',
    'import',
    'api-save',
    'rollback',
    'manual',
    'api-save',
    'manual',
    'import',
    'manual',
  ]
  const entries = Array.from({ length: 9 }, (_, i) => ({
    id: `snap-2026-11-${String(13 - i).padStart(2, '0')}-${String(8 + i).padStart(2, '0')}`,
    created_at: new Date(Date.now() - i * 86400000).toISOString(),
    reason: reasons[i] ?? 'manual',
    checksum: `sha256:${'a1b2c3d4e5f6'.repeat(4)}${i}`,
    size: 1820 + i * 13,
  }))
  return { total: 9, limit: 50, history: entries }
}

function snapshot(): unknown {
  return {
    schema: 1,
    component: 'lan-mdns',
    created_at: new Date().toISOString(),
    reason: 'console-export',
    config_text: RAW_CONFIG,
    checksum: `sha256:${'a1b2c3d4e5f6'.repeat(8)}`,
  }
}

function metricsText(): string {
  return [
    '# HELP lan_mdns_queries_total Total mDNS questions received.',
    '# TYPE lan_mdns_queries_total counter',
    'lan_mdns_queries_total 48213',
    '# HELP lan_mdns_responses_total Total mDNS answers sent.',
    '# TYPE lan_mdns_responses_total counter',
    'lan_mdns_responses_total 47190',
    '# HELP lan_mdns_ingested_total Foreign resource records observed.',
    '# TYPE lan_mdns_ingested_total counter',
    'lan_mdns_ingested_total 128',
    '# HELP lan_mdns_health_up Healthy target count.',
    '# TYPE lan_mdns_health_up gauge',
    'lan_mdns_health_up 3',
    '# HELP lan_mdns_health_down Unhealthy target count.',
    '# TYPE lan_mdns_health_down gauge',
    'lan_mdns_health_down 1',
    '# HELP lan_mdns_gateway_requests_total Gateway requests handled.',
    '# TYPE lan_mdns_gateway_requests_total counter',
    'lan_mdns_gateway_requests_total 1204',
  ].join('\n')
}

function csvText(): string {
  const header = 'ts,source,name,qtype'
  const t = now()
  const rows = Array.from({ length: 12 }, (_, i) => {
    const ts = t - i * 7
    const source = `192.168.1.${10 + (i % 5)}`
    const name = ['nas.local', 'grafana.local', 'printer.local', 'ci.local', '_http._tcp.local'][i % 5]!
    const qtype = ['A', 'AAAA', 'PTR', 'SRV', 'TXT'][i % 5]!
    return `${ts},${source},${name},${qtype}`
  })
  return [header, ...rows].join('\n')
}

/* ----------------------------------------------------------- route table */

const DEMO_ROUTES: DemoRoute[] = [
  // --- text endpoints (metrics / CSV) ---
  { method: 'GET', match: (p) => p.includes('/api/queries') && p.includes('export=csv'), body: csvText, text: true },
  { method: 'GET', match: (p) => p.startsWith('/api/metrics'), body: metricsText, text: true },

  // --- config + auth (method-sensitive) ---
  { method: 'GET', match: (p) => p.startsWith('/api/config/history'), body: history },
  { method: 'GET', match: (p) => p.startsWith('/api/config/snapshot'), body: snapshot },
  { method: 'GET', match: (p) => p === '/api/config', body: () => ({ raw: RAW_CONFIG, structured: structuredConfig() }) },
  { method: 'POST', match: (p) => p.startsWith('/api/config/rollback'), body: () => ({ rolled_back: true, id: 'snap-demo', config_saves: 12 }) },
  { method: 'POST', match: (p) => p.startsWith('/api/config/snapshot'), body: () => ({ imported: true, reason: 'import', config_saves: 12 }) },
  { method: 'POST', match: (p) => p === '/api/config', body: () => ({ saved: true, snapshot: 'snap-demo', config_saves: 12, config_reloads: 5 }) },
  { method: 'POST', match: (p) => p.startsWith('/api/auth/password'), body: () => ({ password_set: true, changed: true }) },
  { method: 'POST', match: (p) => p.startsWith('/api/auth/verify'), body: () => ({ valid: true }) },
  { method: 'GET', match: (p) => p.startsWith('/api/auth'), body: () => ({ token_required: false, password_set: true, write_protection: true }) },

  // --- observability / logs / alerts / status ---
  { method: 'GET', match: (p) => p.startsWith('/api/logs'), body: logs },
  { method: 'GET', match: (p) => p.startsWith('/api/alerts'), body: alerts },
  { method: 'GET', match: (p) => p.startsWith('/api/conflicts'), body: conflicts },
  { method: 'GET', match: (p) => p.startsWith('/api/browse'), body: browse },
  { method: 'GET', match: (p) => p.startsWith('/api/queries'), body: queries },
  { method: 'GET', match: (p) => p.startsWith('/api/records'), body: records },
  { method: 'GET', match: (p) => p.startsWith('/api/health/targets'), body: healthTargets },
  { method: 'GET', match: (p) => p.startsWith('/api/interfaces'), body: interfaces },
  { method: 'GET', match: (p) => p.startsWith('/api/status'), body: status },

  // --- generic write success fallback (covers any unmapped POST) ---
  { method: 'POST', match: () => true, body: () => ({ ok: true }) },
]

/* -------------------------------------------------------------- shims */

function patchFetch(): void {
  const realFetch = window.fetch.bind(window)
  // The unified console routes every component call through the same-origin
  // proxy at `/p/<component>/api/...`; strip the prefix so the demo route table
  // (keyed by plain `/api/...`) still matches.
  const stripProxy = (raw: string): string => raw.replace(/^\/p\/[^/]+/, '')
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url =
      typeof input === 'string'
        ? input
        : input instanceof URL
          ? input.toString()
          : (input as Request).url
    const method = (init?.method ?? 'GET').toString().toUpperCase()
    const route = DEMO_ROUTES.find((r) => r.method === method && r.match(stripProxy(url)))
    if (!route) return realFetch(input, init)

    if (route.text) {
      return new Response(String(route.body()), {
        status: 200,
        headers: { 'Content-Type': 'text/plain; charset=utf-8' },
      })
    }
    const payload = { ok: true, data: route.body(), request_id: 'demo' }
    return new Response(JSON.stringify(payload), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })
  }
}

/** Minimal `EventSource` stub: opens cleanly and emits one demo line. */
class DemoEventSource {
  onopen: ((ev: Event) => void) | null = null
  onmessage: ((ev: MessageEvent<string>) => void) | null = null
  onerror: ((ev: Event) => void) | null = null
  readonly url: string

  constructor(url: string) {
    this.url = url
    setTimeout(() => this.onopen?.(new Event('open')), 0)
    setTimeout(() => {
      this.onmessage?.(
        new MessageEvent('message', {
          data: JSON.stringify({
            ts: now(),
            level: 'info',
            source: 'demo',
            msg: '实时流已连接（演示模式）',
          }),
        }),
      )
    }, 50)
  }

  close(): void {
    /* nothing to tear down */
  }

  addEventListener(): void {
    /* no-op */
  }

  removeEventListener(): void {
    /* no-op */
  }
}

function patchEventSource(): void {
  // Only shadow the global when it exists; pages already guard on its absence.
  if (typeof window !== 'undefined') {
    ;(window as unknown as { EventSource: unknown }).EventSource = DemoEventSource
  }
}

/**
 * Install the demo shim. Returns true when demo mode was activated.
 * Safe to call unconditionally: a no-op unless `?demo=1` is present.
 */
export function installDemoMode(): boolean {
  if (typeof location === 'undefined') return false
  const params = new URLSearchParams(location.search)
  if (params.get('demo') !== '1') return false
  patchFetch()
  patchEventSource()
  return true
}

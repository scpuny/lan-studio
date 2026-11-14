/**
 * Blank-but-typed starting values. Factories (not shared constants) so a
 * component can never mutate another component's default state.
 */
import type { AlertStatus, LogsData, StatusData } from './types'

export function emptyStatus(): StatusData {
  return {
    component: '',
    version: '',
    running: false,
    uptime: 0,
    protocol: '',
    records: 0,
    group: '',
    mdns_port: 0,
    interface_mode: '',
    queries: 0,
    responses: 0,
    ingested: 0,
    announces: 0,
    malformed: 0,
    bytes_in: 0,
    bytes_out: 0,
    security: { token_required: false, password_set: false, write_protection: true },
    history: { versions: 0, limit: 0 },
    health: { up: 0, down: 0, unknown: 0 },
    conflicts: 0,
    config_saves: 0,
    config_reloads: 0,
    alerts: { enabled: false, webhooks: 0, queued: 0, sent: 0, failed: 0 },
    gateway: { enabled: false, port: 0, routes: 0, requests: 0, forwarded: 0 },
    logs: { buffered: 0, capacity: 0, disk_bytes: 0, disk_limit_bytes: 0 },
  }
}

export function emptyLogs(): LogsData {
  return { logs: [], total: 0, capacity: 0, disk_bytes: 0, disk_limit_bytes: 0 }
}

export function emptyAlertStatus(): AlertStatus {
  return {
    enabled: false,
    webhooks: 0,
    queue_capacity: 0,
    queued: 0,
    sent: 0,
    failed: 0,
    dropped: 0,
    suppressed: 0,
    retries: 0,
    cooldown_seconds: 0,
    by_kind: [],
    gateway: { enabled: false, routes: 0, requests: 0, forwarded: 0, unreachable: 0 },
  }
}

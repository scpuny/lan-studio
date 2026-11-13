/**
 * Wire contract of the lan-mdns admin API.
 *
 * Every interface here mirrors something asserted by `contracts/openapi.yaml`
 * (envelope + error codes) or by the exact JSON emitted by a view handler in
 * `components/lan-mdns/src/main.rs`. When a handler changes shape, this file
 * changes with it — `npm run typecheck` is the gate that keeps them in sync.
 */

/* ------------------------------------------------------------ envelopes */

/** M0 contract: every success response is `{ ok: true, data, request_id }`. */
export interface ApiEnvelope<T> {
  ok: true
  data: T
  request_id: string
}

export interface ApiErrorDetail {
  code: ApiErrorCode
  message: string
  /** Present when the rejection is attributable to one config field. */
  field?: string
}

/** M0 contract: every failure is `{ ok: false, error: {...}, request_id }`. */
export interface ApiErrorEnvelope {
  ok: false
  error: ApiErrorDetail
  request_id: string
}

/** `docs/ERROR-CODES.md` — closed set, never extended at a call site. */
export type ApiErrorCode =
  | 'invalid_config'
  | 'auth_required'
  | 'auth_failed'
  | 'setup_required'
  | 'not_found'
  | 'target_unreachable'
  | 'rate_limited'
  | 'internal_error'

/* -------------------------------------------------------------- scalars */

export type LogLevel = 'debug' | 'info' | 'warning' | 'error'
export type HealthState = 'up' | 'down' | 'unknown'
export type AlertResult = 'sent' | 'failed'
export type RecordType = 'A' | 'AAAA' | 'PTR' | 'SRV' | 'TXT'
export type InterfaceMode = 'auto' | 'all' | 'specified'

/**
 * Some numeric fields are emitted by the server through `get_str`, so they
 * arrive as decimal text (`"5.0"`). Callers normalise with `Number()`.
 */
export type Numeric = number | string

/* --------------------------------------------------------------- status */

export interface SecurityInfo {
  token_required: boolean
  password_set: boolean
  write_protection: boolean
}

export interface HistoryInfo {
  versions: number
  limit: number
}

export interface HealthCounts {
  up: number
  down: number
  unknown: number
}

export interface AlertSummary {
  enabled: boolean
  webhooks: number
  queued: number
  sent: number
  failed: number
}

export interface GatewaySummary {
  enabled: boolean
  port: number
  routes: number
  requests: number
  forwarded: number
}

export interface LogSummary {
  buffered: number
  capacity: number
  disk_bytes: number
  disk_limit_bytes: number
}

export interface StatusData {
  component: string
  version: string
  running: boolean
  uptime: number
  protocol: string
  records: number
  group: string
  mdns_port: number
  interface_mode: string
  queries: number
  responses: number
  ingested: number
  announces: number
  malformed: number
  bytes_in: number
  bytes_out: number
  security: SecurityInfo
  history: HistoryInfo
  health: HealthCounts
  conflicts: number
  config_saves: number
  config_reloads: number
  alerts: AlertSummary
  gateway: GatewaySummary
  logs: LogSummary
}

/* --------------------------------------------------------------- config */

export interface ConfigRecord {
  name: string
  type: string
  value: string
  ttl: number
}

export interface ConfigService {
  instance: string
  type: string
  host: string
  port: number
  proxy: boolean
  txt: string[]
}

export interface ConfigHealthTarget {
  alias: string
  host: string
  port: number
}

export interface ConfigGatewayHost {
  host: string
  target: string
  proxy: boolean
}

export interface ConfigGatewayView {
  enabled: boolean
  port: number
  listen: string
  admin_host: string
  timeout_seconds: Numeric
  hosts: ConfigGatewayHost[]
}

/** Server-side projection of the parsed document (`config_model_json`). */
export interface StructuredConfig {
  version: number
  component: string
  server: {
    listen: string
    port: number
    webui: boolean
    /** The token itself is never returned — only whether one is set. */
    admin_token_set: boolean
    log_level: string
  }
  observability: {
    metrics: boolean
    alerts: boolean
    history_limit: number
  }
  logging: {
    max_bytes: number
    backup_count: number
    level: string
  }
  alerts: {
    enabled: boolean
    webhooks: string[]
    cooldown_seconds: number
    timeout_seconds: Numeric
    max_retries: number
  }
  mdns: {
    group: string
    port: number
    ttl: number
    announce_count: number
    announce_interval_ms: number
    announce_period_s: number
    reverse_ptr: boolean
    interface_mode: string
    interfaces: string[]
    records: ConfigRecord[]
    services: ConfigService[]
    health_targets: ConfigHealthTarget[]
    gateway: ConfigGatewayView
  }
}

/** Emitted instead of a projection when the stored document fails validation. */
export interface InvalidStructuredConfig {
  invalid: true
  code: string
  field: string
  detail: string
}

export type ConfigProjection = StructuredConfig | InvalidStructuredConfig

/** Narrowing helper: the projection parsed cleanly. */
export function isStructuredConfig(
  value: ConfigProjection | null | undefined,
): value is StructuredConfig {
  return !!value && !(value as InvalidStructuredConfig).invalid
}

export interface ConfigData {
  raw: string
  structured: ConfigProjection
}

export interface ConfigSaveResult {
  saved: boolean
  /** Id of the `before-save` snapshot written for rollback. */
  snapshot: string
  config_saves: number
  config_reloads: number
}

export interface ReloadResult {
  reloaded: boolean
  config_reloads: number
}

/* ------------------------------------------------------------- snapshots */

/** `contracts/snapshot.json` — self-contained, checksum-verified. */
export interface Snapshot {
  schema: number
  component: string
  created_at: string
  reason: string
  config_text: string
  checksum: string
}

export interface HistoryEntry {
  id: string
  created_at: string
  reason: string
  checksum: string
  size: number
}

export interface ConfigHistoryData {
  history: HistoryEntry[]
  total: number
  limit: number
}

export interface RollbackResult {
  rolled_back: boolean
  id: string
  config_saves: number
}

export interface SnapshotImportResult {
  imported: boolean
  reason: string
  config_saves: number
}

/* ---------------------------------------------------------------- auth */

export interface AuthState {
  token_required: boolean
  password_set: boolean
  write_protection: boolean
}

export interface PasswordResult {
  password_set: boolean
  changed: boolean
}

export interface VerifyResult {
  valid: boolean
}

/* ------------------------------------------------------------ protocol */

export interface RecordItem {
  name: string
  type: string
  ttl: number
  value: string
  cache_flush: boolean
}

export interface RecordsData {
  records: RecordItem[]
  total: number
}

export interface QueryItem {
  ts: number
  source: string
  name: string
  qtype: string
}

export interface QueriesData {
  queries: QueryItem[]
  total: number
  limit: number
}

export interface BrowseEntry {
  ts: number
  source: string
  name: string
  type: string
  value: string
  ttl: number
}

export interface ServiceInstance {
  name: string
  value: string
  source: string
  ts: number
}

export interface ServiceGroup {
  service_type: string
  instances: ServiceInstance[]
  count: number
}

export interface BrowseData {
  browse: BrowseEntry[]
  total: number
  services: ServiceGroup[]
  service_types: number
}

export interface ConflictItem {
  name: string
  type: string
  rtype: number
  values: string[]
  sources: string[]
}

export interface ConflictsData {
  conflicts: ConflictItem[]
  total: number
}

export interface ProbeAnswer {
  name: string
  type: string
  ttl: number
  value: string
}

export interface ProbeResult {
  name: string
  type: string
  answers: ProbeAnswer[]
  total: number
}

/* -------------------------------------------------------------- health */

export interface HealthTargetStatus {
  alias: string
  host: string
  port: number
  state: HealthState
  checked_at: number
  latency_ms: number
}

export interface HealthTargetsData extends HealthCounts {
  targets: HealthTargetStatus[]
  total: number
}

/* ---------------------------------------------------------- interfaces */

export interface InterfaceInfo {
  name: string
  addr: string
  /** Whether this address currently takes part in the multicast group. */
  joined: boolean
}

export interface InterfacesData {
  interfaces: InterfaceInfo[]
  total: number
  mode: string
  selected: string[]
  default_interface: string | null
  /** F-A12: the interface set is rebuilt at bind time, never hot-reloaded. */
  restart_required: boolean
}

/* ---------------------------------------------------------------- logs */

export interface LogsData {
  logs: string[]
  total: number
  capacity: number
  disk_bytes: number
  disk_limit_bytes: number
}

/** `contracts/log-record.json` — what one `logs[]` string parses into. */
export interface LogRecord {
  ts: number
  level: LogLevel
  source: string
  msg: string
  extra?: Record<string, unknown>
  request_id?: string
}

/* -------------------------------------------------------------- alerts */

export interface AlertKindCount {
  kind: string
  result: AlertResult
  count: number
}

export interface AlertGatewayStatus {
  enabled: boolean
  routes: number
  requests: number
  forwarded: number
  unreachable: number
}

export interface AlertStatus {
  enabled: boolean
  webhooks: number
  queue_capacity: number
  queued: number
  sent: number
  failed: number
  dropped: number
  suppressed: number
  retries: number
  cooldown_seconds: number
  by_kind: AlertKindCount[]
  gateway: AlertGatewayStatus
}

/* ------------------------------------------------------------- form AST */

/**
 * The editable document model used by form mode (F-E04). It is deliberately a
 * hand-written AST rather than a straight projection of `StructuredConfig`:
 * the form must round-trip `server.admin_token`, which the server redacts.
 */
export interface RecordModel {
  name: string
  type: RecordType
  value: string
  ttl: number
}

export interface ServiceModel {
  instance: string
  type: string
  host: string
  port: number
  proxy: boolean
  /** Each entry is a verbatim `key=value` TXT string. */
  txt: string[]
}

export interface HealthTargetModel {
  alias: string
  host: string
  port: number
}

export interface GatewayHostModel {
  host: string
  target: string
  proxy: boolean
}

export interface GatewayModel {
  enabled: boolean
  port: number
  listen: string
  adminHost: string
  timeoutSeconds: number
  hosts: GatewayHostModel[]
}

export interface ServerModel {
  listen: string
  port: number
  webui: boolean
  adminToken: string
  logLevel: LogLevel
}

export interface ObservabilityModel {
  metrics: boolean
  alerts: boolean
  historyLimit: number
}

export interface LoggingModel {
  maxBytes: number
  backupCount: number
  level: LogLevel
}

export interface AlertsModel {
  enabled: boolean
  webhooks: string[]
  cooldownSeconds: number
  timeoutSeconds: number
  maxRetries: number
}

export interface MdnsModel {
  group: string
  port: number
  ttl: number
  announceCount: number
  announceIntervalMs: number
  announcePeriodS: number
  reversePtr: boolean
  interfaceMode: InterfaceMode
  interfaces: string[]
  records: RecordModel[]
  services: ServiceModel[]
  healthTargets: HealthTargetModel[]
  gateway: GatewayModel
}

export interface ConfigModel {
  version: number
  component: string
  server: ServerModel
  observability: ObservabilityModel
  logging: LoggingModel
  alerts: AlertsModel
  mdns: MdnsModel
}

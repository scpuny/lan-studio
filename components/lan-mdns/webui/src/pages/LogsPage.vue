<template>
  <div class="ls-panel">
    <div class="ls-panel-title">
      运行日志
      <span class="ls-hint">结构化记录：ts / level / source / msg / request_id</span>
    </div>

    <div class="ls-row" style="margin-bottom: 12px">
      <a-select v-model="level" style="width: 160px">
        <a-option value="all">全部级别</a-option>
        <a-option value="debug">debug</a-option>
        <a-option value="info">info</a-option>
        <a-option value="warning">warning</a-option>
        <a-option value="error">error</a-option>
      </a-select>
      <a-input v-model="keyword" placeholder="关键字过滤" allow-clear style="width: 240px" />
      <span class="ls-inline">
        <a-switch v-model="follow" />
        <span class="ls-note">自动跟随最新</span>
      </span>
      <span class="ls-inline">
        <span class="ls-dot" :class="connected ? 'is-up' : 'is-down'"></span>
        <span class="ls-note">{{ streamHint }}</span>
      </span>
      <div class="ls-header-spacer"></div>
      <a-button @click="loadBuffer">载入历史</a-button>
      <a-button @click="lines = []">清空视图</a-button>
    </div>

    <div class="ls-row" style="margin-bottom: 10px">
      <span class="ls-note">
        内存缓冲 {{ buffer.capacity }} 条 · 当前 {{ lines.length }} 条 · 磁盘
        {{ formatBytes(buffer.disk_bytes) }} / {{ formatBytes(buffer.disk_limit_bytes) }}
      </span>
      <div class="ls-header-spacer"></div>
      <a-progress
        v-if="buffer.disk_limit_bytes"
        :percent="diskPercent"
        :color="diskPercent > 85 ? '#f53f3f' : '#165dff'"
        :show-text="false"
        style="width: 180px"
      />
    </div>

    <div ref="viewport" class="ls-log">
      <div v-if="visible.length === 0" class="ls-note">暂无日志。</div>
      <div v-for="line in visible" :key="line.key" class="ls-log-line">
        <span class="ls-log-time">{{ line.time }}</span>
        <span class="ls-tag-level" :class="`ls-level-${line.level}`">{{ line.level }}</span>
        <span class="ls-cert" style="margin-right: 8px">{{ line.source }}</span>
        <span>{{ line.msg }}</span>
        <span v-if="line.extra" class="ls-cert"> {{ line.extra }}</span>
      </div>
    </div>

    <p class="ls-note" style="margin-top: 10px">
      日志只保留受控字段；凭据、密码摘要与令牌不会写入日志。磁盘占用硬上界为
      「单文件上限 ×（备份数 + 1）」，可在「配置管理 · 可观测与日志」调整。
    </p>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { get } from '../api'
import { emptyLogs } from '../defaults'
import { formatBytes } from '../poll'
import type { LogLevel, LogRecord, LogsData } from '../types'

interface Props {
  active?: boolean
}
const props = withDefaults(defineProps<Props>(), { active: false })

type LevelFilter = 'all' | LogLevel

interface LogLine {
  key: string
  raw: string
  ts: number
  time: string
  level: LogLevel
  source: string
  msg: string
  extra: string
}

const MAX_VIEW = 1000

const lines = ref<LogLine[]>([])
const level = ref<LevelFilter>('all')
const keyword = ref('')
const follow = ref(true)
const connected = ref(false)
const buffer = ref<LogsData>(emptyLogs())
const viewport = ref<HTMLDivElement | null>(null)

let source: EventSource | null = null
let seen = new Set<string>()

/**
 * `EventSource` is missing in some embedded webviews and in non-browser test
 * environments. Degrade to "manual refresh only" instead of throwing out of
 * `onMounted`, which would take the whole console down.
 */
const sseSupported = typeof EventSource !== 'undefined'

const streamHint = computed<string>(() => {
  if (!sseSupported) return '当前环境不支持实时流，请用「载入历史」手动刷新'
  return connected.value ? '实时流已连接' : '实时流断开，自动重连中'
})

const diskPercent = computed<number>(() => {  const limit = buffer.value.disk_limit_bytes || 0
  if (!limit) return 0
  return Math.min(100, Math.round((buffer.value.disk_bytes / limit) * 100))
})

const visible = computed<LogLine[]>(() => {
  const wanted = level.value
  const needle = keyword.value.trim().toLowerCase()
  return lines.value.filter((line) => {
    if (wanted !== 'all' && line.level !== wanted) return false
    if (!needle) return true
    return `${line.msg} ${line.source} ${line.extra}`.toLowerCase().includes(needle)
  })
})

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function asLevel(value: unknown): LogLevel {
  if (value === 'debug' || value === 'warning' || value === 'error') return value
  return 'info'
}

function parseLine(raw: string): LogLine {
  let record: LogRecord | null = null
  try {
    const parsed: unknown = JSON.parse(raw)
    if (isRecord(parsed)) {
      record = {
        ts: typeof parsed.ts === 'number' ? parsed.ts : 0,
        level: asLevel(parsed.level),
        source: typeof parsed.source === 'string' ? parsed.source : 'lan-mdns',
        msg: typeof parsed.msg === 'string' ? parsed.msg : '',
        extra: isRecord(parsed.extra) ? parsed.extra : undefined,
      }
    }
  } catch {
    record = null
  }
  if (!record) {
    return { key: raw, raw, ts: 0, time: '—', level: 'info', source: 'lan-mdns', msg: raw, extra: '' }
  }
  const ts = Number(record.ts) || 0
  return {
    key: raw,
    raw,
    ts,
    time: ts ? new Date(ts * 1000).toLocaleTimeString('zh-CN', { hour12: false }) : '—',
    level: record.level,
    source: record.source,
    msg: record.msg,
    extra: record.extra ? JSON.stringify(record.extra) : '',
  }
}

function scrollToEnd(): void {
  void nextTick(() => {
    const element = viewport.value
    if (element) element.scrollTop = element.scrollHeight
  })
}

function push(raw: string): void {
  if (!raw || seen.has(raw)) return
  seen.add(raw)
  lines.value.push(parseLine(raw))
  if (lines.value.length > MAX_VIEW) {
    const dropped = lines.value.splice(0, lines.value.length - MAX_VIEW)
    for (const item of dropped) seen.delete(item.raw)
  }
  if (follow.value) scrollToEnd()
}

async function loadBuffer(): Promise<void> {
  try {
    const data = await get<LogsData>('/api/logs?limit=300')
    buffer.value = data
    lines.value = []
    seen = new Set<string>()
    for (const raw of data.logs) push(raw)
    if (follow.value) scrollToEnd()
  } catch {
    /* keep the view as-is */
  }
}

function connect(): void {
  if (!sseSupported) {
    connected.value = false
    return
  }
  source?.close()
  source = new EventSource('/api/logs/stream')
  source.onopen = (): void => {
    connected.value = true
  }
  source.onmessage = (event: MessageEvent<string>): void => {
    push(event.data)
  }
  source.onerror = (): void => {
    connected.value = false
    // The server bounds every stream (max_duration / max_events); EventSource
    // reconnects on its own using the advertised retry hint.
  }
}

onMounted(() => {
  void loadBuffer()
  connect()
})

onBeforeUnmount(() => {
  source?.close()
  source = null
})

watch(
  () => props.active,
  (value) => {
    if (value) {
      if (!source) connect()
      void loadBuffer()
    }
  },
)
</script>

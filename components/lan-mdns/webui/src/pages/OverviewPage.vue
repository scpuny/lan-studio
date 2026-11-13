<template>
  <div>
    <div class="ls-panel">
      <div class="ls-panel-title">
        运行状态
        <span class="ls-hint">每 5 秒静默刷新，不触发布局动画</span>
      </div>
      <div class="ls-grid ls-grid--status">
        <div v-for="item in lights" :key="item.label" class="ls-metric-card">
          <div class="ls-inline">
            <span class="ls-dot" :class="item.cls"></span>
            <div>
              <div class="ls-kpi-label">{{ item.label }}</div>
              <div class="ls-kpi-value small">{{ item.value }}</div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <div class="ls-panel">
      <div class="ls-panel-title">协议计数</div>
      <div class="ls-grid ls-grid--counters">
        <div v-for="item in counters" :key="item.label" class="ls-metric-card">
          <div class="ls-kpi-label">{{ item.label }}</div>
          <div class="ls-kpi-value">{{ item.value }}</div>
          <div v-if="item.foot" class="ls-kpi-foot">{{ item.foot }}</div>
        </div>
      </div>
    </div>

    <!-- F-D04: health lights + list -->
    <div class="ls-panel">
      <div class="ls-panel-title">
        业务健康目标
        <span class="ls-hint">TCP 探测，与 /healthz 存活探针无关</span>
        <div class="ls-header-spacer"></div>
        <span class="ls-inline" style="font-weight: 400">
          <span class="ls-dot is-up"></span> {{ health.up }} 正常
          <span class="ls-dot is-down" style="margin-left: 10px"></span> {{ health.down }} 异常
          <span class="ls-dot is-unknown" style="margin-left: 10px"></span> {{ health.unknown }} 未知
        </span>
      </div>

      <a-alert v-if="healthError" type="error">{{ healthError }}</a-alert>
      <a-table
        v-else
        :data="healthRows"
        :columns="healthColumns"
        :pagination="false"
        :bordered="false"
        size="small"
        row-key="key"
      >
        <template #state="{ record }">
          <span class="ls-inline">
            <span class="ls-dot" :class="stateClass(record.state)"></span>
            <span>{{ stateText(record.state) }}</span>
          </span>
        </template>
        <template #latency="{ record }">
          <span class="ls-mono">{{ latencyText(record.latency_ms) }}</span>
        </template>
        <template #checked="{ record }">
          <span class="ls-mono ls-muted">{{ formatTime(record.checked_at) }}</span>
        </template>
        <template #empty>
          <div class="ls-empty">
            <a-empty description="尚未配置健康目标">
              <template #image>
                <span class="ls-dot is-unknown" style="width: 28px; height: 28px"></span>
              </template>
            </a-empty>
            <p class="ls-note" style="text-align: center">
              在「配置管理 · 域名与服务」中添加 <code>health_targets</code> 后，这里会显示状态灯与探测结果。
            </p>
          </div>
        </template>
      </a-table>
    </div>

    <!-- F-H05: in-console navigation to registered services -->
    <div class="ls-panel">
      <div class="ls-panel-title">
        服务导航
        <span class="ls-hint">辅助入口：仅跳转已登记名称，不改变 DNS 边界</span>
      </div>

      <a-alert v-if="!gatewayEnabled" type="info" style="margin-bottom: 12px">
        内置 HTTP 网关当前关闭，<code>/go/</code> 跳转不可用；下方「直达」仍按已登记的
        <code>.local</code> 名称打开服务。网关开关在「配置管理 · HTTP 网关」。
      </a-alert>

      <div v-if="services.length === 0" class="ls-empty">
        <a-empty description="尚未登记服务" />
        <p class="ls-note" style="text-align: center">
          在「配置管理 · 域名与服务」中添加服务实例后，这里会出现直达入口。
        </p>
      </div>

      <div v-else>
        <div v-for="service in services" :key="service.key" class="ls-service-row">
          <div>
            <div class="ls-service-name">{{ service.instance }}</div>
            <div class="ls-cert">
              {{ service.type }} · {{ service.host }}:{{ service.port }} ·
              代理{{ service.proxy ? '开' : '关' }}
            </div>
          </div>
          <div class="ls-row">
            <a-button size="mini" type="primary" @click="openService(service)">直达</a-button>
            <a-button size="mini" :disabled="!gatewayEnabled" @click="openGateway(service)">
              /go/ 跳转
            </a-button>
            <a-button size="mini" type="text" @click="copy(service.url)">复制</a-button>
          </div>
        </div>
      </div>
    </div>

    <div class="ls-panel">
      <div class="ls-panel-title">
        跳转说明
        <span class="ls-hint">PRD 1.4 边界</span>
      </div>
      <p class="ls-note">
        客户端解析 <code>.local</code> 后直连目标地址，DNS 服务不是流量前置代理。因此
        <code>/go/</code> 只是管理台内的辅助入口：它把已登记名称重定向到对应地址，
        未登记的 Host 一律拒绝，不缓存、不记录业务载荷。
      </p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from 'vue'
import { Message } from '@arco-design/web-vue'
import type { TableColumnData } from '@arco-design/web-vue'
import { errorMessage, get } from '../api'
import { emptyStatus } from '../defaults'
import { formatBytes, formatTime, usePolling } from '../poll'
import { isStructuredConfig } from '../types'
import type {
  ConfigData,
  ConfigService,
  HealthCounts,
  HealthState,
  HealthTargetStatus,
  HealthTargetsData,
  StatusData,
} from '../types'

interface Props {
  active?: boolean
}
const props = withDefaults(defineProps<Props>(), { active: false })

interface Light {
  label: string
  value: string
  cls: string
}

interface Counter {
  label: string
  value: string | number
  foot: string
}

interface HealthRow extends HealthTargetStatus {
  key: string
  target: string
}

interface ServiceRow extends ConfigService {
  key: string
  url: string
}

const status = reactive<StatusData>(emptyStatus())
const health = reactive<HealthCounts>({ up: 0, down: 0, unknown: 0 })
const healthRows = ref<HealthRow[]>([])
const healthError = ref('')
const services = ref<ServiceRow[]>([])
const gatewayEnabled = ref(false)
const gateway = reactive({ port: 80, adminHost: 'mdns.local', enabled: false })

const healthColumns: TableColumnData[] = [
  { title: '状态', dataIndex: 'state', slotName: 'state', width: 110 },
  { title: '别名', dataIndex: 'alias', width: 150 },
  { title: '目标', dataIndex: 'target' },
  { title: '延迟', dataIndex: 'latency', slotName: 'latency', width: 110 },
  { title: '最近检查', dataIndex: 'checked', slotName: 'checked', width: 190 },
]

const STATE_TEXT: Record<HealthState, string> = { up: '正常', down: '异常', unknown: '未知' }
const STATE_CLASS: Record<HealthState, string> = {
  up: 'is-up',
  down: 'is-down',
  unknown: 'is-unknown',
}

function stateText(state: HealthState): string {
  return STATE_TEXT[state] ?? '未知'
}

function stateClass(state: HealthState): string {
  return STATE_CLASS[state] ?? 'is-unknown'
}

function latencyText(ms: number): string {
  return ms ? `${ms.toFixed(1)} ms` : '—'
}

const lights = computed<Light[]>(() => [
  {
    label: 'mDNS 引擎',
    value: status.running ? '运行中' : '停止',
    cls: status.running ? 'is-up' : 'is-down',
  },
  {
    label: '写保护',
    value: status.security.password_set ? '密码已设置' : '未初始化',
    cls: status.security.password_set ? 'is-up' : 'is-warn',
  },
  {
    label: 'HTTP 网关',
    value: gatewayEnabled.value ? `监听 ${gateway.port}` : '已关闭',
    cls: gatewayEnabled.value ? 'is-up' : 'is-unknown',
  },
  {
    label: '告警通道',
    value: status.alerts.enabled ? `${status.alerts.webhooks} 个 Webhook` : '未启用',
    cls: status.alerts.enabled ? 'is-up' : 'is-unknown',
  },
  {
    label: '健康目标',
    value: `${health.up} 正常 / ${health.down} 异常`,
    cls: health.down > 0 ? 'is-down' : health.up > 0 ? 'is-up' : 'is-unknown',
  },
  {
    label: '名称冲突',
    value: status.conflicts ? `${status.conflicts} 条` : '无',
    cls: status.conflicts ? 'is-warn' : 'is-up',
  },
])

const counters = computed<Counter[]>(() => [
  { label: '资源记录', value: status.records, foot: '/api/records' },
  { label: '查询', value: status.queries, foot: '收到的问题报文' },
  { label: '应答', value: status.responses, foot: '发出的应答报文' },
  { label: '外来记录', value: status.ingested, foot: '局域网观测到的 RR' },
  { label: '主动宣告', value: status.announces, foot: '启动 + 周期' },
  { label: '畸形报文', value: status.malformed, foot: '解析失败计数' },
  { label: '入流量', value: formatBytes(status.bytes_in), foot: '累计字节' },
  { label: '出流量', value: formatBytes(status.bytes_out), foot: '累计字节' },
])

function openService(service: ServiceRow): void {
  window.open(service.url, '_blank', 'noopener')
}

function openGateway(service: ServiceRow): void {
  const base =
    gateway.port === 80
      ? `http://${gateway.adminHost}`
      : `http://${gateway.adminHost}:${gateway.port}`
  window.open(`${base}/go/?host=${encodeURIComponent(service.host)}`, '_blank', 'noopener')
}

async function copy(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text)
    Message.success('已复制')
  } catch {
    Message.warning(`请手动复制：${text}`)
  }
}

async function refreshHealth(): Promise<void> {
  try {
    const data = await get<HealthTargetsData>('/api/health/targets')
    health.up = data.up
    health.down = data.down
    health.unknown = data.unknown
    healthRows.value = data.targets.map((target, index) => ({
      ...target,
      key: `${target.alias || target.host}:${target.port}:${index}`,
      target: `${target.host}:${target.port}`,
    }))
    healthError.value = ''
  } catch (error) {
    healthError.value = `健康目标读取失败：${errorMessage(error)}`
    healthRows.value = []
  }
}

async function refreshConfig(): Promise<void> {
  try {
    const data = await get<ConfigData>('/api/config')
    if (!isStructuredConfig(data.structured)) {
      services.value = []
      return
    }
    const view = data.structured.mdns.gateway
    gatewayEnabled.value = view.enabled
    gateway.enabled = view.enabled
    gateway.port = view.port
    gateway.adminHost = view.admin_host || 'mdns.local'
    services.value = data.structured.mdns.services.map((service, index) => ({
      ...service,
      key: `${service.instance}:${index}`,
      url:
        service.port === 80
          ? `http://${service.host}/`
          : `http://${service.host}:${service.port}/`,
    }))
  } catch {
    services.value = []
  }
}

async function refreshStatus(): Promise<void> {
  try {
    Object.assign(status, await get<StatusData>('/api/status'))
  } catch {
    status.running = false
  }
}

usePolling(
  async () => {
    await Promise.all([refreshStatus(), refreshHealth(), refreshConfig()])
  },
  5000,
  computed(() => props.active),
)
</script>

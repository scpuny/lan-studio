<template>
  <div class="ls-panel">
    <div class="ls-panel-title">
      <Icon name="radar" />
      可观测
      <span class="ls-hint">服务地图与审计队列按 TTL 自动老化</span>
      <div class="ls-header-spacer"></div>
      <a-button @click="refreshAll">刷新</a-button>
    </div>

    <a-tabs v-model:active-key="tab" type="card-gutter" :destroy-on-hide="false">
      <a-tab-pane key="map">
        <template #title><Icon name="globe" :size="14" /> 服务地图</template>
        <div class="ls-grid" style="margin-bottom: 14px">
          <div class="ls-metric-card">
            <div class="ls-kpi-label">服务类型</div>
            <div class="ls-kpi-value">{{ browse.service_types }}</div>
          </div>
          <div class="ls-metric-card">
            <div class="ls-kpi-label">观测条目</div>
            <div class="ls-kpi-value">{{ browse.total }}</div>
          </div>
        </div>

        <div v-if="browse.services.length === 0" class="ls-empty">
          <a-empty description="尚未观测到局域网服务" />
          <p class="ls-note" style="text-align: center">
            客户端发起组播查询、或其他主机宣告服务后，这里会按类型聚合出现。
          </p>
        </div>
        <div
          v-for="group in browse.services"
          :key="group.service_type"
          class="ls-panel"
          style="margin-bottom: 10px"
        >
          <div class="ls-panel-title">
            <code>{{ group.service_type }}</code>
            <span class="ls-hint">{{ group.count }} 个实例</span>
          </div>
          <div v-for="(item, index) in group.instances" :key="index" class="ls-service-row">
            <div>
              <div class="ls-service-name">{{ item.name }}</div>
              <div class="ls-cert">{{ item.value }}</div>
            </div>
            <div class="ls-cert">来源 {{ item.source }} · {{ formatTime(item.ts) }}</div>
          </div>
        </div>
      </a-tab-pane>

      <a-tab-pane key="queries">
        <template #title><Icon name="search" :size="14" /> 查询审计</template>
        <div class="ls-row" style="margin-bottom: 10px">
          <span class="ls-note">最近 {{ queries.length }} 条查询，环形缓冲有界，不落盘。</span>
          <div class="ls-header-spacer"></div>
          <a-button @click="exportCsv">导出 CSV</a-button>
        </div>
        <a-table
          :data="queries"
          :columns="queryColumns"
          :pagination="{ pageSize: 15, size: 'mini' }"
          :bordered="false"
          row-key="key"
        >
          <template #ts="{ record }">
            <span class="ls-mono ls-muted">{{ formatTime(record.ts) }}</span>
          </template>
          <template #empty>
            <div class="ls-note">尚无查询记录。</div>
          </template>
        </a-table>
      </a-tab-pane>

      <a-tab-pane key="conflicts">
        <template #title><Icon name="alert" :size="14" /> 名称冲突</template>
        <a-alert v-if="conflicts.length > 0" type="warning" style="margin-bottom: 12px">
          检测到同名记录出现不同值，可能存在名称冲突或非法宣告。
        </a-alert>
        <a-table
          :data="conflicts"
          :columns="conflictColumns"
          :pagination="false"
          :bordered="false"
          row-key="name"
        >
          <template #values="{ record }">
            <span class="ls-mono">{{ record.values.join(' / ') }}</span>
          </template>
          <template #sources="{ record }">
            <span class="ls-mono ls-muted">{{ record.sources.join(', ') }}</span>
          </template>
          <template #empty>
            <div class="ls-note">未检测到名称冲突。</div>
          </template>
        </a-table>
      </a-tab-pane>

      <a-tab-pane key="records">
        <template #title><Icon name="layers" :size="14" /> 资源记录</template>
        <div class="ls-row" style="margin-bottom: 10px">
          <a-input v-model="recordFilter" placeholder="按名称前缀过滤" allow-clear style="width: 260px" />
          <span class="ls-note">共 {{ records.length }} 条，本进程发布。</span>
        </div>
        <a-table
          :data="filteredRecords"
          :columns="recordColumns"
          :pagination="{ pageSize: 12, size: 'mini' }"
          :bordered="false"
          row-key="key"
        >
          <template #empty>
            <div class="ls-note">尚未发布记录。</div>
          </template>
        </a-table>
      </a-tab-pane>

      <a-tab-pane key="metrics">
        <template #title><Icon name="chart" :size="14" /> 指标</template>
        <div class="ls-row" style="margin-bottom: 10px">
          <span class="ls-note">
            Prometheus 文本格式（<code>0.0.4</code>），前缀 <code>lanstudio_mdns_*</code>。
          </span>
          <div class="ls-header-spacer"></div>
          <a-button @click="copyMetrics">复制</a-button>
        </div>
        <pre class="ls-log" style="height: 420px">{{ metricsText || '加载中…' }}</pre>
      </a-tab-pane>
    </a-tabs>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Message } from '@arco-design/web-vue'
import type { TableColumnData } from '@arco-design/web-vue'
import { errorMessage, get, getText } from '../api'
import Icon from '../Icon.vue'
import { formatTime, usePolling } from '../poll'
import type {
  BrowseData,
  ConflictItem,
  ConflictsData,
  QueryItem,
  QueriesData,
  RecordItem,
  RecordsData,
} from '../types'

interface Props {
  active?: boolean
}
const props = withDefaults(defineProps<Props>(), { active: false })

interface QueryRow extends QueryItem {
  key: string
}

interface RecordRow extends RecordItem {
  key: string
}

const tab = ref('map')
const browse = ref<BrowseData>({ browse: [], services: [], total: 0, service_types: 0 })
const queries = ref<QueryRow[]>([])
const conflicts = ref<ConflictItem[]>([])
const records = ref<RecordRow[]>([])
const metricsText = ref('')
const recordFilter = ref('')

const queryColumns: TableColumnData[] = [
  { title: '时间', dataIndex: 'ts', slotName: 'ts', width: 190 },
  { title: '来源', dataIndex: 'source', width: 170 },
  { title: '查询名', dataIndex: 'name' },
  { title: '类型', dataIndex: 'qtype', width: 90 },
]

const conflictColumns: TableColumnData[] = [
  { title: '名称', dataIndex: 'name', width: 220 },
  { title: '类型', dataIndex: 'type', width: 90 },
  { title: '不同取值', dataIndex: 'values', slotName: 'values' },
  { title: '来源', dataIndex: 'sources', slotName: 'sources', width: 220 },
]

const recordColumns: TableColumnData[] = [
  { title: '名称', dataIndex: 'name', width: 230 },
  { title: '类型', dataIndex: 'type', width: 80 },
  { title: 'TTL', dataIndex: 'ttl', width: 80 },
  { title: '取值', dataIndex: 'value' },
]

const filteredRecords = computed<RecordRow[]>(() => {
  const prefix = recordFilter.value.trim().toLowerCase()
  if (!prefix) return records.value
  return records.value.filter((record) => record.name.toLowerCase().includes(prefix))
})

async function refreshAll(): Promise<void> {
  try {
    browse.value = await get<BrowseData>('/api/browse')
  } catch {
    browse.value = { browse: [], services: [], total: 0, service_types: 0 }
  }
  try {
    const data = await get<QueriesData>('/api/queries?limit=200')
    queries.value = data.queries.map((entry, index) => ({ ...entry, key: `${entry.ts}:${index}` }))
  } catch {
    queries.value = []
  }
  try {
    const data = await get<ConflictsData>('/api/conflicts')
    conflicts.value = data.conflicts
  } catch {
    conflicts.value = []
  }
  try {
    const data = await get<RecordsData>('/api/records')
    records.value = data.records.map((record, index) => ({
      ...record,
      key: `${record.name}:${record.type}:${index}`,
    }))
  } catch {
    records.value = []
  }
  try {
    metricsText.value = await getText('/api/metrics')
  } catch {
    metricsText.value = ''
  }
}

async function exportCsv(): Promise<void> {
  try {
    const text = await getText('/api/queries?limit=1000&export=csv')
    const blob = new Blob([text], { type: 'text/csv;charset=utf-8' })
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = 'lan-mdns-queries.csv'
    anchor.click()
    URL.revokeObjectURL(url)
  } catch (error) {
    Message.error(`导出失败：${errorMessage(error)}`)
  }
}

async function copyMetrics(): Promise<void> {
  try {
    await navigator.clipboard.writeText(metricsText.value)
    Message.success('已复制指标文本')
  } catch {
    Message.warning('浏览器拒绝了剪贴板访问')
  }
}

usePolling(refreshAll, 5000, computed(() => props.active))

watch(
  () => props.active,
  (value) => {
    if (value) void refreshAll()
  },
)
</script>

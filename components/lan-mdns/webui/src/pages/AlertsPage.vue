<template>
  <div>
    <div class="ls-panel">
      <div class="ls-panel-title">
        告警通道
        <span class="ls-hint">本地判定 + Webhook 推送，不依赖外部服务</span>
        <div class="ls-header-spacer"></div>
        <a-button @click="$emit('goto', 'config')">编辑 Webhook</a-button>
      </div>

      <a-alert v-if="error" type="error">{{ error }}</a-alert>

      <div class="ls-grid" style="margin-bottom: 14px">
        <div class="ls-metric-card">
          <div class="ls-kpi-label">状态</div>
          <div class="ls-inline">
            <span class="ls-dot" :class="status.enabled ? 'is-up' : 'is-unknown'"></span>
            <span class="ls-kpi-value small">{{ status.enabled ? '已启用' : '未启用' }}</span>
          </div>
          <div class="ls-kpi-foot">{{ status.webhooks }} 个 Webhook</div>
        </div>
        <div class="ls-metric-card">
          <div class="ls-kpi-label">队列</div>
          <div class="ls-kpi-value">
            {{ status.queued
            }}<span class="ls-muted" style="font-size: 13px"> / {{ status.queue_capacity }}</span>
          </div>
          <div class="ls-kpi-foot">有界队列，满则丢弃并计数</div>
        </div>
        <div class="ls-metric-card">
          <div class="ls-kpi-label">已发送</div>
          <div class="ls-kpi-value">{{ status.sent }}</div>
          <div class="ls-kpi-foot">失败 {{ status.failed }} · 丢弃 {{ status.dropped }}</div>
        </div>
        <div class="ls-metric-card">
          <div class="ls-kpi-label">冷却抑制</div>
          <div class="ls-kpi-value">{{ status.suppressed }}</div>
          <div class="ls-kpi-foot">窗口 {{ status.cooldown_seconds }} 秒</div>
        </div>
        <div class="ls-metric-card">
          <div class="ls-kpi-label">重试次数</div>
          <div class="ls-kpi-value">{{ status.retries }}</div>
          <div class="ls-kpi-foot">单事件最多重试 2 次</div>
        </div>
      </div>

      <a-tabs default-active-key="kinds" type="card-gutter" :destroy-on-hide="false">
        <a-tab-pane key="kinds" title="按事件类型">
          <a-table
            :data="kindRows"
            :columns="kindColumns"
            :pagination="false"
            :bordered="false"
            row-key="key"
          >
            <template #result="{ record }">
              <a-tag :color="record.result === 'sent' ? 'green' : 'red'" size="small">
                {{ record.result }}
              </a-tag>
            </template>
            <template #empty>
              <div class="ls-note">
                尚无告警发出。触发条件为健康状态翻转、名称冲突、配置非法与网关不可达。
              </div>
            </template>
          </a-table>
        </a-tab-pane>

        <a-tab-pane key="gateway" title="网关状态">
          <div class="ls-grid">
            <div class="ls-metric-card">
              <div class="ls-kpi-label">网关</div>
              <div class="ls-inline">
                <span class="ls-dot" :class="status.gateway.enabled ? 'is-up' : 'is-unknown'"></span>
                <span class="ls-kpi-value small">{{ status.gateway.enabled ? '已开启' : '已关闭' }}</span>
              </div>
              <div class="ls-kpi-foot">{{ status.gateway.routes }} 条静态路由</div>
            </div>
            <div class="ls-metric-card">
              <div class="ls-kpi-label">请求</div>
              <div class="ls-kpi-value">{{ status.gateway.requests }}</div>
            </div>
            <div class="ls-metric-card">
              <div class="ls-kpi-label">已转发</div>
              <div class="ls-kpi-value">{{ status.gateway.forwarded }}</div>
            </div>
            <div class="ls-metric-card">
              <div class="ls-kpi-label">不可达</div>
              <div class="ls-kpi-value">{{ status.gateway.unreachable }}</div>
              <div class="ls-kpi-foot">触发 gateway_target_unreachable 告警</div>
            </div>
          </div>
        </a-tab-pane>

        <a-tab-pane key="platforms" title="平台适配">
          <a-table
            :data="platformRows"
            :columns="platformColumns"
            :pagination="false"
            :bordered="false"
            row-key="name"
          >
            <template #match="{ record }">
              <span class="ls-mono">{{ record.match }}</span>
            </template>
          </a-table>
          <p class="ls-note" style="margin-top: 12px">
            平台按 URL 域名自动识别，无需额外配置。事件体统一遵循
            <code>contracts/alert-event.json</code>；令牌、密码与摘要一律脱敏后再发送。
            连通性错误与发送失败分别计入 <code>failed</code> 与 <code>retries</code>。
          </p>
        </a-tab-pane>
      </a-tabs>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from 'vue'
import type { TableColumnData } from '@arco-design/web-vue'
import { errorMessage, get } from '../api'
import { emptyAlertStatus } from '../defaults'
import { usePolling } from '../poll'
import type { AlertKindCount, AlertStatus } from '../types'

interface Props {
  active?: boolean
}
const props = withDefaults(defineProps<Props>(), { active: false })
defineEmits<{ goto: [key: string] }>()

interface KindRow extends AlertKindCount {
  key: string
}

interface PlatformRow {
  name: string
  match: string
}

const status = reactive<AlertStatus>(emptyAlertStatus())
const error = ref('')

const kindColumns: TableColumnData[] = [
  { title: '事件类型', dataIndex: 'kind', width: 240 },
  { title: '结果', dataIndex: 'result', slotName: 'result', width: 100 },
  { title: '次数', dataIndex: 'count', width: 100 },
]

const platformColumns: TableColumnData[] = [
  { title: '平台', dataIndex: 'name', width: 140 },
  { title: '域名匹配', dataIndex: 'match', slotName: 'match' },
]

const platformRows: PlatformRow[] = [
  { name: '钉钉', match: 'oapi.dingtalk.com' },
  { name: '飞书', match: 'open.feishu.cn / open.larksuite.com' },
  { name: 'Slack', match: 'hooks.slack.com' },
  { name: '企业微信', match: 'qyapi.weixin.qq.com' },
  { name: '通用 JSON', match: '其他任意 http(s) 地址' },
]

const kindRows = computed<KindRow[]>(() =>
  status.by_kind.map((item) => ({ ...item, key: `${item.kind}:${item.result}` })),
)

async function refresh(): Promise<void> {
  try {
    const data = await get<AlertStatus>('/api/alerts')
    // `gateway` is nested, so it is merged separately to keep the reactive
    // sub-object identity stable across polls.
    const { gateway, ...rest } = data
    Object.assign(status, rest)
    Object.assign(status.gateway, gateway)
    error.value = ''
  } catch (failure) {
    error.value = `告警状态读取失败：${errorMessage(failure)}`
  }
}

usePolling(refresh, 5000, computed(() => props.active))
</script>

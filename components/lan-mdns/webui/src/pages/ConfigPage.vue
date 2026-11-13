<template>
  <div>
    <div class="ls-panel">
      <div class="ls-panel-title">
        配置管理
        <span class="ls-hint">表单与源码是同一份文档；保存前服务端离线校验，失败不落盘</span>
        <div class="ls-header-spacer"></div>
        <span v-if="formDirty || rawDirty" class="ls-inline" style="font-weight: 400">
          <span class="ls-dot is-warn"></span> 有未保存修改
        </span>
      </div>

      <a-alert v-if="loadError" type="error" style="margin-bottom: 12px">{{ loadError }}</a-alert>
      <a-alert
        v-if="saveError"
        type="error"
        style="margin-bottom: 12px"
        closable
        @close="saveError = null"
      >
        <strong>{{ saveError.code }}</strong>
        <span v-if="saveError.field"> · 字段 <code>{{ saveError.field }}</code></span>
        ：{{ saveError.message }}
      </a-alert>
      <a-alert v-if="saveOk" type="success" style="margin-bottom: 12px" closable @close="saveOk = ''">
        {{ saveOk }}
      </a-alert>

      <div class="ls-row" style="margin-bottom: 14px">
        <a-button type="primary" :loading="saving" :disabled="!loaded" @click="save">
          校验并保存
        </a-button>
        <a-button :disabled="saving" @click="reload(true)">放弃修改并重载</a-button>
        <a-button v-if="subTab === 'raw' && formDirty" :disabled="saving" @click="regenerateSource">
          按表单重新生成源码
        </a-button>
        <span v-if="loaded" class="ls-note">
          组件 <code>{{ model.component }}</code> · 文档 <code>version {{ model.version }}</code>
        </span>
      </div>

      <a-spin :loading="loading" style="width: 100%">
        <a-tabs
          v-if="loaded"
          v-model:active-key="subTab"
          size="small"
          type="card-gutter"
          :destroy-on-hide="false"
          @change="onSubTabChange"
        >
          <a-tab-pane key="basic" title="基础参数">
            <div class="ls-field-grid">
              <div class="ls-field">
                <label>管理接口监听地址</label>
                <a-input v-model="model.server.listen" placeholder="127.0.0.1" />
                <span class="ls-note">默认仅回环；改为非回环地址时必须配置 Admin Token。</span>
              </div>
              <div class="ls-field">
                <label>管理接口端口</label>
                <a-input-number
                  v-model="model.server.port"
                  :min="1"
                  :max="65535"
                  style="width: 100%"
                />
              </div>
              <div class="ls-field">
                <label>内嵌控制台</label>
                <a-switch v-model="model.server.webui" />
                <span class="ls-note">关闭后 <code>/</code> 返回 404，API 不受影响。</span>
              </div>
              <div class="ls-field">
                <label>日志级别（进程）</label>
                <a-select v-model="model.server.logLevel" style="width: 100%">
                  <a-option v-for="level in LEVELS" :key="level" :value="level">
                    {{ level }}
                  </a-option>
                </a-select>
              </div>
            </div>

            <div class="ls-field-grid">
              <div class="ls-field">
                <label>多播组</label>
                <a-input v-model="model.mdns.group" />
              </div>
              <div class="ls-field">
                <label>mDNS 端口</label>
                <a-input-number v-model="model.mdns.port" :min="1" :max="65535" style="width: 100%" />
              </div>
              <div class="ls-field">
                <label>记录 TTL（秒）</label>
                <a-input-number v-model="model.mdns.ttl" :min="1" :max="86400" style="width: 100%" />
              </div>
              <div class="ls-field">
                <label>反向 PTR</label>
                <a-switch v-model="model.mdns.reversePtr" />
              </div>
            </div>

            <div class="ls-field-grid">
              <div class="ls-field">
                <label>启动宣告次数</label>
                <a-input-number
                  v-model="model.mdns.announceCount"
                  :min="1"
                  :max="10"
                  style="width: 100%"
                />
              </div>
              <div class="ls-field">
                <label>宣告间隔（毫秒）</label>
                <a-input-number
                  v-model="model.mdns.announceIntervalMs"
                  :min="50"
                  :max="5000"
                  :step="50"
                  style="width: 100%"
                />
              </div>
              <div class="ls-field">
                <label>周期宣告（秒）</label>
                <a-input-number
                  v-model="model.mdns.announcePeriodS"
                  :min="10"
                  :max="3600"
                  style="width: 100%"
                />
              </div>
            </div>
          </a-tab-pane>

          <a-tab-pane key="records" title="域名与服务">
            <div class="ls-panel-title">
              资源记录
              <span class="ls-hint">A / AAAA / PTR / SRV / TXT；TTL 留 0 表示跟随全局</span>
              <div class="ls-header-spacer"></div>
              <a-button size="mini" @click="addRecord">新增记录</a-button>
            </div>
            <div v-if="model.mdns.records.length === 0" class="ls-note">尚未发布任何记录。</div>
            <div
              v-for="(record, index) in model.mdns.records"
              :key="`r${index}`"
              class="ls-row"
              style="margin-bottom: 8px"
            >
              <a-input v-model="record.name" placeholder="nas.local" style="width: 220px" />
              <a-select v-model="record.type" style="width: 110px">
                <a-option v-for="type in RECORD_TYPES" :key="type" :value="type">
                  {{ type }}
                </a-option>
              </a-select>
              <a-input v-model="record.value" placeholder="192.168.5.10" style="width: 260px" />
              <a-input-number
                v-model="record.ttl"
                :min="0"
                :max="86400"
                placeholder="TTL"
                style="width: 110px"
              />
              <a-button
                size="mini"
                status="danger"
                type="text"
                @click="model.mdns.records.splice(index, 1)"
              >
                删除
              </a-button>
            </div>

            <div class="ls-panel-title" style="margin-top: 22px">
              服务实例（DNS-SD）
              <span class="ls-hint">自动生成 PTR / SRV / TXT 与服务类型枚举</span>
              <div class="ls-header-spacer"></div>
              <a-button size="mini" @click="addService">新增服务</a-button>
            </div>
            <div v-if="model.mdns.services.length === 0" class="ls-note">尚未登记服务实例。</div>
            <div
              v-for="(service, index) in model.mdns.services"
              :key="`s${index}`"
              class="ls-panel"
              style="margin-bottom: 10px"
            >
              <div class="ls-field-grid">
                <div class="ls-field">
                  <label>实例名</label>
                  <a-input v-model="service.instance" placeholder="Web Server" />
                </div>
                <div class="ls-field">
                  <label>服务类型</label>
                  <a-input v-model="service.type" placeholder="_http._tcp.local" />
                </div>
                <div class="ls-field">
                  <label>主机名</label>
                  <a-input v-model="service.host" placeholder="nas.local" />
                </div>
                <div class="ls-field">
                  <label>端口</label>
                  <a-input-number v-model="service.port" :min="1" :max="65535" style="width: 100%" />
                </div>
              </div>
              <div class="ls-field">
                <label>TXT 记录（每行 <code>key=value</code>）</label>
                <textarea
                  class="ls-code"
                  style="min-height: 74px"
                  :value="service.txt.join('\n')"
                  @input="onTxtInput(service, $event)"
                ></textarea>
              </div>
              <div class="ls-row">
                <span class="ls-inline">
                  <a-switch v-model="service.proxy" size="small" />
                  <span class="ls-note">参与内置网关代理（需全局网关开启）</span>
                </span>
                <div class="ls-header-spacer"></div>
                <a-button
                  size="mini"
                  status="danger"
                  type="text"
                  @click="model.mdns.services.splice(index, 1)"
                >
                  删除服务
                </a-button>
              </div>
            </div>

            <div class="ls-panel-title" style="margin-top: 22px">
              健康目标
              <span class="ls-hint">TCP 连接探测；别名用于指标标签</span>
              <div class="ls-header-spacer"></div>
              <a-button size="mini" @click="addTarget">新增目标</a-button>
            </div>
            <div v-if="model.mdns.healthTargets.length === 0" class="ls-note">未配置健康目标。</div>
            <div
              v-for="(target, index) in model.mdns.healthTargets"
              :key="`t${index}`"
              class="ls-row"
              style="margin-bottom: 8px"
            >
              <a-input v-model="target.alias" placeholder="别名（可空）" style="width: 180px" />
              <a-input v-model="target.host" placeholder="192.168.5.10" style="width: 220px" />
              <a-input-number
                v-model="target.port"
                :min="1"
                :max="65535"
                placeholder="端口"
                style="width: 110px"
              />
              <a-button
                size="mini"
                status="danger"
                type="text"
                @click="model.mdns.healthTargets.splice(index, 1)"
              >
                删除
              </a-button>
            </div>
          </a-tab-pane>

          <a-tab-pane key="gateway" title="HTTP 网关">
            <a-alert type="info" style="margin-bottom: 14px">
              网关只做 Host 头静态路由，不缓存、不解析、不记录业务载荷；未登记 Host 一律拒绝。
              全局关闭时不创建任何监听。
            </a-alert>
            <div class="ls-field-grid">
              <div class="ls-field">
                <label>全局开关</label>
                <a-switch v-model="model.mdns.gateway.enabled" />
                <span class="ls-note">关闭后每服务 <code>proxy</code> 开关不生效。</span>
              </div>
              <div class="ls-field">
                <label>监听端口</label>
                <a-input-number
                  v-model="model.mdns.gateway.port"
                  :min="1"
                  :max="65535"
                  style="width: 100%"
                />
              </div>
              <div class="ls-field">
                <label>监听地址</label>
                <a-input v-model="model.mdns.gateway.listen" />
              </div>
              <div class="ls-field">
                <label>管理台统一域名</label>
                <a-input v-model="model.mdns.gateway.adminHost" />
              </div>
              <div class="ls-field">
                <label>上游超时（秒）</label>
                <a-input-number
                  v-model="model.mdns.gateway.timeoutSeconds"
                  :min="1"
                  :max="60"
                  style="width: 100%"
                />
              </div>
            </div>

            <div class="ls-panel-title" style="margin-top: 12px">
              静态路由覆盖
              <span class="ls-hint">未列出的服务按已发布 A 记录自动生成路由</span>
              <div class="ls-header-spacer"></div>
              <a-button size="mini" @click="addGatewayHost">新增路由</a-button>
            </div>
            <div v-if="model.mdns.gateway.hosts.length === 0" class="ls-note">无显式路由覆盖。</div>
            <div
              v-for="(host, index) in model.mdns.gateway.hosts"
              :key="`g${index}`"
              class="ls-row"
              style="margin-bottom: 8px"
            >
              <a-input v-model="host.host" placeholder="printer.local" style="width: 200px" />
              <a-input v-model="host.target" placeholder="192.168.5.30:80" style="width: 220px" />
              <span class="ls-inline">
                <a-switch v-model="host.proxy" size="small" />
                <span class="ls-note">代理</span>
              </span>
              <a-button
                size="mini"
                status="danger"
                type="text"
                @click="model.mdns.gateway.hosts.splice(index, 1)"
              >
                删除
              </a-button>
            </div>
          </a-tab-pane>

          <a-tab-pane key="interfaces" title="网卡绑定">
            <a-alert type="warning" style="margin-bottom: 14px">
              接口集合变更需要重建组播 socket，因此保存后必须重启进程才生效；本页不伪装成热重载。
            </a-alert>
            <div class="ls-field-grid">
              <div class="ls-field">
                <label>绑定模式</label>
                <a-select v-model="model.mdns.interfaceMode" style="width: 100%">
                  <a-option value="auto">自动（内核默认出口网卡）</a-option>
                  <a-option value="all">全部（枚举所有 IPv4 网卡）</a-option>
                  <a-option value="specified">指定（使用下方地址列表）</a-option>
                </a-select>
              </div>
            </div>

            <div class="ls-panel-title">
              已配置地址
              <span class="ls-hint">仅 <code>specified</code> 模式读取此列表</span>
              <div class="ls-header-spacer"></div>
              <a-button
                size="mini"
                :disabled="model.mdns.interfaceMode !== 'specified'"
                @click="model.mdns.interfaces.push('')"
              >
                新增地址
              </a-button>
            </div>
            <div v-if="model.mdns.interfaces.length === 0" class="ls-note">
              列表为空：<code>all</code> 会自动展开为全部本机地址，<code>auto</code> 由内核选择。
            </div>
            <div
              v-for="(_addr, index) in model.mdns.interfaces"
              :key="`i${index}`"
              class="ls-row"
              style="margin-bottom: 8px"
            >
              <a-input
                v-model="model.mdns.interfaces[index]"
                placeholder="192.168.5.20"
                style="width: 240px"
              />
              <a-button
                size="mini"
                status="danger"
                type="text"
                @click="model.mdns.interfaces.splice(index, 1)"
              >
                删除
              </a-button>
            </div>

            <div class="ls-panel-title" style="margin-top: 18px">
              运行态网卡
              <span class="ls-hint">来自 <code>/api/interfaces</code></span>
              <div class="ls-header-spacer"></div>
              <a-button size="mini" @click="loadInterfaces">刷新</a-button>
            </div>
            <p v-if="interfaceInfo" class="ls-note">
              当前模式 <code>{{ interfaceInfo.mode }}</code> ·
              默认网卡 <code>{{ interfaceInfo.default_interface || '未探测到' }}</code> ·
              {{ interfaceInfo.restart_required ? '接口变更需重启生效' : '' }}
            </p>
            <a-table
              :data="interfaceRows"
              :columns="interfaceColumns"
              :pagination="false"
              :bordered="false"
              size="small"
              row-key="addr"
            >
              <template #joined="{ record }">
                <span class="ls-inline">
                  <span class="ls-dot" :class="record.joined ? 'is-up' : 'is-unknown'"></span>
                  {{ record.joined ? '已加入组播' : '未加入' }}
                </span>
              </template>
              <template #empty>
                <div class="ls-note">未枚举到 IPv4 网卡（运行环境可能缺少 ip 命令）。</div>
              </template>
            </a-table>
          </a-tab-pane>

          <a-tab-pane key="observe" title="可观测与日志">
            <div class="ls-field-grid">
              <div class="ls-field">
                <label>暴露 Prometheus 指标</label>
                <a-switch v-model="model.observability.metrics" />
              </div>
              <div class="ls-field">
                <label>启用告警</label>
                <a-switch v-model="model.observability.alerts" />
              </div>
              <div class="ls-field">
                <label>配置历史保留条数</label>
                <a-input-number
                  v-model="model.observability.historyLimit"
                  :min="1"
                  :max="1000"
                  style="width: 100%"
                />
              </div>
            </div>
            <div class="ls-field-grid">
              <div class="ls-field">
                <label>单个日志文件上限（字节）</label>
                <a-input-number
                  v-model="model.logging.maxBytes"
                  :min="1024"
                  :step="1024"
                  style="width: 100%"
                />
                <span class="ls-note">磁盘硬上界 = 上限 ×（备份数 + 1）。</span>
              </div>
              <div class="ls-field">
                <label>轮转备份数</label>
                <a-input-number
                  v-model="model.logging.backupCount"
                  :min="0"
                  :max="100"
                  style="width: 100%"
                />
              </div>
              <div class="ls-field">
                <label>写入级别</label>
                <a-select v-model="model.logging.level" style="width: 100%">
                  <a-option v-for="level in LEVELS" :key="level" :value="level">
                    {{ level }}
                  </a-option>
                </a-select>
              </div>
            </div>
          </a-tab-pane>

          <a-tab-pane key="security" title="安全">
            <a-alert type="info" style="margin-bottom: 14px">
              管理员密码通过右上角「管理员」设置或修改，不写入配置文件。
            </a-alert>
            <div class="ls-field">
              <label>Admin Token（所有 /api/* 请求需携带 X-Admin-Token）</label>
              <a-input-password
                v-model="model.server.adminToken"
                placeholder="留空表示不启用 Token"
              />
              <span class="ls-note">
                当前运行态：Token {{ runtime.tokenRequired ? '已启用' : '未启用' }} · 管理员密码
                {{ runtime.passwordSet ? '已设置' : '未初始化' }}。 非回环监听地址必须配置 Token。
              </span>
            </div>
          </a-tab-pane>

          <a-tab-pane key="alerts" title="告警">
            <div class="ls-field-grid">
              <div class="ls-field">
                <label>启用告警</label>
                <a-switch v-model="model.alerts.enabled" />
              </div>
              <div class="ls-field">
                <label>冷却时间（秒）</label>
                <a-input-number
                  v-model="model.alerts.cooldownSeconds"
                  :min="0"
                  :max="86400"
                  style="width: 100%"
                />
              </div>
              <div class="ls-field">
                <label>请求超时（秒）</label>
                <a-input-number
                  v-model="model.alerts.timeoutSeconds"
                  :min="1"
                  :max="60"
                  :step="0.5"
                  style="width: 100%"
                />
              </div>
              <div class="ls-field">
                <label>失败重试次数</label>
                <a-input-number
                  v-model="model.alerts.maxRetries"
                  :min="0"
                  :max="2"
                  style="width: 100%"
                />
              </div>
            </div>

            <div class="ls-field">
              <label>Webhook 地址（每行一个）</label>
              <textarea
                class="ls-code"
                style="min-height: 120px"
                :value="model.alerts.webhooks.join('\n')"
                placeholder="https://oapi.dingtalk.com/robot/send?access_token=..."
                @input="onWebhooksInput"
              ></textarea>
              <span class="ls-note">
                平台按域名自动识别：钉钉 / 飞书 / Slack / 企业微信，其余按通用 JSON 发送。
                保存后立即生效，无需重启。
              </span>
            </div>
          </a-tab-pane>

          <a-tab-pane key="raw" title="源码模式">
            <a-alert type="info" style="margin-bottom: 12px">
              保存会按服务端解析器校验；表单模式保存会重写为标准格式，注释不会保留。
            </a-alert>
            <textarea
              v-model="rawDraft"
              class="ls-code"
              spellcheck="false"
              placeholder="version: 1&#10;component: lan-mdns&#10;..."
            ></textarea>
          </a-tab-pane>
        </a-tabs>
      </a-spin>
    </div>
  </div>
</template>

<script setup lang="ts">
import { nextTick, reactive, ref, watch } from 'vue'
import { Message } from '@arco-design/web-vue'
import type { TableColumnData } from '@arco-design/web-vue'
import { errorMessage, get, isApiError, isAuthError, post } from '../api'
import {
  LEVELS,
  RECORD_TYPES,
  emptyModel,
  flatten,
  modelFromFlat,
  parseConfigText,
  serializeConfig,
} from '../config-text'
import type {
  AuthState,
  ConfigData,
  ConfigSaveResult,
  InterfaceInfo,
  InterfacesData,
  ServiceModel,
} from '../types'

interface Props {
  active?: boolean
}
const props = withDefaults(defineProps<Props>(), { active: false })
const emit = defineEmits<{ 'need-auth': [] }>()

interface SaveErrorView {
  code: string
  field: string
  message: string
}

interface RuntimeFlags {
  tokenRequired: boolean
  passwordSet: boolean
}

const model = ref(emptyModel())
const loaded = ref(false)
const rawDraft = ref('')
const subTab = ref('basic')
const loading = ref(false)
const saving = ref(false)
const formDirty = ref(false)
const rawDirty = ref(false)
const loadError = ref('')
const saveError = ref<SaveErrorView | null>(null)
const saveOk = ref('')
const suppress = ref(false)
const interfaceRows = ref<InterfaceInfo[]>([])
const interfaceInfo = ref<InterfacesData | null>(null)
const runtime = reactive<RuntimeFlags>({ tokenRequired: false, passwordSet: false })

const interfaceColumns: TableColumnData[] = [
  { title: '网卡', dataIndex: 'name', width: 140 },
  { title: '地址', dataIndex: 'addr', width: 180 },
  { title: '组播状态', dataIndex: 'joined', slotName: 'joined' },
]

async function reload(force = false): Promise<void> {
  if (!force && (formDirty.value || rawDirty.value)) {
    Message.warning('存在未保存修改，请先保存或选择「放弃修改并重载」')
    return
  }
  loading.value = true
  try {
    const data = await get<ConfigData>('/api/config')
    suppress.value = true
    rawDraft.value = data.raw
    model.value = modelFromFlat(flatten(data.raw))
    loaded.value = true
    await nextTick()
    formDirty.value = false
    rawDirty.value = false
    loadError.value = ''
    saveError.value = null
  } catch (error) {
    loadError.value = `配置读取失败：${errorMessage(error)}`
  } finally {
    suppress.value = false
    loading.value = false
  }
}

watch(
  model,
  () => {
    if (!suppress.value && loaded.value) formDirty.value = true
  },
  { deep: true },
)

watch(rawDraft, () => {
  if (!suppress.value && subTab.value === 'raw') rawDirty.value = true
})

function onSubTabChange(key: string | number): void {
  if (key === 'raw') {
    if (formDirty.value) regenerateSource()
    return
  }
  if (!rawDirty.value) return
  const parsed = parseConfigText(rawDraft.value)
  if (!parsed) {
    Message.warning('源码无法解析为表单，已保留上次表单内容；源码未丢失')
    return
  }
  suppress.value = true
  model.value = parsed
  void nextTick(() => {
    suppress.value = false
    formDirty.value = true
  })
  rawDirty.value = false
}

function regenerateSource(): void {
  suppress.value = true
  rawDraft.value = serializeConfig(model.value)
  void nextTick(() => {
    suppress.value = false
    formDirty.value = false
    rawDirty.value = false
  })
  Message.info('已按表单生成标准格式源码（注释不保留）')
}

async function save(): Promise<void> {
  const text = subTab.value === 'raw' ? rawDraft.value : serializeConfig(model.value)
  saving.value = true
  saveError.value = null
  saveOk.value = ''
  try {
    const data = await post<ConfigSaveResult>('/api/config', { text })
    saveOk.value = `已保存并热重载，回滚点快照 ${data.snapshot}（累计保存 ${data.config_saves} 次）`
    Message.success('配置已保存')
    await reload(true)
  } catch (error) {
    if (isAuthError(error)) {
      saveError.value = {
        code: error.code,
        field: '',
        message:
          error.code === 'setup_required'
            ? '尚未设置管理员密码，请先点击右上角「管理员」初始化'
            : '管理员密码缺失或错误，请重新解锁',
      }
      emit('need-auth')
    } else if (isApiError(error)) {
      saveError.value = { code: error.code, field: error.field, message: error.message }
    } else {
      saveError.value = { code: 'internal_error', field: '', message: errorMessage(error) }
    }
  } finally {
    saving.value = false
  }
}

async function loadInterfaces(): Promise<void> {
  try {
    const data = await get<InterfacesData>('/api/interfaces')
    interfaceRows.value = data.interfaces
    interfaceInfo.value = data
  } catch {
    interfaceRows.value = []
    interfaceInfo.value = null
  }
}

async function refreshRuntime(): Promise<void> {
  try {
    const data = await get<AuthState>('/api/auth')
    runtime.tokenRequired = data.token_required
    runtime.passwordSet = data.password_set
  } catch {
    /* ignore */
  }
}

function textareaValue(event: Event): string {
  const target = event.target
  return target instanceof HTMLTextAreaElement ? target.value : ''
}

function onTxtInput(service: ServiceModel, event: Event): void {
  service.txt = textareaValue(event)
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
}

function onWebhooksInput(event: Event): void {
  model.value.alerts.webhooks = textareaValue(event)
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
}

function addRecord(): void {
  model.value.mdns.records.push({ name: '', type: 'A', value: '', ttl: 0 })
}

function addService(): void {
  model.value.mdns.services.push({
    instance: '',
    type: '_http._tcp.local',
    host: '',
    port: 80,
    proxy: true,
    txt: [],
  })
}

function addTarget(): void {
  model.value.mdns.healthTargets.push({ alias: '', host: '', port: 80 })
}

function addGatewayHost(): void {
  model.value.mdns.gateway.hosts.push({ host: '', target: '', proxy: true })
}

watch(
  () => props.active,
  (value) => {
    if (!value) return
    // Refresh on every activation unless the operator has unsaved edits: a
    // rollback or a save made elsewhere must not leave this pane stale.
    if (!formDirty.value && !rawDirty.value) void reload(true)
    void refreshRuntime()
    void loadInterfaces()
  },
  { immediate: true },
)
</script>

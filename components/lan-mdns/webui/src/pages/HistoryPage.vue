<template>
  <div>
    <div class="ls-panel">
      <div class="ls-panel-title">
        版本历史
        <span class="ls-hint">每次保存前自动写入快照；回滚同样先备份当前配置</span>
        <div class="ls-header-spacer"></div>
        <a-button :loading="loading" @click="load">刷新</a-button>
      </div>

      <a-alert v-if="error" type="error" style="margin-bottom: 12px">{{ error }}</a-alert>
      <a-alert v-if="notice" type="success" style="margin-bottom: 12px" closable @close="notice = ''">
        {{ notice }}
      </a-alert>

      <div class="ls-row" style="margin-bottom: 12px">
        <a-button @click="exportCurrent">导出当前配置快照</a-button>
        <a-button :loading="importing" @click="pickFile">导入快照文件</a-button>
        <input
          ref="fileInput"
          type="file"
          accept="application/json,.json"
          style="display: none"
          @change="onFilePicked"
        />
        <span class="ls-note">
          保留上限 <code>{{ data.limit }}</code> 条，当前 <code>{{ data.total }}</code> 条；
          快照自包含 <code>config_text</code> 与 <code>sha256</code> 校验和。
        </span>
      </div>

      <a-table
        :data="rows"
        :columns="columns"
        :pagination="{ pageSize: 20, size: 'mini' }"
        :bordered="false"
        row-key="id"
      >
        <template #created_at="{ record }">
          <span class="ls-mono">{{ formatDate(record.created_at) }}</span>
        </template>
        <template #reason="{ record }">
          <a-tag size="small" :color="record.reason === 'before-save' ? 'arcoblue' : 'gray'">
            {{ record.reason || 'manual' }}
          </a-tag>
        </template>
        <template #size="{ record }">
          <span class="ls-mono ls-muted">{{ formatBytes(record.size) }}</span>
        </template>
        <template #checksum="{ record }">
          <span class="ls-mono ls-muted" :title="record.checksum">{{ shortHash(record.checksum) }}</span>
        </template>
        <template #actions="{ record }">
          <div class="ls-row">
            <a-button size="mini" :loading="busy === record.id" @click="rollback(record)">
              回滚到此版本
            </a-button>
            <a-button size="mini" type="text" @click="download(record)">下载</a-button>
          </div>
        </template>
        <template #empty>
          <div class="ls-note">尚无快照。保存一次配置后这里会出现回滚点。</div>
        </template>
      </a-table>

      <p class="ls-note" style="margin-top: 12px">
        回滚链路：校验快照 ID 与校验和 → 备份当前配置为新快照 → 原子替换配置文件 → 热重载。
        任何一步失败都不会改动正在运行的配置。
      </p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Message, Modal } from '@arco-design/web-vue'
import type { TableColumnData } from '@arco-design/web-vue'
import { errorMessage, get, isAuthError, post } from '../api'
import { formatBytes, formatDate } from '../poll'
import type {
  ConfigHistoryData,
  HistoryEntry,
  RollbackResult,
  Snapshot,
  SnapshotImportResult,
} from '../types'

interface Props {
  active?: boolean
}
const props = withDefaults(defineProps<Props>(), { active: false })
const emit = defineEmits<{ 'need-auth': [] }>()

const data = ref<ConfigHistoryData>({ history: [], total: 0, limit: 0 })
const loading = ref(false)
const importing = ref(false)
const busy = ref('')
const error = ref('')
const notice = ref('')
const fileInput = ref<HTMLInputElement | null>(null)

const columns: TableColumnData[] = [
  { title: '时间', dataIndex: 'created_at', slotName: 'created_at', width: 190 },
  { title: '原因', dataIndex: 'reason', slotName: 'reason', width: 150 },
  { title: '大小', dataIndex: 'size', slotName: 'size', width: 100 },
  { title: '校验和', dataIndex: 'checksum', slotName: 'checksum', width: 150 },
  { title: '快照 ID', dataIndex: 'id' },
  { title: '操作', dataIndex: 'actions', slotName: 'actions', width: 190 },
]

const rows = computed<HistoryEntry[]>(() => data.value.history)

function shortHash(checksum: string): string {
  const hex = checksum.startsWith('sha256:') ? checksum.slice(7) : checksum
  return hex.slice(0, 12)
}

async function load(): Promise<void> {
  loading.value = true
  try {
    data.value = await get<ConfigHistoryData>('/api/config/history')
    error.value = ''
  } catch (failure) {
    error.value = `历史读取失败：${errorMessage(failure)}`
  } finally {
    loading.value = false
  }
}

function saveBlob(text: string, filename: string): void {
  const blob = new Blob([text], { type: 'application/json;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = filename
  anchor.click()
  URL.revokeObjectURL(url)
}

async function exportCurrent(): Promise<void> {
  try {
    const snap = await get<Snapshot>('/api/config/snapshot?reason=console-export')
    saveBlob(JSON.stringify(snap, null, 2), `lan-mdns-${snap.created_at}.json`)
    notice.value = '已导出当前配置快照'
  } catch (failure) {
    error.value = `导出失败：${errorMessage(failure)}`
  }
}

async function download(entry: HistoryEntry): Promise<void> {
  try {
    const snap = await get<Snapshot>(`/api/config/snapshot?id=${encodeURIComponent(entry.id)}`)
    saveBlob(JSON.stringify(snap, null, 2), entry.id)
  } catch (failure) {
    error.value = `下载失败：${errorMessage(failure)}`
  }
}

function pickFile(): void {
  fileInput.value?.click()
}

async function onFilePicked(event: Event): Promise<void> {
  const input = event.target
  if (!(input instanceof HTMLInputElement)) return
  const file = input.files?.[0]
  input.value = ''
  if (!file) return
  importing.value = true
  error.value = ''
  try {
    const parsed: unknown = JSON.parse(await file.text())
    if (typeof parsed !== 'object' || parsed === null) {
      throw new Error('快照文件不是合法 JSON 对象')
    }
    const result = await post<SnapshotImportResult>('/api/config/snapshot', parsed)
    notice.value = `已导入快照（reason=${result.reason}），累计保存 ${result.config_saves} 次`
    Message.success('快照已导入并热重载')
    await load()
  } catch (failure) {
    if (isAuthError(failure)) {
      emit('need-auth')
      error.value =
        failure.code === 'setup_required'
          ? '尚未设置管理员密码，请先初始化后再导入'
          : '管理员密码缺失或错误，请重新解锁'
    } else {
      error.value = `导入失败：${errorMessage(failure)}`
    }
  } finally {
    importing.value = false
  }
}

async function applyRollback(entry: HistoryEntry): Promise<void> {
  busy.value = entry.id
  error.value = ''
  try {
    const result = await post<RollbackResult>('/api/config/rollback', { id: entry.id })
    notice.value = `已回滚到 ${result.id}，累计保存 ${result.config_saves} 次`
    Message.success('配置已回滚并热重载')
    await load()
  } catch (failure) {
    if (isAuthError(failure)) {
      emit('need-auth')
      error.value =
        failure.code === 'setup_required'
          ? '尚未设置管理员密码，请先初始化后再回滚'
          : '管理员密码缺失或错误，请重新解锁'
    } else {
      error.value = `回滚失败：${errorMessage(failure)}`
    }
  } finally {
    busy.value = ''
  }
}

function rollback(entry: HistoryEntry): void {
  // `Modal.confirm` returns a handle, not a promise; the mutation runs from the
  // `onOk` hook so a cancelled dialog can never touch the config.
  Modal.confirm({
    title: '回滚配置',
    content: `将当前配置替换为 ${formatDate(entry.created_at)} 的快照（${entry.reason || 'manual'}）。当前配置会先备份，可再次回滚。`,
    okText: '确认回滚',
    cancelText: '取消',
    onOk: () => {
      void applyRollback(entry)
    },
  })
}

watch(
  () => props.active,
  (value) => {
    if (value) void load()
  },
  { immediate: true },
)
</script>

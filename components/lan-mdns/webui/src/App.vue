<template>
  <a-config-provider size="large">
    <div class="ls-app">
    <aside class="ls-rail">
      <div class="ls-brand">
        <div class="ls-brand-mark"><Icon name="broadcast" :size="19" /></div>
        <div>
          <div class="ls-brand-name">Lan Studio</div>
          <div class="ls-brand-sub">Multicast Console</div>
        </div>
      </div>

      <div class="ls-nav-label">Workspace</div>
      <nav class="ls-nav">
        <button
          v-for="item in navItems"
          :key="item.key"
          class="ls-nav-item"
          :class="{ on: activeKey === item.key }"
          type="button"
          @click="activeKey = item.key"
        >
          <Icon :name="item.icon" />
          <span>{{ item.label }}</span>
        </button>
      </nav>

      <div class="ls-rail-foot">
        <div class="ls-rail-stat">
          <span>引擎状态</span>
          <b class="ls-inline">
            <span class="ls-dot" :class="status.running ? 'is-up' : 'is-down'"></span>
            {{ status.running ? '运行中' : '停止' }}
          </b>
        </div>
        <div class="ls-rail-stat">
          <span>组播地址</span>
          <b class="ls-mono">{{ status.group || '—' }}:{{ status.mdns_port || '—' }}</b>
        </div>
        <div class="ls-rail-stat">
          <span>版本</span>
          <b class="ls-mono">{{ status.version || '—' }}</b>
        </div>
      </div>
    </aside>

    <main class="ls-main">
      <header class="ls-topbar">
        <div>
          <div class="ls-eyebrow">
            <span class="ls-eyebrow-dot"></span>
            <span>{{ currentMeta.eyebrow }}</span>
          </div>
          <h1 class="ls-page-title">{{ currentMeta.title }}</h1>
          <p class="ls-page-sub">{{ currentMeta.sub }}</p>
        </div>
        <div class="ls-top-actions">
          <span class="ls-status-pill">
            <span class="ls-dot" :class="status.running ? 'is-up' : 'is-down'"></span>
            <span>{{ status.running ? '已连接' : '未连接' }}</span>
          </span>
          <button class="ls-icon-btn" :title="dark ? '切换浅色' : '切换深色'" @click="toggleTheme">
            <Icon :name="dark ? 'sun' : 'moon'" />
          </button>
          <a-button size="small" @click="openAuth">
            <Icon name="lock" :size="14" />
            {{ session.password ? '已解锁' : '管理员' }}
          </a-button>
        </div>
      </header>

      <a-alert
        v-if="banner"
        type="warning"
        :show-icon="true"
        closable
        @close="banner = ''"
      >
        {{ banner }}
      </a-alert>

      <div class="ls-view">
        <OverviewPage v-show="activeKey === 'overview'" :active="activeKey === 'overview'" />
        <ConfigPage
          v-show="activeKey === 'config'"
          :active="activeKey === 'config'"
          @need-auth="openAuth"
        />
        <ObservePage v-show="activeKey === 'observe'" :active="activeKey === 'observe'" />
        <LogsPage v-show="activeKey === 'logs'" :active="activeKey === 'logs'" />
        <AlertsPage
          v-show="activeKey === 'alerts'"
          :active="activeKey === 'alerts'"
          @goto="activeKey = $event"
        />
        <HistoryPage
          v-show="activeKey === 'history'"
          :active="activeKey === 'history'"
          @need-auth="openAuth"
        />
      </div>
    </main>

    <a-modal
      v-model:visible="authVisible"
      :title="authTitle"
      :footer="false"
      :width="460"
      unmount-on-close
    >
      <div class="ls-stack">
        <p class="ls-note">
          写操作统一由管理员密码保护。密码以 PBKDF2-SHA256 摘要保存在数据目录（权限 0600），
          浏览器只在会话内保留明文，不会写入磁盘。
        </p>

        <div class="ls-field">
          <label>Admin Token（配置了 server.admin_token 时必填）</label>
          <a-input v-model="authForm.token" placeholder="留空表示未启用" allow-clear />
        </div>

        <template v-if="!session.passwordSet">
          <div class="ls-field">
            <label>设置管理员密码（至少 8 位）</label>
            <a-input-password v-model="authForm.password" placeholder="首次使用需先设置" />
          </div>
          <div class="ls-field">
            <label>确认密码</label>
            <a-input-password v-model="authForm.confirm" placeholder="再次输入" />
          </div>
          <a-button type="primary" long :loading="busy" @click="setupPassword">
            设置并解锁
          </a-button>
        </template>

        <template v-else>
          <div class="ls-field">
            <label>当前密码</label>
            <a-input-password v-model="authForm.password" placeholder="用于解锁写操作" />
          </div>
          <div class="ls-row">
            <a-button type="primary" :loading="busy" @click="unlock">解锁</a-button>
            <a-button @click="toggleChange">{{ changing ? '取消修改' : '修改密码' }}</a-button>
          </div>

          <template v-if="changing">
            <div class="ls-field">
              <label>新密码（至少 8 位）</label>
              <a-input-password v-model="authForm.newPassword" />
            </div>
            <div class="ls-field">
              <label>确认新密码</label>
              <a-input-password v-model="authForm.confirm" />
            </div>
            <a-button type="primary" status="warning" long :loading="busy" @click="changePassword">
              确认修改
            </a-button>
          </template>
        </template>
      </div>
    </a-modal>
    </div>
  </a-config-provider>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { Message } from '@arco-design/web-vue'
import { clearPassword, errorMessage, get, post, session, storeCredentials } from './api'
import { emptyStatus } from './defaults'
import { usePolling } from './poll'
import type { PasswordResult, StatusData, VerifyResult } from './types'
import AlertsPage from './pages/AlertsPage.vue'
import ConfigPage from './pages/ConfigPage.vue'
import HistoryPage from './pages/HistoryPage.vue'
import Icon from './Icon.vue'
import LogsPage from './pages/LogsPage.vue'
import ObservePage from './pages/ObservePage.vue'
import OverviewPage from './pages/OverviewPage.vue'

interface AuthForm {
  token: string
  password: string
  newPassword: string
  confirm: string
}

interface NavItem {
  key: string
  label: string
  icon: string
  eyebrow: string
  title: string
  sub: string
}

// 侧栏导航（旧工程 mdns-responder 的菜单结构）
const navItems: NavItem[] = [
  {
    key: 'overview',
    label: '状态总览',
    icon: 'grid',
    eyebrow: 'Overview / 状态总览',
    title: '状态总览',
    sub: '局域网组播域名与服务发现的实时状态',
  },
  {
    key: 'config',
    label: '配置管理',
    icon: 'sliders',
    eyebrow: 'Configuration / 配置管理',
    title: '配置管理',
    sub: '域名、IP 与端口映射 · 保存后立即热重载',
  },
  {
    key: 'observe',
    label: '可观测',
    icon: 'activity',
    eyebrow: 'Observability / 可观测',
    title: '可观测',
    sub: '流量、协议分布与运行指标',
  },
  {
    key: 'logs',
    label: '日志',
    icon: 'terminal',
    eyebrow: 'Telemetry / 日志',
    title: '日志',
    sub: '查询、应答与配置变更的事件流',
  },
  {
    key: 'alerts',
    label: '告警设置',
    icon: 'bell',
    eyebrow: 'Alerts / 告警设置',
    title: '告警设置',
    sub: '阈值与通知渠道',
  },
  {
    key: 'history',
    label: '版本历史',
    icon: 'history',
    eyebrow: 'History / 版本历史',
    title: '版本历史',
    sub: '配置快照与回滚',
  },
]

const activeKey = ref('overview')
const banner = ref('')
const status = reactive<StatusData>(emptyStatus())

const dark = ref(false)
const authVisible = ref(false)
const changing = ref(false)
const busy = ref(false)
const authForm = reactive<AuthForm>({ token: '', password: '', newPassword: '', confirm: '' })

const authTitle = computed(() => (session.passwordSet ? '管理员凭据' : '初始化管理员密码'))
const currentMeta = computed(
  () => navItems.find((item) => item.key === activeKey.value) ?? navItems[0]!,
)

function applyTheme(): void {
  document.body.setAttribute('arco-theme', dark.value ? 'dark' : 'light')
  try {
    localStorage.setItem('lanstudio.dark', dark.value ? '1' : '0')
  } catch {
    /* ignore */
  }
}

function toggleTheme(): void {
  dark.value = !dark.value
  applyTheme()
}

async function refreshStatus(): Promise<void> {
  try {
    const data = await get<StatusData>('/api/status')
    Object.assign(status, data)
    session.tokenRequired = data.security.token_required
    session.passwordSet = data.security.password_set
    session.ready = true
    banner.value = ''
  } catch (error) {
    banner.value = `无法连接管理接口：${errorMessage(error)}`
  }
}

function openAuth(): void {
  authForm.token = session.token
  authForm.password = session.password
  authForm.newPassword = ''
  authForm.confirm = ''
  changing.value = false
  authVisible.value = true
}

async function setupPassword(): Promise<void> {
  if (authForm.password.length < 8) {
    Message.warning('管理员密码至少 8 位')
    return
  }
  if (authForm.password !== authForm.confirm) {
    Message.warning('两次输入的密码不一致')
    return
  }
  busy.value = true
  try {
    storeCredentials(authForm.token, '')
    await post<PasswordResult>('/api/auth/password', { new_password: authForm.password })
    storeCredentials(authForm.token, authForm.password)
    await refreshStatus()
    Message.success('管理员密码已设置，写操作已解锁')
    authVisible.value = false
  } catch (error) {
    Message.error(errorMessage(error))
  } finally {
    busy.value = false
  }
}

async function unlock(): Promise<void> {
  if (!authForm.password) {
    Message.warning('请输入管理员密码')
    return
  }
  busy.value = true
  try {
    storeCredentials(authForm.token, authForm.password)
    await post<VerifyResult>('/api/auth/verify', { password: authForm.password })
    Message.success('已解锁写操作')
    authVisible.value = false
  } catch (error) {
    clearPassword()
    storeCredentials(authForm.token, '')
    Message.error(errorMessage(error))
  } finally {
    busy.value = false
  }
}

function toggleChange(): void {
  changing.value = !changing.value
  authForm.newPassword = ''
  authForm.confirm = ''
}

async function changePassword(): Promise<void> {
  if (authForm.newPassword.length < 8) {
    Message.warning('新密码至少 8 位')
    return
  }
  if (authForm.newPassword !== authForm.confirm) {
    Message.warning('两次输入的新密码不一致')
    return
  }
  busy.value = true
  try {
    storeCredentials(authForm.token, authForm.password)
    await post<PasswordResult>('/api/auth/password', {
      current_password: authForm.password,
      new_password: authForm.newPassword,
    })
    storeCredentials(authForm.token, authForm.newPassword)
    authForm.password = authForm.newPassword
    changing.value = false
    Message.success('管理员密码已更新')
    authVisible.value = false
  } catch (error) {
    Message.error(errorMessage(error))
  } finally {
    busy.value = false
  }
}

onMounted(() => {
  try {
    dark.value = localStorage.getItem('lanstudio.dark') === '1'
  } catch {
    dark.value = false
  }
  applyTheme()
  void refreshStatus()
})

usePolling(refreshStatus, 5000)
</script>

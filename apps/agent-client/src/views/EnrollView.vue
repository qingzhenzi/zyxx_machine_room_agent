<script setup lang="ts">
import { ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  CircleAlert,
  CircleCheck,
  Clock3,
  Link2,
  Loader2,
  ShieldCheck,
} from 'lucide-vue-next'
import { RouterLink } from 'vue-router'

import DaemonOfflinePanel from '@/components/shell/DaemonOfflinePanel.vue'
import BrandMark from '@/components/ui/BrandMark.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { isEnrollLink } from '@/lib/ipc'
import { useAgentStore } from '@/stores/agent'
import { useUiStore } from '@/stores/ui'

const { t } = useI18n()
const agent = useAgentStore()
const ui = useUiStore()

const link = ref(agent.lastDeepLink)
const error = ref('')

// The OS `deep-link` event is bridged globally in WorkbenchLayout; it lands in
// the agent store, and this page mirrors it into the input field for display.
watch(
  () => agent.lastDeepLink,
  (value) => {
    if (!value) return
    link.value = value
    error.value = ''
  },
)

async function submit() {
  error.value = ''

  if (!link.value.trim()) {
    error.value = t('请先粘贴注册链接。')
    return
  }

  if (!isEnrollLink(link.value)) {
    error.value = t('注册链接格式不正确，应为 https://…/enroll?token=… 或 qestudio://…。')
    return
  }

  agent.resetEnrollOutcome()
  const result = await agent.enroll(link.value.trim())

  if (result.kind === 'issued') {
    ui.pushToast({ kind: 'success', title: t('注册成功'), detail: t('已签发设备证书。') })
  } else if (result.kind === 'pending') {
    ui.pushToast({
      kind: 'info',
      title: t('等待服务端审批'),
      detail: t('请求已提交，等待管理员批准后 daemon 会继续完成注册。'),
    })
  }
}

function clear() {
  link.value = ''
  error.value = ''
  agent.resetEnrollOutcome()
}
</script>

<template>
  <div class="h-full overflow-y-auto">
    <div class="mx-auto max-w-[880px] px-6 py-5">
      <header class="mb-4 flex items-start gap-3">
        <BrandMark :size="38" />
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <h1 class="text-[18px] font-semibold tracking-[-0.02em] text-ink">
              {{ t('注册到 QEStudio Agent') }}
            </h1>
            <StatusBadge
              :label="agent.connected ? t('已连接 daemon') : t('未连接')"
              :tone="agent.connected ? 'success' : 'danger'"
            />
          </div>
          <p class="mt-0.5 text-[11px] text-muted">
            {{ t('粘贴服务端生成的注册链接，通过 qe-agent 本机 IPC 完成注册。') }}
          </p>
        </div>
      </header>

      <DaemonOfflinePanel
        v-if="agent.connection === 'disconnected'"
        class="mb-3"
        :error="agent.error"
      />

      <section
        v-if="agent.lastDeepLink"
        class="panel mb-3 border-accent-line px-4 py-2.5"
      >
        <div class="flex items-center gap-2 text-[11px] font-medium text-accent">
          <Link2 :size="12" />
          {{ t('已收到 deep-link 事件') }}
          <StatusBadge class="ml-auto" :label="t('深链接')" tone="accent" />
        </div>
        <p class="mono mt-1 break-all text-[10px] leading-4 text-ink-soft">
          {{ agent.lastDeepLink }}
        </p>
      </section>

      <section
        v-if="agent.enrollOutcome"
        class="panel mb-3 overflow-hidden"
        :class="{
          'border-danger/40': agent.enrollOutcome.kind === 'failed',
          'border-warning/40': agent.enrollOutcome.kind === 'pending',
          'border-success/40': agent.enrollOutcome.kind === 'issued',
        }"
      >
        <div class="flex items-start gap-3 px-4 py-3">
          <CircleCheck
            v-if="agent.enrollOutcome.kind === 'issued'"
            :size="16"
            class="mt-0.5 shrink-0 text-success"
          />
          <Clock3
            v-else-if="agent.enrollOutcome.kind === 'pending'"
            :size="16"
            class="mt-0.5 shrink-0 text-warning"
          />
          <CircleAlert v-else :size="16" class="mt-0.5 shrink-0 text-danger" />
          <div class="min-w-0 flex-1">
            <template v-if="agent.enrollOutcome.kind === 'issued'">
              <h2 class="text-[12px] font-semibold text-ink">{{ t('注册成功') }}</h2>
              <p class="mt-0.5 text-[10.5px] text-muted">{{ t('已签发设备证书。') }}</p>
              <dl class="mono mt-2 space-y-1 text-[10px] text-ink-soft">
                <div class="flex gap-2">
                  <dt class="w-[92px] shrink-0 text-faint">{{ t('设备槽位') }}</dt>
                  <dd class="break-all">{{ agent.enrollOutcome.reply.device_slot ?? '—' }}</dd>
                </div>
                <div class="flex gap-2">
                  <dt class="w-[92px] shrink-0 text-faint">{{ t('设备序列号') }}</dt>
                  <dd class="break-all">{{ agent.enrollOutcome.reply.serial ?? '—' }}</dd>
                </div>
                <div class="flex gap-2">
                  <dt class="w-[92px] shrink-0 text-faint">{{ t('服务端地址') }}</dt>
                  <dd class="break-all">{{ agent.enrollOutcome.reply.mtls_endpoint ?? '—' }}</dd>
                </div>
              </dl>
              <p class="mt-2 text-[9.5px] text-faint">
                {{ t('daemon 已写盘证书；已启动的角色在重启后使用新凭证。') }}
              </p>
            </template>

            <template v-else-if="agent.enrollOutcome.kind === 'pending'">
              <h2 class="text-[12px] font-semibold text-ink">{{ t('等待服务端审批') }}</h2>
              <p class="mt-1 text-[10.5px] leading-4 text-muted">
                {{ t('请求已提交，等待管理员批准后 daemon 会继续完成注册。') }}
              </p>
              <p class="mono mt-1 break-all text-[10px] text-ink-soft">
                {{ t('待审批 ID') }}: {{ agent.enrollOutcome.reply.enroll_id ?? '—' }}
              </p>
            </template>

            <template v-else>
              <h2 class="text-[12px] font-semibold text-ink">{{ t('注册失败') }}</h2>
              <p class="mono mt-1 break-all text-[10px] leading-4 text-danger">
                {{ agent.enrollOutcome.message }}
              </p>
              <p class="mt-1 text-[9.5px] text-faint">
                {{ t('错误信息来自 qe-agent IPC；请确认 daemon 已启动且链接有效。') }}
              </p>
            </template>
          </div>
          <RouterLink v-if="agent.enrollOutcome.kind !== 'failed'" to="/" class="btn btn-sm shrink-0">
            {{ t('前往状态页') }}
          </RouterLink>
        </div>
      </section>

      <section class="panel p-4">
        <h2 class="panel-title">{{ t('注册步骤') }}</h2>
        <ol class="mt-2 space-y-1.5 text-[10.5px] leading-4 text-muted">
          <li>1. {{ t('在 QEStudio 服务端为该机器生成注册链接。') }}</li>
          <li>2. {{ t('把链接粘贴到下方输入框，或通过系统深链接自动填入。') }}</li>
          <li>3. {{ t('点击“注册”，请求会交给 qe-agent daemon。') }}</li>
        </ol>
      </section>

      <section class="panel mt-3 p-4">
        <label for="enroll-link" class="section-label">{{ t('注册链接') }}</label>
        <textarea
          id="enroll-link"
          v-model="link"
          rows="4"
          spellcheck="false"
          class="mono mt-1.5 w-full resize-y rounded border border-line bg-canvas px-3 py-2 text-[11px] leading-5 text-ink-soft outline-none transition-colors focus:border-accent-line"
          :placeholder="t('https://…/enroll?token=…&slot=…')"
        />
        <p v-if="error" class="mt-1.5 text-[10px] text-danger">{{ error }}</p>
        <p class="mt-1.5 text-[9.5px] text-faint">
          {{ t('首次注册默认信任服务端证书（TOFU），由 qe-agent 完成指纹校验。') }}
        </p>

        <div class="mt-3 flex flex-wrap items-center gap-2">
          <button
            type="button"
            class="btn btn-primary"
            :disabled="agent.enrolling"
            @click="submit"
          >
            <Loader2 v-if="agent.enrolling" :size="12" class="animate-spin-slow" />
            <ShieldCheck v-else :size="12" />
            {{ agent.enrolling ? t('注册中…') : t('注册') }}
          </button>
          <button type="button" class="btn btn-ghost" :disabled="agent.enrolling" @click="clear">
            {{ t('清空') }}
          </button>
          <span class="ml-auto text-[9.5px] text-faint">
            {{ t('请求经 qe-agent 本机 IPC 发送；注册可能等待服务端审批。') }}
          </span>
        </div>
      </section>
    </div>
  </div>
</template>

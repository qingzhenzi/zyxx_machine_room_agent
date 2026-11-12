<script setup lang="ts">
import { computed, ref } from 'vue'
import { Power, MonitorOff, RotateCcw, Server } from 'lucide-vue-next'

import DaemonOfflinePanel from '@/components/shell/DaemonOfflinePanel.vue'
import BrandMark from '@/components/ui/BrandMark.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { ipcPower, type PowerReply } from '@/lib/ipc'
import { useAgentStore } from '@/stores/agent'
import { useUiStore } from '@/stores/ui'

const agent = useAgentStore()
const ui = useUiStore()
const busy = ref('')
const result = ref<PowerReply | null>(null)

const canPower = computed(() => agent.connected)
const cleanupLabel = computed(() => {
  const cleanup = result.value?.data?.cleanup
  if (!cleanup) return ''
  const vms = cleanup.vms?.length ?? 0
  const containers = cleanup.containers?.length ?? 0
  return `清理 VM ${vms} 台、容器 ${containers} 个`
})

async function run(action: 'shutdown' | 'reboot' | 'fake', title: string, detail: string) {
  if (!window.confirm(`${title}\n\n${detail}`)) return
  busy.value = action
  result.value = null
  try {
    const reply = await ipcPower(action, true)
    result.value = reply
    ui.pushToast({
      kind: action === 'fake' ? 'warning' : 'success',
      title: action === 'fake' ? '已进入伪关机' : `${title}已调度`,
      detail: cleanupLabel.value || '动作已提交',
    })
  } catch (caught) {
    ui.pushToast({
      kind: 'error',
      title: `${title}失败`,
      detail: caught instanceof Error ? caught.message : String(caught),
    })
  } finally {
    busy.value = ''
  }
}
</script>

<template>
  <div class="h-full overflow-y-auto">
    <div class="mx-auto max-w-[880px] px-6 py-5">
      <header class="mb-4 flex items-start gap-3">
        <BrandMark :size="38" />
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <h1 class="text-[18px] font-semibold tracking-[-0.02em] text-ink">电源与关机</h1>
            <StatusBadge
              :label="agent.connected ? '已连接 daemon' : '未连接'"
              :tone="agent.connected ? 'success' : 'danger'"
            />
          </div>
          <p class="mt-0.5 text-[11px] text-muted">
            关机/重启前会先关闭 libvirt VM 和 Incus 容器；动作需要
            <code>QE_AGENT_ALLOW_POWEROFF=1</code>。
          </p>
        </div>
      </header>

      <DaemonOfflinePanel v-if="!agent.connected" :error="agent.error" />

      <template v-else>
        <section class="panel overflow-hidden">
          <div class="panel-header">
            <div>
              <h2 class="panel-title">本机电源</h2>
              <p class="panel-subtitle">与 CLI <code>qe-agent cli power</code> 使用同一条链路。</p>
            </div>
          </div>
          <div class="grid gap-2 p-4 sm:grid-cols-3">
            <button
              type="button"
              class="btn justify-center gap-2"
              :disabled="!canPower || busy !== ''"
              @click="run('shutdown', '关闭这台机器', '会先关闭所有 VM/容器，然后执行 systemctl poweroff。继续？')"
            >
              <Power :size="16" /> {{ busy === 'shutdown' ? '关机中…' : '关机' }}
            </button>
            <button
              type="button"
              class="btn justify-center gap-2"
              :disabled="!canPower || busy !== ''"
              @click="run('reboot', '重启这台机器', '会先关闭所有 VM/容器，然后执行 systemctl reboot。继续？')"
            >
              <RotateCcw :size="16" /> {{ busy === 'reboot' ? '重启中…' : '重启' }}
            </button>
            <button
              type="button"
              class="btn justify-center gap-2"
              :disabled="!canPower || busy !== ''"
              @click="run('fake', '伪关机', '只进入伪关机状态，不真正断电，VM/容器保持运行。继续？')"
            >
              <MonitorOff :size="16" /> {{ busy === 'fake' ? '处理中…' : '伪关机' }}
            </button>
          </div>
        </section>

        <section v-if="result" class="panel mt-3 p-4">
          <h2 class="mb-2 text-[12px] font-semibold text-ink">最近一次结果</h2>
          <pre class="max-h-[260px] overflow-auto rounded bg-surface-2 p-3 text-[11px] text-ink-soft">{{ JSON.stringify(result, null, 2) }}</pre>
        </section>

        <section class="panel mt-3 p-4 text-[11px] text-muted">
          <p class="mb-1 flex items-center gap-1"><Server :size="13" /> 关机前 agent 会先执行 VM/容器清理，并写入 <code>/run/qe-agent/last-power.json</code>。</p>
          <p>控制面下发的 <code>agent.command poweroff/reboot</code> 也走同一清理逻辑。</p>
        </section>
      </template>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'

const props = defineProps<{
  params: Record<string, unknown>
}>()

const stateText = computed(() => {
  switch (props.params.state) {
    case 'pending':
      return '等待控制服务器审批'
    case 'failed':
      return '注册失败（不阻塞初始化）'
    case 'ok':
      return '注册完成'
    default:
      return '配置信息不足，等待 provisioning'
  }
})

const stateColor = computed(() => {
  switch (props.params.state) {
    case 'ok':
      return 'text-[var(--blue)]'
    case 'failed':
      return 'text-[#d4a3a3]'
    default:
      return 'text-[var(--ink-soft)]'
  }
})
</script>

<template>
  <div class="flex h-full flex-col items-center justify-center">
    <div class="flex flex-col items-center">
      <div class="rise text-[2vw] font-medium tracking-[0.12em] leading-none">设备注册</div>
      <div class="rise rise-1 mt-[3vw] h-px w-[22vw] overflow-hidden bg-[var(--line)]">
        <div v-if="props.params.state !== 'ok'" class="sweep-bar h-px w-1/4 bg-[var(--blue)]" />
        <div v-else class="h-px w-full bg-[var(--blue)]" />
      </div>
      <div :class="[stateColor, 'rise', 'rise-2']" class="mt-[3vw] text-[1.5vw]">
        {{ stateText }}
      </div>
      <p
        v-if="typeof props.params.detail === 'string'"
        class="rise rise-3 mt-[1vw] text-[1vw] text-[var(--ink-faint)]"
      >
        {{ props.params.detail }}
      </p>
    </div>
  </div>
</template>

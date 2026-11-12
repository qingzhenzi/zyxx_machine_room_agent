<script setup lang="ts">
import { Minus, Minimize2, Square, X } from 'lucide-vue-next'
import { useI18n } from 'vue-i18n'

withDefaults(defineProps<{ maximized: boolean; maximizable?: boolean }>(), {
  maximizable: true,
})

const { t } = useI18n()

const emit = defineEmits<{
  minimize: []
  maximize: []
  close: []
}>()
</script>

<template>
  <div class="flex items-center">
    <button
      type="button"
      class="flex h-[38px] w-[42px] items-center justify-center text-muted transition-colors hover:bg-surface-2 hover:text-ink"
      :title="t('最小化窗口')"
      :aria-label="t('最小化窗口')"
      @click="emit('minimize')"
    >
      <Minus :size="13" />
    </button>
    <button
      v-if="maximizable"
      type="button"
      class="flex h-[38px] w-[42px] items-center justify-center text-muted transition-colors hover:bg-surface-2 hover:text-ink"
      :title="maximized ? t('还原窗口') : t('最大化窗口')"
      :aria-label="maximized ? t('还原窗口') : t('最大化窗口')"
      @click="emit('maximize')"
    >
      <Minimize2 v-if="maximized" :size="12" />
      <Square v-else :size="10" />
    </button>
    <button
      type="button"
      class="flex h-[38px] w-[42px] items-center justify-center text-muted transition-colors hover:bg-[#c94343] hover:text-white"
      :title="t('关闭窗口')"
      :aria-label="t('关闭窗口')"
      @click="emit('close')"
    >
      <X :size="14" />
    </button>
  </div>
</template>

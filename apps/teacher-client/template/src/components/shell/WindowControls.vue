<script setup lang="ts">
import { Minus, Minimize2, Square, X } from 'lucide-vue-next'
import { useI18n } from 'vue-i18n'

defineProps<{ maximized: boolean }>()

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
      :title="t('window.minimize')"
      :aria-label="t('window.minimize')"
      @click="emit('minimize')"
    >
      <Minus :size="13" />
    </button>
    <button
      type="button"
      class="flex h-[38px] w-[42px] items-center justify-center text-muted transition-colors hover:bg-surface-2 hover:text-ink"
      :title="maximized ? t('window.restore') : t('window.maximize')"
      :aria-label="maximized ? t('window.restore') : t('window.maximize')"
      @click="emit('maximize')"
    >
      <Minimize2 v-if="maximized" :size="12" />
      <Square v-else :size="10" />
    </button>
    <button
      type="button"
      class="flex h-[38px] w-[42px] items-center justify-center text-muted transition-colors hover:bg-[#c94343] hover:text-white"
      :title="t('window.close')"
      :aria-label="t('window.close')"
      @click="emit('close')"
    >
      <X :size="14" />
    </button>
  </div>
</template>

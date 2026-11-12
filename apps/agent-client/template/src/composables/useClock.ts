import { computed, onMounted, onUnmounted, ref } from 'vue'

import { formatClock, formatDate, formatTimezone } from '@/lib/format'

export function useClock(intervalMs = 15_000) {
  const now = ref(new Date())
  let timer: ReturnType<typeof setInterval> | undefined

  onMounted(() => {
    timer = setInterval(() => {
      now.value = new Date()
    }, intervalMs)
  })

  onUnmounted(() => {
    if (timer) clearInterval(timer)
  })

  const clockLabel = computed(() => formatClock(now.value))
  const dateLabel = computed(() => formatDate(now.value))
  const timezoneLabel = computed(() => formatTimezone(now.value))

  return { now, clockLabel, dateLabel, timezoneLabel }
}

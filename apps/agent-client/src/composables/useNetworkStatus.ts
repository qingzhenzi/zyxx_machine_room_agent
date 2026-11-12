import { onMounted, onUnmounted, ref } from 'vue'

export function useNetworkStatus() {
  const online = ref(typeof navigator === 'undefined' ? true : navigator.onLine)

  function update() {
    online.value = navigator.onLine
  }

  onMounted(() => {
    window.addEventListener('online', update)
    window.addEventListener('offline', update)
  })

  onUnmounted(() => {
    window.removeEventListener('online', update)
    window.removeEventListener('offline', update)
  })

  return { online }
}

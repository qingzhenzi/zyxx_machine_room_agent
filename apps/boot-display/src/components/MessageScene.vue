<script setup lang="ts">
const props = defineProps<{
  params: Record<string, unknown>
}>()

function asText(value: unknown, fallback: string): string {
  return typeof value === 'string' && value ? value : fallback
}

function asLines(value: unknown): string[] {
  if (Array.isArray(value)) return value.filter((v): v is string => typeof v === 'string')
  return []
}
</script>

<template>
  <div class="flex h-full flex-col items-center justify-center">
    <div class="w-[min(52vw,60rem)]">
      <div class="rise text-[2.4vw] font-medium leading-tight">
        {{ asText(props.params.title, '通知') }}
      </div>
      <div class="rise rise-1 mt-[1.8vw] h-[3px] w-[4vw] bg-[var(--blue)]" />
      <p
        v-if="asText(props.params.body, '')"
        class="rise rise-2 mt-[2.6vw] text-[1.35vw] leading-relaxed text-[var(--ink-soft)]"
      >
        {{ asText(props.params.body, '') }}
      </p>
      <ul v-if="asLines(props.params.lines).length" class="rise rise-3 mt-[2vw] space-y-[0.9vw]">
        <li
          v-for="line in asLines(props.params.lines)"
          :key="line"
          class="flex items-start gap-[1.2vw] text-[1.25vw] text-[var(--ink-soft)]"
        >
          <span class="mt-[0.55em] h-[0.32vw] w-[0.32vw] shrink-0 rounded-full bg-[var(--ink-faint)]" />
          {{ line }}
        </li>
      </ul>
    </div>
  </div>
</template>

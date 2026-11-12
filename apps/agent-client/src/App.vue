<script setup lang="ts">
import { onMounted, ref } from 'vue'

import OnboardingWizard from '@/components/onboarding/OnboardingWizard.vue'

const ONBOARDED_KEY = 'qe-agent-client.onboarded.v1'
// 启动即进入初始化检查：不再根据历史标记直接跳过。
// daemon 已就绪时动画会在 ~1.4s 后自动结束；未安装/未注册时会停在安装或注册步骤。
const showOnboarding = ref(true)

function finishOnboarding() {
  try {
    localStorage.setItem(ONBOARDED_KEY, '1')
  } catch {
    // localStorage 不可用时忽略（隐私模式）。
  }
  showOnboarding.value = false
}

onMounted(() => {
  // 兼容旧数据：保留标记只用于记录，不再决定是否执行启动初始化。
  try {
    void localStorage.getItem(ONBOARDED_KEY)
  } catch {
    // localStorage 不可用时忽略。
  }
})
</script>

<template>
  <OnboardingWizard v-if="showOnboarding" @done="finishOnboarding" />
  <RouterView />
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  CheckCircle2,
  Info,
  Layers,
  Menu,
  Sparkles,
  TriangleAlert,
} from 'lucide-vue-next'

import KitCard from '@/components/ui-kit/KitCard.vue'
import AppDrawer from '@/components/ui/dialog/AppDrawer.vue'
import AppModal from '@/components/ui/dialog/AppModal.vue'
import ConfirmDialog from '@/components/ui/dialog/ConfirmDialog.vue'
import FormDialog from '@/components/ui/dialog/FormDialog.vue'
import NoticeDialog from '@/components/ui/dialog/NoticeDialog.vue'
import PopoverMenu, { type PopoverMenuItem } from '@/components/ui/dialog/PopoverMenu.vue'
import DonutChart from '@/components/ui/DonutChart.vue'
import MiniSparkline from '@/components/ui/MiniSparkline.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import ToggleSwitch from '@/components/ui/ToggleSwitch.vue'
import { useUiStore } from '@/stores/ui'

const { t } = useI18n()
const ui = useUiStore()

const modalOpen = ref(false)
const confirmOpen = ref(false)
const noticeOpen = ref(false)
const noticeTone = ref<'success' | 'info' | 'warning'>('success')
const formOpen = ref(false)
const formBusy = ref(false)
const drawerOpen = ref(false)
const menuOpen = ref(false)
const demoSwitch = ref(true)
const form = ref({ name: '', note: '' })

const menuItems: PopoverMenuItem[] = [
  { id: 'copy', label: t('uiKit.menu.copy') },
  { id: 'export', label: t('uiKit.menu.export'), dividerBefore: true },
  { id: 'delete', label: t('uiKit.menu.delete'), danger: true, dividerBefore: true },
]

function notify(kind: 'success' | 'error', title: string) {
  ui.pushToast({ kind, title })
}

function onMenuSelect() {
  menuOpen.value = false
  notify('success', t('uiKit.demo.saved'))
}

function submitForm() {
  formBusy.value = true
  setTimeout(() => {
    formBusy.value = false
    formOpen.value = false
    notify('success', t('uiKit.demo.saved'))
    form.value = { name: '', note: '' }
  }, 350)
}
</script>

<template>
  <div class="h-full overflow-y-auto">
    <header class="flex items-start justify-between gap-6 px-5 pb-3.5 pt-4">
      <div>
        <div class="flex items-center gap-2.5">
          <h1 class="text-[18px] font-semibold tracking-[-0.02em] text-ink">{{ t('uiKit.title') }}</h1>
          <span class="status-badge status-badge-accent">
            <Layers :size="10" />
            UI KIT
          </span>
        </div>
        <p class="mt-1 text-[11px] text-muted">{{ t('uiKit.subtitle') }}</p>
      </div>
      <button type="button" class="btn btn-primary shrink-0" @click="modalOpen = true">
        <Sparkles :size="12" />
        {{ t('uiKit.demo.openDialog') }}
      </button>
    </header>

    <div class="grid grid-cols-3 gap-3 px-5">
      <KitCard
        :title="t('uiKit.dialog')"
        subtitle="AppModal / BaseDialog"
        :description="t('uiKit.dialogDescription')"
        :icon="Layers"
      >
        <button type="button" class="btn btn-sm" @click="modalOpen = true">
          {{ t('uiKit.demo.openDialog') }}
        </button>
      </KitCard>

      <KitCard
        :title="t('uiKit.confirm')"
        subtitle="ConfirmDialog"
        :description="t('uiKit.confirmDescription')"
        :icon="TriangleAlert"
        icon-class="border-danger/20 bg-danger-soft text-danger"
      >
        <button type="button" class="btn btn-sm btn-danger" @click="confirmOpen = true">
          {{ t('uiKit.demo.danger') }}
        </button>
      </KitCard>

      <KitCard
        :title="t('uiKit.notice')"
        subtitle="NoticeDialog"
        :description="t('uiKit.noticeDescription')"
        :icon="Info"
        icon-class="border-info/20 bg-info-soft text-info"
      >
        <button type="button" class="btn btn-sm" @click="noticeTone = 'success'; noticeOpen = true">
          {{ t('uiKit.demo.success') }}
        </button>
        <button type="button" class="btn btn-sm" @click="noticeTone = 'info'; noticeOpen = true">
          {{ t('uiKit.demo.info') }}
        </button>
      </KitCard>

      <KitCard
        :title="t('uiKit.formTitle')"
        subtitle="FormDialog"
        :description="t('uiKit.formDescription')"
        :icon="CheckCircle2"
      >
        <button type="button" class="btn btn-sm" @click="formOpen = true">
          {{ t('uiKit.demo.openForm') }}
        </button>
      </KitCard>

      <KitCard
        :title="t('uiKit.drawerTitle')"
        subtitle="AppDrawer"
        :description="t('uiKit.drawerDescription')"
        :icon="Layers"
      >
        <button type="button" class="btn btn-sm" @click="drawerOpen = true">
          {{ t('uiKit.demo.openDrawer') }}
        </button>
      </KitCard>

      <KitCard
        :title="t('uiKit.menuTitle')"
        subtitle="PopoverMenu"
        :description="t('uiKit.menuDescription')"
        :icon="Menu"
      >
        <div class="relative">
          <button type="button" class="btn btn-sm" @click="menuOpen = !menuOpen">
            {{ t('uiKit.demo.openMenu') }}
          </button>
          <PopoverMenu
            class="left-0 top-full mt-1"
            :open="menuOpen"
            :items="menuItems"
            :aria-label="t('uiKit.menuTitle')"
            @select="onMenuSelect"
            @close="menuOpen = false"
          />
        </div>
      </KitCard>
    </div>

    <section class="grid grid-cols-12 gap-3 px-5 py-3">
      <article class="panel col-span-7 p-4">
        <h2 class="text-[12px] font-semibold text-ink">{{ t('uiKit.toast') }}</h2>
        <p class="mt-0.5 text-[10px] text-muted">{{ t('uiKit.toastDescription') }}</p>
        <div class="mt-3 flex flex-wrap gap-2">
          <button
            type="button"
            class="btn btn-sm"
            @click="notify('success', t('uiKit.demo.saved'))"
          >
            {{ t('uiKit.demo.toastSuccess') }}
          </button>
          <button type="button" class="btn btn-sm" @click="notify('error', t('uiKit.demo.deleted'))">
            {{ t('uiKit.demo.toastError') }}
          </button>
        </div>

        <div class="mt-4 flex items-center justify-between border-t border-line pt-3">
          <span class="text-[10.5px] text-muted">ToggleSwitch</span>
          <ToggleSwitch v-model="demoSwitch" label="ToggleSwitch" />
        </div>
      </article>

      <article class="panel col-span-5 p-4">
        <h2 class="text-[12px] font-semibold text-ink">{{ t('uiKit.charts') }}</h2>
        <p class="mt-0.5 text-[10px] text-muted">{{ t('uiKit.chartsDescription') }}</p>
        <div class="mt-3 flex items-center gap-5">
          <DonutChart :passed="72" :failed="9" :skipped="4" :size="96" :thickness="10" label="uiKit.passRate" />
          <div class="space-y-2">
            <MiniSparkline :values="[12, 18, 15, 22, 19, 27, 24]" :width="110" :height="34" />
            <div class="flex flex-wrap gap-1.5">
              <StatusBadge :label="t('items.status.todo')" tone="muted" />
              <StatusBadge :label="t('items.status.doing')" tone="accent" />
              <StatusBadge :label="t('items.status.done')" tone="success" />
            </div>
          </div>
        </div>
      </article>
    </section>

    <div class="px-5 pb-5">
      <p class="rounded border border-line bg-surface-2 px-3.5 py-2 text-[9.5px] leading-4 text-muted">
        UI primitives live in <span class="mono">src/components/ui/</span>; dialogs share
        <span class="mono">composables/useDialogFocus.ts</span>.
      </p>
    </div>

    <AppModal
      :open="modalOpen"
      :title="t('uiKit.dialog')"
      :description="t('uiKit.dialogDescription')"
      width="620px"
      @close="modalOpen = false"
    >
      <p class="text-[10.5px] leading-5 text-muted">
        {{ t('uiKit.drawer.body') }}
      </p>
      <template #footer>
        <button type="button" class="btn" @click="modalOpen = false">{{ t('common.close') }}</button>
        <button
          type="button"
          class="btn btn-primary"
          @click="modalOpen = false; notify('success', t('uiKit.demo.saved'))"
        >
          {{ t('common.confirm') }}
        </button>
      </template>
    </AppModal>

    <ConfirmDialog
      :open="confirmOpen"
      :title="t('uiKit.confirm')"
      :description="t('uiKit.confirmDescription')"
      :confirm-label="t('common.delete')"
      :cancel-label="t('common.cancel')"
      tone="danger"
      @confirm="confirmOpen = false; notify('error', t('uiKit.demo.deleted'))"
      @cancel="confirmOpen = false"
    />

    <NoticeDialog
      :open="noticeOpen"
      :title="t('uiKit.notice')"
      :description="t('uiKit.noticeDescription')"
      :tone="noticeTone"
      @close="noticeOpen = false"
    />

    <FormDialog
      :open="formOpen"
      :title="t('uiKit.formTitle')"
      :description="t('uiKit.formDescription')"
      :submit-label="t('common.save')"
      :cancel-label="t('common.cancel')"
      :busy="formBusy"
      :submit-disabled="!form.name.trim()"
      @submit="submitForm"
      @cancel="formOpen = false"
    >
      <div class="space-y-3">
        <label class="block">
          <span class="mb-1.5 block text-[10.5px] text-muted">{{ t('uiKit.form.name') }}</span>
          <div class="field">
            <input v-model="form.name" :placeholder="t('uiKit.form.namePlaceholder')" autofocus />
          </div>
        </label>
        <label class="block">
          <span class="mb-1.5 block text-[10.5px] text-muted">{{ t('uiKit.form.note') }}</span>
          <div class="field">
            <input v-model="form.note" :placeholder="t('uiKit.form.notePlaceholder')" />
          </div>
        </label>
      </div>
    </FormDialog>

    <AppDrawer
      :open="drawerOpen"
      :title="t('uiKit.drawer.title')"
      width="440px"
      @close="drawerOpen = false"
    >
      <p class="text-[10.5px] leading-5 text-muted">{{ t('uiKit.drawer.body') }}</p>
      <template #footer>
        <button type="button" class="btn" @click="drawerOpen = false">{{ t('common.close') }}</button>
      </template>
    </AppDrawer>
  </div>
</template>

import { expect, test } from '@playwright/test'

// Minimal smoke coverage for the P3 agent shell in a browser preview. The GUI
// reports a disconnected daemon here (Tauri IPC is only available in the
// desktop build); the real IPC path is covered by Rust unit tests, the
// ignored real-daemon test and manual desktop smoke runs.

test('status page shows the disconnected daemon state with a start guide', async ({ page }) => {
  await page.goto('/')

  await expect(page.getByRole('heading', { name: '机器 Agent 状态' })).toBeVisible()
  await expect(page.getByText('未连接 qe-agent daemon')).toBeVisible()
  await expect(page.getByText('/run/qe-agent/agent.sock').first()).toBeVisible()
  await expect(page.getByRole('button', { name: '重试连接' })).toBeVisible()
  await expect(page.getByText('qe-agent run --role both --ipc-socket', { exact: false })).toBeVisible()
})

test('enroll page validates links locally and surfaces IPC failures', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('button', { name: '注册', exact: true }).first().click()

  await expect(page.getByRole('heading', { name: '注册到 QEStudio Agent' })).toBeVisible()

  // Invalid link: rejected client-side without calling IPC.
  await page.locator('#enroll-link').fill('not-a-link')
  await page.locator('button.btn-primary').click()
  await expect(page.getByText('注册链接格式不正确')).toBeVisible()

  // Valid link: the IPC call fails in a browser preview and is shown in the
  // enrollment outcome panel (never an uncaught exception).
  await page.locator('#enroll-link').fill('https://qe.example/enroll?token=t')
  await page.locator('button.btn-primary').click()
  await expect(page.getByText('注册失败')).toBeVisible()
  await expect(page.getByText('本机 IPC 仅在桌面端可用').first()).toBeVisible()
})

test('logs page reports the disconnected daemon instead of crashing', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('button', { name: '日志', exact: true }).click()

  await expect(page.getByRole('heading', { name: '运行日志' })).toBeVisible()
  await expect(page.getByText('未连接 qe-agent daemon')).toBeVisible()
  await expect(page.getByRole('button', { name: '重试连接' })).toBeVisible()
})

test('settings page shows read-only daemon info with the socket path', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('button', { name: '设置', exact: true }).click()

  await expect(page.getByRole('heading', { name: '设置', exact: true })).toBeVisible()
  await expect(page.getByText('daemon 信息')).toBeVisible()
  await expect(page.getByText('/run/qe-agent/agent.sock').first()).toBeVisible()
})

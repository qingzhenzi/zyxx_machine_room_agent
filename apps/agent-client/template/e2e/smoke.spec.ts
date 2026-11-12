import { expect, test } from '@playwright/test'

test('boots into the workbench', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '概览' })).toBeVisible()
  await expect(page.getByRole('complementary', { name: '主导航' })).toBeVisible()
})

test('opens the command palette with the keyboard', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '概览' })).toBeVisible()
  await page.keyboard.press('Control+k')
  await expect(page.getByRole('dialog', { name: '命令面板' })).toBeVisible()
})

test('creates an item through the dialog', async ({ page }) => {
  await page.goto('/#/items')
  await page.getByRole('button', { name: '新建条目' }).first().click()
  await page.getByPlaceholder('例如：接入真实后端').fill('E2E item')
  await page.getByRole('button', { name: '保存' }).click()
  await expect(page.getByRole('status').getByText('E2E item')).toBeVisible()
})

test('switches language', async ({ page }) => {
  await page.goto('/#/settings')
  await page.getByRole('button', { name: 'English' }).click()
  await expect(page.getByRole('heading', { name: 'Settings' })).toBeVisible()
})

#!/usr/bin/env node
/**
 * Turn the template into a concrete app.
 *
 *   pnpm run init
 *   pnpm run init --name "My App" --identifier com.acme.myapp --scheme myapp --yes
 *
 * Rewrites the product name, package name, bundle identifier, deep-link scheme,
 * localStorage prefix, Cargo crate names, README and LICENSE, then removes
 * itself so the generated project stays clean.
 */
import { readFileSync, writeFileSync, readdirSync, statSync, rmSync } from 'node:fs'
import { join, resolve, dirname, extname } from 'node:path'
import { createInterface } from 'node:readline/promises'
import { fileURLToPath } from 'node:url'
import process from 'node:process'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')

const args = process.argv.slice(2)
function flag(name) {
  const index = args.indexOf(`--${name}`)
  if (index === -1) return undefined
  const value = args[index + 1]
  return value && !value.startsWith('--') ? value : undefined
}
const assumeYes = args.includes('--yes') || args.includes('-y')

function slugify(value) {
  return value
    .toLowerCase()
    .normalize('NFKD')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .replace(/-{2,}/g, '-')
}

const packageJsonPath = join(root, 'package.json')
const pkg = JSON.parse(readFileSync(packageJsonPath, 'utf8'))

const defaults = {
  name: 'Desktop Starter',
  packageName: pkg.name,
  identifier: 'com.example.desktopstarter',
  scheme: 'desktopstarter',
  author: 'Desktop Starter contributors',
  description: pkg.description ?? '',
}

async function ask() {
  const rl = createInterface({ input: process.stdin, output: process.stdout })
  try {
    const name = (flag('name') ?? (await rl.question(`App name (${defaults.name}): `))).trim()
    const packageName = (
      flag('package') ?? (await rl.question(`Package name (${slugify(name) || defaults.packageName}): `))
    ).trim()
    const identifier = (
      flag('identifier') ??
      (await rl.question(`Bundle identifier (com.example.${(slugify(packageName) || 'app').replace(/-/g, '')}): `))
    ).trim()
    const scheme = (
      flag('scheme') ?? (await rl.question(`Deep-link scheme (${(slugify(packageName) || 'app').replace(/-/g, '')}): `))
    ).trim()
    const author = (
      flag('author') ?? (await rl.question(`Author / copyright holder (${defaults.author}): `))
    ).trim()
    const description = (
      flag('description') ?? (await rl.question(`Description (${defaults.description}): `))
    ).trim()

    return {
      name: name || defaults.name,
      packageName: packageName || slugify(name) || defaults.packageName,
      identifier:
        identifier ||
        `com.example.${(slugify(packageName || name) || 'app').replace(/-/g, '')}`,
      scheme: scheme || (slugify(packageName || name) || 'app').replace(/-/g, ''),
      author: author || defaults.author,
      description: description || defaults.description,
    }
  } finally {
    rl.close()
  }
}

const answers = assumeYes
  ? {
      name: flag('name') ?? defaults.name,
      packageName: flag('package') ?? slugify(flag('name') ?? defaults.name),
      identifier:
        flag('identifier') ??
        `com.example.${(slugify(flag('package') ?? flag('name') ?? 'app') || 'app').replace(/-/g, '')}`,
      scheme:
        flag('scheme') ??
        (slugify(flag('package') ?? flag('name') ?? 'app') || 'app').replace(/-/g, ''),
      author: flag('author') ?? defaults.author,
      description: flag('description') ?? defaults.description,
    }
  : await ask()

const { name, identifier, scheme, author, description } = answers
const packageName = slugify(answers.packageName)
const libName = `${packageName.replace(/-/g, '_')}_lib`

function fail(message) {
  console.error(`\n✖ ${message}\n`)
  process.exit(1)
}

if (!name) fail('App name is required.')
if (!/^[a-z][a-z0-9-]*$/.test(packageName)) {
  fail(`Package name "${packageName}" must be lowercase letters, digits and dashes.`)
}
if (!/^[A-Za-z][A-Za-z0-9.-]*\.[A-Za-z0-9.-]+$/.test(identifier)) {
  fail(`Bundle identifier "${identifier}" must be reverse-DNS like com.acme.myapp.`)
}
if (!/^[a-z][a-z0-9+.-]*$/.test(scheme)) {
  fail(`Deep-link scheme "${scheme}" must start with a letter.`)
}

const replacements = [
  // Longest / most specific first.
  ['Desktop Starter contributors', author],
  ['com.example.desktopstarter', identifier],
  ['desktop_starter_lib', libName],
  ['desktopstarter://', `${scheme}://`],
  ['desktopstarter', scheme],
  ['desktop-starter', packageName],
  ['Desktop Starter', name],
  ['一个可运行的 Tauri 2 + Vue 3 桌面应用起手模板', '一个 Tauri 2 + Vue 3 桌面应用'],
]

const SKIP_DIRS = new Set(['node_modules', 'dist', '.git', 'target', 'gen', '.vite'])
const TEXT_EXTENSIONS = new Set([
  '.ts', '.vue', '.json', '.toml', '.rs', '.html', '.md', '.mjs', '.js',
  '.css', '.svg', '.yml', '.yaml', '.txt', '.example', '.editorconfig',
])
const SKIP_FILES = new Set(['pnpm-lock.yaml', 'Cargo.lock'])

function rewriteFile(path) {
  const base = path.split('/').pop() ?? ''
  const extension = extname(path)
  const isText =
    TEXT_EXTENSIONS.has(extension) ||
    base === 'LICENSE' ||
    base === '.editorconfig' ||
    base === '.env.example'
  if (!isText) return false
  if (SKIP_FILES.has(base)) return false

  const original = readFileSync(path, 'utf8')
  let next = original
  for (const [from, to] of replacements) {
    if (from !== to) next = next.split(from).join(to)
  }
  if (next === original) return false
  writeFileSync(path, next)
  return true
}

function walk(directory) {
  for (const entry of readdirSync(directory)) {
    const path = join(directory, entry)
    if (statSync(path).isDirectory()) {
      if (SKIP_DIRS.has(entry)) continue
      walk(path)
    } else {
      rewriteFile(path)
    }
  }
}

walk(root)

// package.json: set the final identity and drop the now-unused init script.
const updated = JSON.parse(readFileSync(packageJsonPath, 'utf8'))
updated.name = packageName
updated.version = '0.1.0'
updated.description = description || updated.description
if (updated.scripts) delete updated.scripts.init
writeFileSync(packageJsonPath, `${JSON.stringify(updated, null, 2)}\n`)

// README: drop the "use this template" section and the screenshots, which
// still carry the template's product name.
const readmePath = join(root, 'README.md')
const readme = readFileSync(readmePath, 'utf8')
const stripped = readme
  .replace(/<!-- template:start -->[\s\S]*?<!-- template:end -->\n*/, '')
  .replace(/## 预览\n\n!\[[^\]]*\]\(docs\/[^)]*\)\n\n!\[[^\]]*\]\(docs\/[^)]*\)\n\n/, '')
writeFileSync(readmePath, stripped.replace(/\n{3,}/g, '\n\n'))
rmSync(join(root, 'docs'), { recursive: true, force: true })

rmSync(join(root, 'scripts', 'init.mjs'), { force: true })

console.log(`
✔ ${name}
  package    ${packageName}
  identifier ${identifier}
  scheme     ${scheme}://
  crate      ${libName}
  storage    ${packageName}.*

Next:
  pnpm install
  pnpm dev
  pnpm tauri dev
`)

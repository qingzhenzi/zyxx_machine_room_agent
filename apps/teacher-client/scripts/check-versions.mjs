import { readFileSync } from 'node:fs'

const packageJson = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'))
const tauriConfig = JSON.parse(
  readFileSync(new URL('../src-tauri/tauri.conf.json', import.meta.url), 'utf8'),
)
const cargoToml = readFileSync(new URL('../src-tauri/Cargo.toml', import.meta.url), 'utf8')
const cargoVersion = cargoToml.match(/^version\s*=\s*"([^"]+)"/m)?.[1]

const versions = {
  'package.json': packageJson.version,
  'tauri.conf.json': tauriConfig.version,
  'Cargo.toml': cargoVersion,
}

const unique = new Set(Object.values(versions))
if (unique.size !== 1 || !versions['Cargo.toml']) {
  console.error('Version mismatch:')
  for (const [file, version] of Object.entries(versions)) {
    console.error(`  ${file}: ${version ?? 'missing'}`)
  }
  process.exit(1)
}

console.log(`Version ${packageJson.version} is consistent across package.json, tauri.conf.json and Cargo.toml.`)

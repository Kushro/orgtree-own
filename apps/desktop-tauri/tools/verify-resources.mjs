// Verifica los recursos del instalador con los mismos controles que el
// empaquetado de Electron (tools/package-preflight.mjs): la disposición del
// runtime embebido y cada byte de PostgreSQL contra su manifiesto.
//
//   node apps/desktop-tauri/tools/verify-resources.mjs <recursos> [--compare <otros-recursos>]
//
// <recursos> es bundle-resources/ (antes de empaquetar) o la carpeta donde
// quedó instalada la app. Con --compare, además exige que el runtime y el
// motor sean idénticos en las dos carpetas (lo armado contra lo instalado).
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { assertRuntimeLayout, runtimeTreeDigest } from '../../../tools/runtime-layout.mjs'
import { assertPostgresRuntime } from '../../../tools/postgres-layout.mjs'

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..')
const [dir, flag, other] = process.argv.slice(2)
if (!dir || (flag && flag !== '--compare') || (flag && !other)) {
  console.error('uso: verify-resources.mjs <recursos> [--compare <otros-recursos>]')
  process.exit(2)
}

function walk(root, out = []) {
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    const full = path.join(root, entry.name)
    if (entry.isSymbolicLink()) throw new Error(`enlace en los recursos: ${full}`)
    if (entry.isDirectory()) walk(full, out)
    else out.push(full)
  }
  return out
}

function inspect(resources) {
  const engine = path.join(resources, 'engine')
  const runtime = assertRuntimeLayout(path.join(engine, 'runtime'), { label: `${resources} engine/runtime` })
  const postgres = assertPostgresRuntime(engine, { sourceRoot: repo })
  for (const file of ['engine/launch.py', 'engine/mailhub/mailhub/__init__.py', 'tools/pypg/pgimport.py',
    'tools/pypg/cutover_verify.py', 'ui/index.html', 'build-info.json']) {
    if (!fs.statSync(path.join(resources, file), { throwIfNoEntry: false })?.isFile()) throw new Error(`falta ${file} en ${resources}`)
  }
  const files = walk(engine)
  const dev = files.filter(file => /[\\/](__pycache__|\.git)([\\/]|$)/.test(file) || file.endsWith('.pyc')
    || /[\\/]native[\\/].*[\\/]target[\\/]/.test(path.relative(engine, file)))
  if (dev.length) throw new Error(`archivos de desarrollo en ${engine}:\n  ${dev.slice(0, 20).join('\n  ')}`)
  const bytes = files.reduce((sum, file) => sum + fs.statSync(file).size, 0)
  return {
    resources,
    engineFiles: files.length,
    engineMB: +(bytes / 2 ** 20).toFixed(1),
    runtimeDependencies: runtime.manifest.dependencies.length,
    runtimeDigest: runtimeTreeDigest(path.join(engine, 'runtime')),
    postgres: { files: postgres.files, MB: +(postgres.bytes / 2 ** 20).toFixed(1) },
  }
}

const result = inspect(path.resolve(dir))
if (other) {
  const second = inspect(path.resolve(other))
  if (second.runtimeDigest !== result.runtimeDigest) throw new Error('el runtime instalado no es idéntico al armado')
  if (second.engineFiles !== result.engineFiles) throw new Error(`el motor tiene ${second.engineFiles} archivos en ${other} y ${result.engineFiles} en ${dir}`)
  result.compared = second
}
console.log(JSON.stringify(result, null, 2))

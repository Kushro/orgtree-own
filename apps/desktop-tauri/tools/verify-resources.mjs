// Verifica los recursos del instalador con los mismos controles que el
// empaquetado de Electron (tools/package-preflight.mjs): la disposición del
// runtime embebido y cada byte de PostgreSQL contra su manifiesto.
//
//   node apps/desktop-tauri/tools/verify-resources.mjs <recursos> [--compare <otros-recursos>]
//
// <recursos> es bundle-resources/ (antes de empaquetar) o la carpeta donde
// quedó instalada la app. Con --compare, además exige que el runtime y el
// motor sean idénticos en las dos carpetas (lo armado contra lo instalado).
import crypto from 'node:crypto'
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

// Ruta relativa → tamaño y SHA-256 de cada archivo del motor.
function hashes(engine) {
  const out = new Map()
  for (const file of walk(engine)) {
    const data = fs.readFileSync(file)
    out.set(path.relative(engine, file).split(path.sep).join('/'), `${data.length}:${crypto.createHash('sha256').update(data).digest('hex')}`)
  }
  return out
}

const result = inspect(path.resolve(dir))
if (other) {
  const second = inspect(path.resolve(other))
  const [mine, theirs] = [hashes(path.join(path.resolve(dir), 'engine')), hashes(path.join(path.resolve(other), 'engine'))]
  const missing = [...mine.keys()].filter(file => !theirs.has(file))
  const extra = [...theirs.keys()].filter(file => !mine.has(file))
  const changed = [...mine.keys()].filter(file => theirs.has(file) && theirs.get(file) !== mine.get(file))
  if (missing.length || extra.length || changed.length) {
    const show = list => list.slice(0, 25).join('\n    ') + (list.length > 25 ? `\n    (+${list.length - 25})` : '')
    throw new Error(`el motor instalado no es idéntico al armado: ${missing.length} faltan, ${extra.length} sobran, ${changed.length} distintos`
      + `\n  faltan:\n    ${show(missing)}\n  sobran:\n    ${show(extra)}\n  distintos:\n    ${show(changed)}`)
  }
  result.compared = second
}
console.log(JSON.stringify(result, null, 2))

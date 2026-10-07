// Rust actualiza esta página con el estado del arranque (sin token, sin el puente).
const $ = id => document.getElementById(id)
// #19: el estado del arranque. `starting`/`progress` muestran la fase; `converting`
// el mensaje de la ventana de conversión de Electron con el paso en curso; `failed`
// un rechazo con su motivo y los botones Reintentar y Salir.
window.orgtreeSplash = state => {
  const failed = state.state === 'failed'
  $('progress').hidden = failed
  $('failure').hidden = !failed
  if (failed) {
    $('failure-title').textContent = state.title || 'El motor no pudo arrancar'
    $('failure-message').textContent = state.message || ''
    $('failure-detail').textContent = state.detail || ''
    $('failure-detail').hidden = !state.detail
    $('retry').hidden = state.retry === false
    $('retry').disabled = false
    return
  }
  $('status').textContent = state.text || 'Iniciando…'
  $('phase').textContent = state.phase || ''
}
// Compatibilidad: texto suelto.
window.orgtreeEngineStatus = text => window.orgtreeSplash({ state: 'progress', text })
// La raíz de datos en uso (#18): siempre propia de la app, nunca la de Orgtree instalado.
window.orgtreeLaunch = launch => {
  $('data-root').textContent = launch.dataRoot
  $('mode').textContent = launch.mode === 'packaged'
    ? 'Motor empaquetado (Python embebido, PostgreSQL 18)'
    : 'Motor de desarrollo (ORGTREE_TAURI_PYTHON)'
  $('launch').hidden = false
}
const invoke = cmd => {
  const internals = window.__TAURI_INTERNALS__
  return internals && typeof internals.invoke === 'function' ? internals.invoke(cmd) : Promise.reject(new Error('sin IPC'))
}
$('retry').addEventListener('click', () => {
  $('retry').disabled = true
  window.orgtreeSplash({ state: 'starting', text: 'Reintentando…' })
  invoke('splash_retry').catch(error => {
    window.orgtreeSplash({ state: 'failed', title: 'No se pudo reintentar', message: String(error), retry: true })
  })
})
$('quit').addEventListener('click', () => { invoke('splash_quit').catch(() => {}) })

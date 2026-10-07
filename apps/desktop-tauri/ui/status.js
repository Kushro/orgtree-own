// Rust actualiza este texto con el estado del motor (sin IPC ni token).
window.orgtreeEngineStatus = text => { document.getElementById('status').textContent = text }
// La raíz de datos en uso (#18): siempre propia de la app, nunca la de Orgtree instalado.
window.orgtreeLaunch = launch => {
  document.getElementById('data-root').textContent = launch.dataRoot
  document.getElementById('mode').textContent = launch.mode === 'packaged'
    ? 'Motor empaquetado (Python embebido, PostgreSQL 18)'
    : 'Motor de desarrollo (ORGTREE_TAURI_PYTHON)'
  document.getElementById('launch').hidden = false
}

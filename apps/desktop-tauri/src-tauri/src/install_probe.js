// Prueba del instalador (#18), solo con ORGTREE_TAURI_INSTALL_PROBE. Corre en
// la página del motor empaquetado y devuelve el resultado por document.title,
// sin IPC. __PREFIX__ lo reemplaza Rust. Crea una org en la raíz descartable
// del CI para verificar que el almacén (PostgreSQL) escribe y lee.
(async () => {
  if (window.__orgtreeInstallProbe) return;
  window.__orgtreeInstallProbe = true;
  const r = {};
  const pause = ms => new Promise(done => setTimeout(done, ms));
  const waitFor = async (test, ms) => {
    const end = Date.now() + ms;
    while (Date.now() < end) { const value = test(); if (value) return value; await pause(200) }
    return null;
  };
  try { r.nav = performance.getEntriesByType('navigation')[0].responseStatus } catch (e) { r.nav = String(e) }
  try { r.http = (await fetch('/api/desktop/identity')).status } catch (e) { r.http = String(e) }
  const bridge = window.orgtreeDesktop;
  r.bridge = !!bridge;
  if (bridge) {
    try { r.status = (await bridge.getStatus()).state } catch (e) { r.status = 'error:' + e }
  }
  const root = document.querySelector('#root');
  r.rendered = !!(root && await waitFor(() => root.children.length, 30000));
  r.text = (document.body.innerText || '').trim().slice(0, 200);
  try {
    const created = await fetch('/api/orgs', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'instalado', net_autoconnect: false }),
    });
    r.create = created.status;
    if (!created.ok) r.createError = (await created.text()).slice(0, 500);
  } catch (e) { r.create = String(e) }
  try {
    const listed = await fetch('/api/orgs');
    r.list = listed.status;
    const orgs = await listed.json();
    r.orgs = Array.isArray(orgs) ? orgs.map(o => o && (o.slug || o.name)) : orgs;
  } catch (e) { r.list = String(e) }
  r.orgShown = !!(await waitFor(() => (document.body.innerText || '').includes('instalado'), 15000));
  document.title = '__PREFIX__' + JSON.stringify(r);
})();

// Etapa final de la prueba (#21, ver probe.rs), cuando el CI la pide. Va al
// final porque tapa la ventana: el clic en un toast abre la bandeja de entrada
// del renderer, y el Explorador y el navegador se abren de verdad.
// __PREFIX__, __CLICK__ y __TARGET__ los pone Rust.
(async () => {
  if (window.__orgtreeLateProbeRan) return;
  window.__orgtreeLateProbeRan = true;
  const r = {};
  const bridge = window.orgtreeDesktop;
  const pause = ms => new Promise(done => setTimeout(done, ms));
  const waitFor = async (test, ms) => {
    const end = Date.now() + ms;
    while (Date.now() < end) { const value = test(); if (value) return value; await pause(200) }
    return null;
  };
  const attempt = async fn => { try { return await fn() } catch (e) { return 'error:' + (e && e.message || e) } };
  const org = 'spike-fixture';
  // El clic en el toast de la pregunta que sigue pendiente (fixture-q2), con la
  // ventana minimizada: la ventana vuelve con el foco, el renderer recibe
  // notification-click y abre el elemento.
  await attempt(() => bridge.minimizeWindow());
  await pause(800);
  const clicks = [];
  const off = bridge.onEvent(event => { if (event.type === 'notification-click') clicks.push(event.data) });
  document.title = '__CLICK__' + JSON.stringify({ org, id: 'fixture-q2' });
  await waitFor(() => clicks.length, 5000);
  off();
  r.click = clicks[0] ? { id: clicks[0].id, org: clicks[0].org, kind: clicks[0].kind } : null;
  r.clickFocus = !!(await waitFor(() => document.hasFocus(), 5000));
  await pause(1500);
  r.clickText = (document.body.innerText || '').slice(0, 300);
  document.title = 'orgtree-probe-pause:clicked';
  await pause(2500);
  // Nada pendiente: el renderer sincroniza (retira el último toast) y para el parpadeo.
  r.publishEmpty = await attempt(async () => (await fetch('/api/fixture/notices', {
    method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ notices: [] }),
  })).status);
  await attempt(() => bridge.setPreferences({}));
  await pause(2500);
  // Un .cmd: el Explorador lo tiene que seleccionar, nunca ejecutar.
  try { r.reveal = await bridge.revealFile(__TARGET__) } catch (e) { r.reveal = 'error:' + e }
  // Captura del Explorador con el archivo seleccionado, antes de abrir otras ventanas.
  await pause(2500);
  document.title = 'orgtree-probe-pause:revealed';
  await pause(2500);
  try { r.charters = await bridge.openCharterFolder() } catch (e) { r.charters = 'error:' + e }
  try { await bridge.openHarnessLink('codex'); r.harnessLink = 'resolved' } catch (e) { r.harnessLink = 'error:' + e }
  r.target = __TARGET__;
  document.title = '__PREFIX__' + JSON.stringify(r);
})();

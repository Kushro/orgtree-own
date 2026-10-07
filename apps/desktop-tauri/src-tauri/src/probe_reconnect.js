// Etapa de reconexión de la prueba (ver probe.rs): corre en la página recargada
// después de que el motor se cayó y el shell lo reinició. __PREFIX__ lo pone Rust.
(async () => {
  const r = {};
  const timeout = (ms, value) => new Promise(done => setTimeout(() => done(value), ms));
  const waitFor = async (test, ms) => {
    const end = Date.now() + ms;
    while (Date.now() < end) { const value = test(); if (value) return value; await timeout(200) }
    return null;
  };
  try { r.nav = performance.getEntriesByType('navigation')[0].responseStatus } catch (e) { r.nav = String(e) }
  try { r.http = (await fetch('/api/desktop/identity')).status } catch (e) { r.http = String(e) }
  r.ws = await new Promise(done => {
    const socket = new WebSocket(`ws://${location.host}/api/orgs/spike-fixture/ws`);
    const timer = setTimeout(() => done('timeout'), 10000);
    socket.onopen = () => { clearTimeout(timer); socket.close(); done('open') };
    socket.onclose = event => { clearTimeout(timer); done('close:' + event.code) };
  });
  r.bridge = !!window.orgtreeDesktop;
  r.path = location.pathname;
  const root = document.querySelector('#root');
  r.rendered = !!(root && await waitFor(() => root.children.length, 15000));
  r.agentShown = !!(await waitFor(() => document.body.innerText.includes('worker'), 20000));
  document.title = '__PREFIX__' + JSON.stringify(r);
})();

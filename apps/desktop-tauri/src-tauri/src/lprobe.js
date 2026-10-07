// Prueba del ciclo de vida del motor (#19, ver lprobe.rs). El director en Rust
// evalúa este archivo en la ventana y llama un paso: `__lprobe.run('<paso>', <args>)`.
// Cada paso devuelve su resultado por document.title (`orgtree-lprobe:<paso>:<json>`).
// Los eventos `engine-status` y `maintenance` del puente se guardan en
// sessionStorage, que sobrevive a la recarga de la ventana tras la caída.
window.__lprobe = window.__lprobe || (() => {
  const KEY = 'orgtree-lprobe-events';
  const pause = ms => new Promise(done => setTimeout(done, ms));
  const waitFor = async (test, ms) => {
    const end = Date.now() + ms;
    while (Date.now() < end) { let value = null; try { value = test() } catch (e) {} if (value) return value; await pause(200) }
    return null;
  };
  const attempt = async fn => { try { return await fn() } catch (e) { return 'error:' + (e && e.message || e) } };
  const bridge = () => window.orgtreeDesktop;
  const events = () => { try { return JSON.parse(sessionStorage.getItem(KEY) || '[]') } catch (e) { return [] } };
  const listen = () => {
    if (window.__lprobeListening || !bridge()) return;
    window.__lprobeListening = true;
    bridge().onEvent(event => {
      if (event.type !== 'engine-status' && event.type !== 'maintenance') return;
      const list = events();
      list.push({ type: event.type, data: event.data, at: Date.now() });
      try { sessionStorage.setItem(KEY, JSON.stringify(list)) } catch (e) {}
    });
  };
  const listed = () => [...document.querySelectorAll('#root *')].some(el => el.children.length === 0 && el.textContent.trim() === 'spike-fixture');
  const notice = () => { const el = document.getElementById('orgtree-engine-notice'); return el ? el.textContent : null };

  const steps = {
    // Recién abierta: el estado del motor y del mantenimiento en el puente.
    async ready() {
      listen();
      return {
        bridge: !!bridge(),
        status: await attempt(() => bridge().getStatus()),
        maintenanceFn: typeof (bridge() && bridge().getMaintenanceStatus),
        maintenance: await attempt(() => bridge().getMaintenanceStatus()),
        listed: !!(await waitFor(listed, 20000)),
        path: location.pathname,
      };
    },
    // Con el motor caído: el aviso en la ventana y el evento que llegó.
    async down() {
      listen();
      await waitFor(notice, 5000);
      return { status: await attempt(() => bridge().getStatus()), notice: notice(), events: events() };
    },
    // Después de la carga fallida y el reinicio: la página volvió y se autentica.
    async recovered() {
      listen();
      const r = { path: location.pathname, bridge: !!bridge() };
      try { r.nav = performance.getEntriesByType('navigation')[0].responseStatus } catch (e) { r.nav = String(e) }
      r.status = await attempt(() => bridge().getStatus());
      try { r.http = (await fetch('/api/desktop/identity')).status } catch (e) { r.http = String(e) }
      r.ws = await new Promise(done => {
        const socket = new WebSocket(`ws://${location.host}/api/orgs/spike-fixture/ws`);
        const timer = setTimeout(() => done('timeout'), 10000);
        socket.onopen = () => { clearTimeout(timer); socket.close(); done('open') };
        socket.onclose = event => { clearTimeout(timer); done('close:' + event.code) };
      });
      r.listed = !!(await waitFor(listed, 20000));
      r.notice = notice();
      r.events = events();
      return r;
    },
    // Un pedido de mantenimiento del motor (ruta del fixture, como la de un agente).
    async maintenance(args) {
      listen();
      return attempt(async () => {
        const response = await fetch('/api/fixture/maintenance', {
          method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ action: args.action }),
        });
        return { status: response.status, body: await response.json() };
      });
    },
    // Lo que reportó el shell: getMaintenanceStatus y el evento `maintenance`.
    async maintenanceStatus() {
      listen();
      await waitFor(() => events().some(e => e.type === 'maintenance'), 10000);
      return {
        maintenance: await attempt(() => bridge().getMaintenanceStatus()),
        status: await attempt(() => bridge().getStatus()),
        events: events().filter(e => e.type === 'maintenance'),
      };
    },
  };

  return {
    async run(step, args) {
      let result;
      try { result = await steps[step](args || {}) } catch (e) { result = { error: String(e && e.stack || e) } }
      document.title = 'orgtree-lprobe:' + step + ':' + JSON.stringify(result);
    },
  };
})();

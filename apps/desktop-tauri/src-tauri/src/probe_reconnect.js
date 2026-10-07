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

  // #7: ventana sin marco, controles propios y notificación nativa.
  const native = r.native = {};
  const bridge = window.orgtreeDesktop;
  native.controls = !!(await waitFor(() => document.querySelector('.window-controls'), 10000));
  native.controlButtons = document.querySelectorAll('.window-controls button').length;
  if (bridge) {
    const states = [];
    const off = bridge.onEvent(event => { if (event.type === 'window-state') states.push(event.data.maximized) });
    try {
      await bridge.toggleMaximizeWindow();
      native.maximized = !!(await waitFor(() => states.includes(true), 5000));
      native.maximizedIcon = !!(await waitFor(() => document.querySelector('.window-controls [aria-label="Restore window"]'), 5000));
      await bridge.toggleMaximizeWindow();
      native.restored = !!(await waitFor(() => states.length && states[states.length - 1] === false, 5000));
    } catch (e) { native.maximizeError = String(e) }
    off();
    try {
      const n = { id: 'probe-1', title: 'Orgtree', body: 'Notificación nativa del spike de Tauri', org: 'spike-fixture', kind: 'question' };
      native.notify = await bridge.notify(n);
      native.notifyAgain = await bridge.notify(n);
    } catch (e) { native.notify = String(e) }
  }
  // Una zona de arrastre del renderer (`-webkit-app-region: drag`) que esté a la
  // vista y no tapada: el CI la arrastra con el mouse real.
  const region = el => { const s = getComputedStyle(el); return s.getPropertyValue('app-region') || s.getPropertyValue('-webkit-app-region') || s.webkitAppRegion || '' };
  for (const el of document.querySelectorAll('body *')) {
    if (region(el) !== 'drag') continue;
    const b = el.getBoundingClientRect();
    if (b.width < 40 || b.height < 8) continue;
    const points = [[b.left + 12, b.top + b.height / 2], [b.left + b.width / 2, b.top + b.height / 2], [b.right - 12, b.top + b.height / 2]];
    const hit = points.find(([x, y]) => {
      if (x < 0 || y < 0 || x > innerWidth || y > innerHeight) return false;
      const at = document.elementFromPoint(x, y);
      return at && region(at) === 'drag';
    });
    if (hit) {
      native.drag = { x: Math.round(hit[0]), y: Math.round(hit[1]), dpr: devicePixelRatio, cls: String(el.className).slice(0, 80) };
      break;
    }
  }
  document.title = '__PREFIX__' + JSON.stringify(r);
})();

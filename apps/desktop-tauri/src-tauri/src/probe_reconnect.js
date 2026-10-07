// Etapa de reconexión de la prueba (ver probe.rs): corre en la página recargada
// después de que el motor se cayó y el shell lo reinició. __PREFIX__ lo pone Rust.
(async () => {
  if (window.__orgtreeReconnectProbeRan) return;
  window.__orgtreeReconnectProbeRan = true;
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

  // #7: ventana sin marco y controles propios (las notificaciones, en #21 más abajo).
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
  }
  // #21: integraciones. Lo que abre el Explorador y el navegador va en la etapa
  // final (probe_late.js), para no tapar la ventana antes de la prueba de arrastre.
  const integ = r.integrations = {};
  // Una sola vez por pestaña y con el motor vivo: la página puede cargar también
  // contra el motor caído, y una segunda pasada ensuciaría los registros del shell.
  let firstPass = false;
  try { firstPass = !sessionStorage.getItem('orgtree-probe-integrations') } catch (e) { firstPass = true }
  if (bridge && r.agentShown && r.http === 200 && firstPass) {
    try { sessionStorage.setItem('orgtree-probe-integrations', '1') } catch (e) {}
    const org = 'spike-fixture';
    const attempt = async fn => { try { return await fn() } catch (e) { return 'error:' + (e && e.message || e) } };
    // Harnesses: la forma del contrato; el CI pone un `codex` falso en el PATH.
    integ.harnesses = await attempt(() => bridge.getHarnesses());
    integ.harnessBadId = await attempt(() => bridge.openHarnessLink('https://example.com'));
    // Archivos: solo las validaciones (la apertura real va al final).
    integ.revealRelative = await attempt(() => bridge.revealFile('relativo\\archivo.txt'));
    integ.revealMissing = await attempt(() => bridge.revealFile(__MISSING__));
    integ.revealDotted = await attempt(() => bridge.revealFile('C:\\Windows\\..\\Windows\\win.ini'));

    // Login: un proveedor no detectado se niega; el ciclo de estado con el CLI falso.
    const login = integ.login = {};
    const harnesses = Array.isArray(integ.harnesses) ? integ.harnesses : [];
    const missing = harnesses.find(h => !h.detected && h.id !== 'antigravity') || harnesses.find(h => !h.detected);
    login.missingId = missing && missing.id;
    if (missing) login.missing = await attempt(() => bridge.startProviderLogin(missing.id));
    login.badProvider = await attempt(() => bridge.startProviderLogin('codex --help'));
    login.idle = await attempt(() => bridge.getProviderLoginStatus('codex'));
    if (harnesses.some(h => h.id === 'codex' && h.detected)) {
      login.start = await attempt(() => bridge.startProviderLogin('codex'));
      login.again = await attempt(() => bridge.startProviderLogin('codex'));
      const end = Date.now() + 15000;
      let status = null;
      while (Date.now() < end) {
        status = await attempt(() => bridge.getProviderLoginStatus('codex'));
        if (status && typeof status.output === 'string' && status.output.includes('fake-codex')) break;
        await timeout(300);
      }
      login.running = status;
      login.code = await attempt(() => bridge.submitProviderLoginCode('codex', '123'));
      login.cancel = await attempt(() => bridge.cancelProviderLogin('codex'));
      login.after = await attempt(() => bridge.getProviderLoginStatus('codex'));
    }

    // Notificaciones de punta a punta: el motor de fixture publica avisos
    // (`PUT /api/fixture/notices`) y el renderer real los pide al puente. El
    // shell anota cada decisión en `<salida>.notify`, `.sync` y `.attention`.
    // Un cambio de preferencias (aunque sea vacío) hace releer al renderer.
    const notes = integ.notify = {};
    const row = (id, kind, title) => ({ id, org, kind, title, body: `Aviso de prueba ${id}`, agent: 'worker', source_id: id });
    const Q1 = row('fixture-q1', 'question', 'Pregunta 1'), R1 = row('fixture-r1', 'routine', 'Correo 1'), Q2 = row('fixture-q2', 'question', 'Pregunta 2');
    const publish = async rows => {
      const status = await attempt(async () => (await fetch('/api/fixture/notices', {
        method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ notices: rows }),
      })).status);
      await attempt(() => bridge.setPreferences({}));
      await timeout(2500);
      return status;
    };
    const prefs = await attempt(() => bridge.getPreferences());
    notes.prefKeys = prefs && typeof prefs === 'object' ? ['notifyQuestions', 'notifyAllMail', 'notifyWhileFocused'].map(k => typeof prefs[k]) : prefs;
    // 1. Sin foco (ventana minimizada): la pregunta se muestra y la barra de tareas parpadea;
    //    el correo de rutina no (tipo apagado por defecto).
    await attempt(() => bridge.minimizeWindow());
    await timeout(800);
    notes.minimizedFocus = document.hasFocus();
    notes.publish1 = await publish([Q1, R1]);
    document.title = 'orgtree-probe-pause:taskbar';
    await timeout(3000);
    // Llamadas directas: el filtro del shell, no el del renderer.
    notes.directRoutine = await attempt(() => bridge.notify(R1));
    notes.directDuplicate = await attempt(() => bridge.notify(Q1));
    notes.directInactive = await attempt(() => bridge.notify(row('fixture-zz', 'question', 'No publicada')));
    notes.invalidKind = await attempt(() => bridge.notify({ ...Q1, kind: 'nope' }));
    notes.unknownField = await attempt(() => bridge.notify({ ...Q1, url: 'https://example.com' }));
    // 2. Prender "todo el correo": el renderer pide la de rutina y se muestra.
    await attempt(() => bridge.setPreferences({ notifyAllMail: true }));
    await timeout(2500);
    await attempt(() => bridge.setPreferences({ notifyAllMail: false }));
    await timeout(1500);
    // 3. Con la ventana al frente: una pregunta nueva no se muestra hasta
    //    prender "notificar con Orgtree enfocado".
    await attempt(() => bridge.showMainWindow());
    notes.shownFocus = !!(await waitFor(() => document.hasFocus(), 5000));
    notes.publish2 = await publish([Q1, Q2]);
    await attempt(() => bridge.setPreferences({ notifyWhileFocused: true }));
    await timeout(2500);
    await attempt(() => bridge.setPreferences({ notifyWhileFocused: false }));
    // 4. Q1 se resuelve: el renderer sincroniza y el shell retira su toast.
    notes.publish3 = await publish([Q2]);
    notes.syncInvalid = await attempt(() => bridge.syncNotifications([{ org, id: Q2.id, extra: 1 }]));
    notes.attentionMismatch = await attempt(() => bridge.setPendingAttention(['a'], [{ org, id: 'a' }, { org, id: 'b' }]));
    // 5 y 6 (el clic en el toast y "nada pendiente") van en probe_late.js: el clic
    // abre la bandeja de entrada del renderer, que taparía la zona de arrastre.
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

// Prueba de ventanas por organización (#20, ver wprobe.rs). El director en
// Rust evalúa este archivo en una ventana y después llama un paso:
// `__wprobe.run('<paso>', <args>)`. Cada paso devuelve su resultado por
// document.title (`orgtree-wprobe:<paso>:<json>`), sin IPC propio: lo único
// que usa del puente es lo mismo que usa el renderer.
window.__wprobe = window.__wprobe || (() => {
  const pause = ms => new Promise(done => setTimeout(done, ms));
  const waitFor = async (test, ms) => {
    const end = Date.now() + ms;
    while (Date.now() < end) { let value = null; try { value = test() } catch (e) {} if (value) return value; await pause(200) }
    return null;
  };
  const attempt = async fn => { try { return await fn() } catch (e) { return 'error:' + (e && e.message || e) } };
  const bridge = () => window.orgtreeDesktop;
  const text = () => (document.body && document.body.innerText || '');
  const leaf = value => [...document.querySelectorAll('#root *')].find(el => el.children.length === 0 && el.textContent.trim() === value);
  const button = label => [...document.querySelectorAll('#root button')].find(el => el.textContent.trim() === label);
  const setInput = (input, value) => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    setter.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  };
  const identity = async () => ({ sync: bridge() && bridge().windowIdentity, live: await attempt(() => bridge().getWindowIdentity()) });
  const shot = async name => { document.title = 'orgtree-wprobe-pause:' + name; await pause(2500) };
  const createOrg = async name => attempt(async () => (await fetch('/api/orgs', {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name, net_autoconnect: false }),
  })).status);

  const steps = {
    // La Homepage de la primera ventana: identidad, puente de varias ventanas y la org sembrada.
    async home() {
      const r = {};
      r.bridge = !!bridge();
      r.requestOrg = typeof (bridge() && bridge().requestOrg);
      r.identity = await identity();
      r.listed = !!(await waitFor(() => leaf('spike-fixture'), 20000));
      r.homepage = !!document.querySelector('.shell-homepage');
      r.openOrgs = await attempt(() => bridge().openOrgs());
      // Una tercera org, para abrirla en una ventana nueva más adelante.
      r.createTercera = await createOrg('tercera');
      return r;
    },
    // "Create new organization" en una Homepage la convierte en la vista de
    // creación (sin abrir otra ventana), y Cancel sin cambios vuelve a la Homepage.
    async createSwitch() {
      const r = {};
      const create = await waitFor(() => document.querySelector('.shell-homepage .shell-create-btn'), 10000);
      if (!create) return { error: 'sin botón Create new organization' };
      create.click();
      r.form = !!(await waitFor(() => document.querySelector('#shell-create-name'), 10000));
      r.during = await identity();
      const cancel = button('Cancel');
      if (cancel) cancel.click();
      r.back = !!(await waitFor(() => document.querySelector('.shell-homepage') && leaf('spike-fixture'), 10000));
      r.after = await identity();
      return r;
    },
    // Abrir una org desde la Homepage liga ESTA ventana a la org.
    async bindHome() {
      const r = {};
      const row = await waitFor(() => leaf('spike-fixture'), 10000);
      if (!row) return { error: 'sin fila spike-fixture' };
      row.click();
      r.path = await waitFor(() => location.pathname.startsWith('/o/') && location.pathname, 10000);
      r.agentShown = !!(await waitFor(() => text().includes('worker'), 20000));
      r.identity = await identity();
      r.openOrgs = await attempt(() => bridge().openOrgs());
      return r;
    },
    // Desde una ventana con org, "crear" abre una ventana de creación aparte.
    async openCreate() {
      return { created: await attempt(() => bridge().openCreateOrgWindow()), identity: await identity() };
    },
    // En la ventana de creación: escribe un nombre (cambio sin guardar).
    async createType() {
      const r = {};
      const input = await waitFor(() => document.querySelector('#shell-create-name'), 20000);
      r.identity = await identity();
      if (!input) return { ...r, error: 'sin formulario' };
      setInput(input, 'segunda');
      await pause(800);
      r.value = input.value;
      return r;
    },
    // Después de "seguir editando": el borrador sigue; se crea la org y la ventana pasa a ser esa org.
    async createSubmit() {
      const r = {};
      const input = document.querySelector('#shell-create-name');
      r.kept = input ? input.value : null;
      r.identityBefore = await identity();
      const advanced = [...document.querySelectorAll('#root button.disclosure')].find(b => b.textContent.includes('Advanced'));
      if (advanced) {
        advanced.click();
        const hub = await waitFor(() => document.querySelector('#shell-create-advanced input[type=checkbox]'), 5000);
        if (hub && hub.checked) hub.click();
        r.hubOff = !!hub && !hub.checked;
      }
      await pause(300);
      const submit = [...document.querySelectorAll('#root button[type=submit]')][0];
      if (!submit) return { ...r, error: 'sin botón Create organization' };
      submit.click();
      r.path = await waitFor(() => location.pathname === '/o/segunda' && location.pathname, 30000);
      r.identity = await identity();
      r.titleShown = !!(await waitFor(() => document.querySelector('.shell-header-title') && document.querySelector('.shell-header-title').textContent.includes('segunda'), 20000));
      r.error = (document.querySelector('.shell-create-error') || {}).textContent || null;
      return r;
    },
    // Pedir una org que no está abierta desde una ventana con org: ventana nueva.
    async requestOrg(args) {
      return { outcome: await attempt(() => bridge().requestOrg(args.org)), identity: await identity(), openOrgs: await attempt(() => bridge().openOrgs()) };
    },
    // Una ventana de org lista: su identidad, su ruta y su encabezado.
    async describe() {
      const r = {};
      r.rendered = !!(await waitFor(() => document.querySelector('#root') && document.querySelector('#root').children.length, 20000));
      await waitFor(() => document.querySelector('.shell-header-title') || document.querySelector('.error'), 40000);
      await pause(1500);
      r.path = location.pathname;
      r.identity = await identity();
      r.title = (document.querySelector('.shell-header-title') || {}).textContent || null;
      r.error = (document.querySelector('.error') || {}).textContent || null;
      r.text = text().replace(/\s+/g, ' ').trim().slice(0, 300);
      r.agentShown = text().includes('worker');
      r.theme = await attempt(async () => (await bridge().getPreferences()).visualTheme);
      return r;
    },
    // Solo la ventana dueña escribe las notificaciones globales.
    async owner() {
      const r = { identity: await identity() };
      r.notify = await attempt(() => bridge().notify({ id: 'wprobe-n', org: 'spike-fixture', kind: 'question', title: 'Prueba', body: 'Prueba de dueña' }));
      return r;
    },
    // Escucha notification-click en esta ventana (lo que reparte el renderer).
    async listen() {
      window.__wprobeClicks = [];
      window.__wprobeOff = bridge().onEvent(event => { if (event.type === 'notification-click') window.__wprobeClicks.push(event.data) });
      return { listening: true };
    },
    async clicks() {
      await waitFor(() => window.__wprobeClicks && window.__wprobeClicks.length, 5000);
      const r = { clicks: window.__wprobeClicks || [], focus: !!(await waitFor(() => document.hasFocus(), 5000)) };
      if (window.__wprobeOff) window.__wprobeOff();
      return r;
    },
    // Preferencias: valores válidos se guardan; claves y temas inválidos se rechazan.
    async prefs(args) {
      const r = {};
      r.set = await attempt(() => bridge().setPreferences(args.patch));
      r.theme = await attempt(() => bridge().setEffectiveTheme(args.theme));
      r.badTheme = await attempt(() => bridge().setEffectiveTheme('no-existe'));
      r.badKey = await attempt(() => bridge().setPreferences({ noExiste: true }));
      r.badType = await attempt(() => bridge().setPreferences({ exitOnClose: 'si' }));
      r.get = await attempt(() => bridge().getPreferences());
      return r;
    },
    async getPrefs() {
      return { get: await attempt(() => bridge().getPreferences()), identity: await identity() };
    },
    async shot(args) { await shot(args.name); return { name: args.name } },
  };

  return {
    async run(step, args) {
      let value;
      try { value = await steps[step](args || {}) } catch (e) { value = { exception: String(e && e.stack || e) } }
      document.title = 'orgtree-wprobe:' + step + ':' + JSON.stringify(value);
    },
  };
})();

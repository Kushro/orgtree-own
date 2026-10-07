// window.orgtreeDesktop sobre invoke de Tauri: el mismo contrato que
// apps/desktop/preload/index.ts (DesktopBridge en packages/contracts).
//
// Corre como initialization_script de cada ventana principal, en cada carga.
// Como el preload de Electron, solo existe en el frame principal, en el origen
// exacto del motor (__ORIGIN__ lo pone Rust) y en una ruta de la app. Los
// popouts about:blank y los iframes de agentes no lo tienen. Rust vuelve a
// verificar ventana y origen en cada comando, y resuelve QUÉ ventana llama por
// su etiqueta: el JS de la página no es la frontera de seguridad.
//
// Ventanas por organización (#20): cada ventana tiene su identidad
// (`windowIdentity`: Homepage, creación u org) y los métodos de Electron para
// abrir, enfocar y crear orgs. Con `requestOrg` presente, el renderer usa su
// modelo de varias ventanas en lugar del camino de un navegador.
//
// `windowIdentity` es síncrono, como el preload (que lo pide con sendSync):
// el renderer decide qué vista pintar en el primer cuadro. Tauri no tiene IPC
// síncrono y este script es fijo para la vida de la ventana, así que la
// semilla es la identidad con la que Rust creó la ventana (__IDENTITY__), o
// la última que recibió este documento si se recargó (sessionStorage es por
// ventana). Si no coincide con la ruta, la semilla es null y vale la
// respuesta de getWindowIdentity(): un cuadro vacío en lugar de la vista
// equivocada.
//
// Los obligatorios fuera del recorte rechazan con un error claro.
(() => {
  if (window.top !== window || location.origin !== '__ORIGIN__') return;
  if (!/^\/(index\.html)?$|^\/o\/[a-z0-9@-]+$/.test(location.pathname)) return;
  const internals = window.__TAURI_INTERNALS__;
  if (!internals || typeof internals.invoke !== 'function') return;
  const call = (cmd, args) => internals.invoke(cmd, args || {});
  const outside = name => () =>
    Promise.reject(new Error(`orgtreeDesktop.${name} está fuera del recorte del spike de Tauri`));
  const WINDOW = '__WINDOW__';
  const STORE = 'orgtree-tauri-window-identity';
  const valid = id => !!id && typeof id === 'object' && id.windowId === WINDOW &&
    (id.kind === 'homepage' || id.kind === 'create' || (id.kind === 'org' && typeof id.org === 'string'));
  const remember = id => { if (valid(id)) { try { sessionStorage.setItem(STORE, JSON.stringify(id)) } catch (e) {} } return id };
  const seed = (() => {
    let id = __IDENTITY__;
    try { const kept = JSON.parse(sessionStorage.getItem(STORE) || 'null'); if (valid(kept)) id = kept } catch (e) {}
    if (!valid(id)) return null;
    const onOrg = location.pathname.startsWith('/o/');
    if (id.kind === 'org' ? location.pathname !== '/o/' + id.org : onOrg) return null;
    return id;
  })();
  const listeners = new Set();
  // Lo que no se puede volver a pedir, si llega antes de que haya un listener
  // (la página todavía carga), se guarda para el primero (`HELD_EVENT_TYPES`
  // de Electron; events/heldbus.ts lo reparte).
  const held = [];
  const HELD = new Set(['open-org', 'notification-click', 'window-identity', 'restore-skipped']);
  const bridge = {
    windowIdentity: seed,
    getWindowIdentity: () => call('desktop_window_identity').then(remember),
    openHomepageWindow: () => call('desktop_open_homepage_window'),
    openCreateOrgWindow: () => call('desktop_open_create_window'),
    cancelCreation: () => call('desktop_cancel_creation'),
    requestOrg: org => call('desktop_request_org', { org }),
    bindCreatedOrg: org => call('desktop_bind_created_org', { org }),
    setUnsavedCreation: dirty => call('desktop_set_unsaved_creation', { dirty }).then(() => undefined),
    openOrgs: () => call('desktop_open_orgs'),
    takePendingWindowEvents: () => call('desktop_take_pending_events').then(events => {
      for (const event of events) if (event && event.type === 'window-identity') remember(event.data);
      return events;
    }),
    getAppVersion: () => call('desktop_app_version'),
    getStatus: () => call('desktop_status'),
    // #19: lo último que reportó el mantenimiento pedido por el motor, o null.
    getMaintenanceStatus: () => call('desktop_maintenance_status'),
    getWindowState: () => call('desktop_window_state'),
    getWindowControlsState: () => call('desktop_window_controls_state'),
    getPreferences: () => call('desktop_preferences'),
    setPreferences: patch => call('desktop_set_preferences', { patch }),
    setEffectiveTheme: theme => call('desktop_set_effective_theme', { theme }).then(() => undefined),
    showMainWindow: () => call('desktop_show'),
    quit: () => call('desktop_quit'),
    minimizeWindow: () => call('desktop_window_minimize'),
    toggleMaximizeWindow: () => call('desktop_window_toggle_maximize'),
    closeWindow: () => call('desktop_window_close'),
    // #22: los controles de un popout. Los llama el JS de ESTA ventana (el
    // popout es un about:blank adoptado, sin puente propio) con el nombre de
    // marco del window.open; Rust lo resuelve entre los popouts de esta ventana.
    getPopoutState: name => call('desktop_popout_state', { name }),
    minimizePopout: name => call('desktop_popout_minimize', { name }),
    toggleMaximizePopout: name => call('desktop_popout_toggle_maximize', { name }),
    closePopout: name => call('desktop_popout_close', { name }),
    focusPopout: name => call('desktop_popout_focus', { name }).then(() => undefined),
    getHarnesses: () => call('desktop_harnesses'),
    notify: notification => call('desktop_notify', { notification }),
    syncNotifications: active => call('desktop_sync_notifications', { active }).then(() => undefined),
    setPendingAttention: (ids, items) => call('desktop_pending_attention', { ids, items: items ?? null }).then(() => undefined),
    openHarnessLink: harness => call('desktop_open_harness', { harness }),
    openCharterFolder: () => call('desktop_open_charter_folder'),
    revealFile: path => call('desktop_reveal_file', { path }),
    startProviderLogin: (provider, opts) => call('desktop_provider_login_start', { provider, opts: opts ?? null }),
    getProviderLoginStatus: provider => call('desktop_provider_login_status', { provider }),
    submitProviderLoginCode: (provider, code) => call('desktop_provider_login_code', { provider, code }),
    cancelProviderLogin: provider => call('desktop_provider_login_cancel', { provider }),
    getUpdateStatus: () => Promise.resolve({ state: 'unavailable' }),
    checkForUpdates: () => Promise.resolve({ state: 'unavailable' }),
    installUpdate: outside('installUpdate'),
    onEvent: listener => {
      listeners.add(listener);
      for (const event of held.splice(0)) { try { listener(event) } catch (e) { console.error(e) } }
      return () => listeners.delete(listener);
    },
  };
  // Rust entrega eventos (DesktopEvent) con eval sobre esta función.
  Object.defineProperty(window, '__orgtreeDesktopDispatch', {
    value: event => {
      if (event && event.type === 'window-identity') remember(event.data);
      if (!listeners.size) { if (HELD.has(event.type)) held.push(event); return }
      for (const listener of [...listeners]) { try { listener(event) } catch (e) { console.error(e) } }
    },
  });
  Object.defineProperty(window, 'orgtreeDesktop', { value: Object.freeze(bridge), enumerable: true });
})();

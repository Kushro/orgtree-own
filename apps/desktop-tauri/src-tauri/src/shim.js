// window.orgtreeDesktop sobre invoke de Tauri: el mismo contrato que
// apps/desktop/preload/index.ts (DesktopBridge en packages/contracts).
//
// Corre como initialization_script de la ventana principal, en cada carga.
// Como el preload de Electron, solo existe en el frame principal y en el origen
// exacto del motor (__ORIGIN__ lo pone Rust). Los popouts about:blank y los
// iframes de agentes no lo tienen. Rust vuelve a verificar ventana y origen en
// cada comando: el JS de la página no es la frontera de seguridad.
//
// Los métodos opcionales del contrato que el recorte no cubre se omiten a
// propósito (requestOrg, ventanas múltiples, popouts nativos): el renderer ya
// tiene un camino para cuando no están, el mismo que usa en un navegador. Los
// obligatorios fuera del recorte rechazan con un error claro.
//
// Integraciones (#21): harnesses, login de proveedores, notificaciones
// completas, parpadeo de la barra de tareas y archivos. Los argumentos van
// tal cual a Rust, que los valida; ninguno llega a una línea de comandos.
(() => {
  if (window.top !== window || location.origin !== '__ORIGIN__') return;
  const internals = window.__TAURI_INTERNALS__;
  if (!internals || typeof internals.invoke !== 'function') return;
  const call = (cmd, args) => internals.invoke(cmd, args || {});
  const outside = name => () =>
    Promise.reject(new Error(`orgtreeDesktop.${name} está fuera del recorte del spike de Tauri`));
  const listeners = new Set();
  // `notification-click` no se puede volver a pedir: si llega antes de que haya
  // un listener (la página todavía carga), se guarda para el primero
  // (`HELD_EVENT_TYPES` de Electron; events/heldbus.ts lo reparte).
  const held = [];
  const HELD = new Set(['open-org', 'notification-click', 'window-identity', 'restore-skipped']);
  const bridge = {
    windowIdentity: null,
    getAppVersion: () => call('desktop_app_version'),
    getStatus: () => call('desktop_status'),
    getWindowState: () => call('desktop_window_state'),
    getWindowControlsState: () => call('desktop_window_controls_state'),
    getPreferences: () => call('desktop_preferences'),
    setPreferences: patch => call('desktop_set_preferences', { patch }),
    setEffectiveTheme: () => Promise.resolve(),
    showMainWindow: () => call('desktop_show'),
    quit: () => call('desktop_quit'),
    minimizeWindow: () => call('desktop_window_minimize'),
    toggleMaximizeWindow: () => call('desktop_window_toggle_maximize'),
    closeWindow: () => call('desktop_window_close'),
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
      if (!listeners.size) { if (HELD.has(event.type)) held.push(event); return }
      for (const listener of [...listeners]) { try { listener(event) } catch (e) { console.error(e) } }
    },
  });
  Object.defineProperty(window, 'orgtreeDesktop', { value: Object.freeze(bridge), enumerable: true });
})();

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
// propósito (requestOrg, ventanas múltiples, popouts nativos, login de
// proveedores): el renderer ya tiene un camino para cuando no están, el mismo
// que usa en un navegador. Los obligatorios fuera del recorte rechazan con un
// error claro.
(() => {
  if (window.top !== window || location.origin !== '__ORIGIN__') return;
  const internals = window.__TAURI_INTERNALS__;
  if (!internals || typeof internals.invoke !== 'function') return;
  const call = (cmd, args) => internals.invoke(cmd, args || {});
  const outside = name => () =>
    Promise.reject(new Error(`orgtreeDesktop.${name} está fuera del recorte del spike de Tauri`));
  const listeners = new Set();
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
    // Notificaciones nativas: #7.
    notify: () => Promise.resolve(false),
    openHarnessLink: outside('openHarnessLink'),
    getUpdateStatus: () => Promise.resolve({ state: 'unavailable' }),
    checkForUpdates: () => Promise.resolve({ state: 'unavailable' }),
    installUpdate: outside('installUpdate'),
    onEvent: listener => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
  // Rust entrega eventos (DesktopEvent) con eval sobre esta función.
  Object.defineProperty(window, '__orgtreeDesktopDispatch', {
    value: event => { for (const listener of [...listeners]) { try { listener(event) } catch (e) { console.error(e) } } },
  });
  Object.defineProperty(window, 'orgtreeDesktop', { value: Object.freeze(bridge), enumerable: true });
})();

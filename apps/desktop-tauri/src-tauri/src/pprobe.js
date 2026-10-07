// Prueba de popouts, pins y desks temporales (#22, ver pprobe.rs). El director
// en Rust evalúa este archivo en win-1 y después llama un paso:
// `__pprobe.run('<paso>', <args>)`. Cada paso devuelve su resultado por
// document.title (`orgtree-pprobe:<paso>:<json>`). Maneja el renderer real
// con clics en sus botones; del puente solo usa lo que usa el renderer.
window.__pprobe = window.__pprobe || (() => {
  const S = { opens: [], children: [], events: [], watch: null };
  const pause = ms => new Promise(done => setTimeout(done, ms));
  const waitFor = async (test, ms) => {
    const end = Date.now() + ms;
    while (Date.now() < end) { let value = null; try { value = test() } catch (e) {} if (value) return value; await pause(150) }
    return null;
  };
  const attempt = async fn => { try { return await fn() } catch (e) { return 'error:' + (e && e.message || e) } };
  const bridge = () => window.orgtreeDesktop;
  const leaf = (value, root = document) => [...root.querySelectorAll('#root *')]
    .find(el => el.children.length === 0 && el.textContent.trim() === value && !el.closest('.msgs, .tempdesk-panel, .ctxmenu'));
  const metrics = w => {
    try {
      return { screenX: w.screenX, screenY: w.screenY, outerWidth: w.outerWidth, outerHeight: w.outerHeight,
        innerWidth: w.innerWidth, innerHeight: w.innerHeight, dpr: w.devicePixelRatio };
    } catch (e) { return 'error:' + e }
  };
  const parse = features => {
    const out = {};
    for (const part of String(features || '').split(',')) {
      const [key, value] = part.split('=');
      if (value !== undefined && /^\s*-?\d+\s*$/.test(value)) out[key.trim()] = Number(value);
    }
    return out;
  };
  const box = el => { const r = el.getBoundingClientRect(); return { x: r.left, y: r.top, w: r.width, h: r.height } };
  // Un clic con la secuencia de puntero completa: el lienzo escucha pointerdown.
  const press = (el, x, y) => {
    const r = el.getBoundingClientRect();
    const at = { bubbles: true, cancelable: true, clientX: x ?? r.x + r.width / 2, clientY: y ?? r.y + r.height / 2,
      pointerId: 1, pointerType: 'mouse', isPrimary: true, button: 0, buttons: 1, view: el.ownerDocument.defaultView };
    const W = el.ownerDocument.defaultView;
    el.dispatchEvent(new W.PointerEvent('pointerdown', at));
    el.dispatchEvent(new W.MouseEvent('mousedown', at));
    el.dispatchEvent(new W.PointerEvent('pointerup', { ...at, buttons: 0 }));
    el.dispatchEvent(new W.MouseEvent('mouseup', { ...at, buttons: 0 }));
    el.dispatchEvent(new W.MouseEvent('click', { ...at, buttons: 0 }));
  };
  // Cada window.open del renderer: nombre, features y, si se pide, si el
  // elemento vigilado seguía en la ventana dueña en ese momento.
  const hook = () => {
    if (window.__pprobeHooked) return;
    window.__pprobeHooked = true;
    const native = window.open;
    window.open = function (...args) {
      const watched = S.watch ? document.querySelector(S.watch) : null;
      const entry = { name: String(args[1] || ''), features: String(args[2] || ''), at: Date.now(),
        watched: S.watch ? { connected: !!watched && watched.isConnected, inOwner: !!watched && watched.ownerDocument === document,
          stillModal: !!watched && !!watched.closest('.overlay') } : null };
      const child = native.apply(this, args);
      entry.opened = !!child;
      S.opens.push(entry);
      S.children.push(child);
      return child;
    };
    bridge().onEvent(event => { if (event.type === 'popout-state') S.events.push({ ...event.data, t: Date.now() }) });
  };
  const newOpen = async (before, test, ms) => waitFor(() => {
    if (S.opens.length <= before) return null;
    const child = S.children[S.children.length - 1];
    return child && test(child) && { entry: S.opens[S.opens.length - 1], child };
  }, ms);
  const controls = child => [...child.document.querySelectorAll('.popout-window-controls button')].map(b => b.getAttribute('aria-label'));
  // Fuera de la vista de desk: la tarjeta solo tiene menú si no es el desk enfocado.
  // (Los íconos de MUI no llevan data-testid en producción: se buscan por título.)
  const fitCard = async r => {
    const fit = document.querySelector('#root button[title="fit the whole org"]');
    r.fit = !!fit;
    if (fit) { fit.click(); await pause(1500) }
    let card = await waitFor(() => leaf('worker'), 5000);
    for (let i = 0; !card && i < 3; i++) {
      const out = document.querySelector('#root button[title="zoom out"]');
      if (out) { out.click(); await pause(800) }
      card = leaf('worker');
    }
    if (!card) r.candidates = [...document.querySelectorAll('#root *')].filter(el => el.children.length < 3 && /worker/.test(el.textContent))
      .slice(0, 8).map(el => el.tagName + '.' + String(el.className).slice(0, 40) + ':' + el.textContent.trim().slice(0, 30));
    return card;
  };
  const shot = async name => { document.title = 'orgtree-pprobe-pause:' + name; await pause(2500) };

  const steps = {
    // La org del fixture abierta en esta ventana, y los ganchos de la prueba.
    async org() {
      const r = {};
      r.bridge = !!bridge();
      r.methods = ['getPopoutState', 'minimizePopout', 'toggleMaximizePopout', 'closePopout', 'focusPopout']
        .filter(name => typeof (bridge() || {})[name] === 'function');
      if (!location.pathname.startsWith('/o/')) {
        const row = await waitFor(() => leaf('spike-fixture'), 30000);
        if (!row) return { ...r, error: 'sin fila spike-fixture' };
        row.click();
        r.path = await waitFor(() => location.pathname.startsWith('/o/') && location.pathname, 10000);
      }
      r.agentShown = !!(await waitFor(() => leaf('worker'), 20000));
      hook();
      r.owner = metrics(window);
      return r;
    },
    // window.open('', nombre, features) con un rectángulo de pantalla.
    async primitive(args) {
      const features = `popup=yes,left=${args.left},top=${args.top},width=${args.width},height=${args.height}`;
      const w = window.open('', 'orgtree-pprobe-primitive', features);
      if (!w) return { opened: false };
      S.primitive = w;
      const d = w.document;
      d.open(); d.write('<!doctype html><html><head><meta charset="utf-8"></head><body style="margin:0;background:#2b6">primitiva</body></html>'); d.close();
      d.title = 'Primitiva · Orgtree';
      await pause(1200);
      return { opened: true, features, child: metrics(w), owner: metrics(window) };
    },
    async primitiveClose() {
      const w = S.primitive;
      if (!w) return { error: 'sin primitiva' };
      w.close();
      return { closed: !!(await waitFor(() => w.closed, 5000)) };
    },
    // El desk real de worker, abierto en el lienzo y llevado a un popout.
    async desk() {
      const r = {};
      const card = leaf('worker');
      if (!card) return { error: 'sin tarjeta del agente' };
      press(card);
      r.deskOpen = !!(await waitFor(() => document.querySelector('.msgs .msg'), 20000));
      const button = await waitFor(() => document.querySelector('#root [aria-label="Open in new window"]'), 10000);
      if (!button) return { ...r, error: 'sin botón Open in new window' };
      const before = S.opens.length;
      button.click();
      const opened = await newOpen(before, child => child.document.querySelector('.msgs .msg'), 20000);
      if (!opened) return { ...r, error: 'el desk no llegó al popout', opens: S.opens };
      S.desk = opened.child;
      S.deskName = opened.entry.name;
      r.name = opened.entry.name;
      r.features = opened.entry.features;
      r.requested = parse(opened.entry.features);
      r.controls = await waitFor(() => { const c = controls(S.desk); return c.length === 3 && c }, 5000);
      r.ownerEmptied = !document.querySelector('.msgs .msg');
      r.child = metrics(S.desk);
      r.owner = metrics(window);
      return r;
    },
    async state() {
      return {
        desk: await attempt(() => bridge().getPopoutState(S.deskName)),
        unknown: await attempt(() => bridge().getPopoutState('orgtree-popout-999999')),
        empty: await attempt(() => bridge().getPopoutState('')),
        wrongType: await attempt(() => bridge().getPopoutState(42)),
      };
    },
    // Los botones de la ventana del popout, que dibuja PopoutWindowControls.
    async maximize() {
      const since = Date.now();
      const button = S.desk.document.querySelector('.popout-window-controls [aria-label="Maximize window"]');
      if (!button) return { error: 'sin botón Maximize', controls: controls(S.desk) };
      button.click();
      const event = await waitFor(() => S.events.find(e => e.t >= since && e.name === S.deskName && e.maximized === true), 5000);
      const label = await waitFor(() => S.desk.document.querySelector('.popout-window-controls [aria-label="Restore window"]'), 3000);
      return { event, relabeled: !!label, state: await attempt(() => bridge().getPopoutState(S.deskName)), child: metrics(S.desk) };
    },
    async restore() {
      const since = Date.now();
      const button = S.desk.document.querySelector('.popout-window-controls [aria-label="Restore window"]');
      if (!button) return { error: 'sin botón Restore', controls: controls(S.desk) };
      button.click();
      const event = await waitFor(() => S.events.find(e => e.t >= since && e.name === S.deskName && e.maximized === false), 5000);
      return { event, state: await attempt(() => bridge().getPopoutState(S.deskName)), child: metrics(S.desk) };
    },
    async minimize() {
      const since = Date.now();
      const button = S.desk.document.querySelector('.popout-window-controls [aria-label="Minimize window"]');
      if (!button) return { error: 'sin botón Minimize', controls: controls(S.desk) };
      button.click();
      await pause(800);
      return { events: S.events.filter(e => e.t >= since), state: await attempt(() => bridge().getPopoutState(S.deskName)) };
    },
    // "Show desk" del lugar del desk en el lienzo: MovableSurface.reveal → focusPopout.
    async focus() {
      const show = [...document.querySelectorAll('#root button')].find(b => b.textContent.trim() === 'Show desk');
      if (show) { show.click(); await pause(800); return { via: 'Show desk' } }
      return { via: 'focusPopout', result: await attempt(() => bridge().focusPopout(S.deskName)) };
    },
    // "Open desk" desde la fila de worker en la lista de agentes: el modal toma
    // prestado el desk del popout (MovableSurface.borrow), que se cierra con
    // window.close(). La lista no mueve la cámara, así que el lugar del desk en el
    // lienzo (el marcador "Show desk") sigue montado y Desks.endBorrow puede
    // devolver la ventana (restore). Por la tarjeta habría que encuadrar la org, eso
    // desmonta ese lugar y el renderer suelta la ventana en lugar de devolverla.
    async tempDesk() {
      const r = {};
      r.slotMounted = [...document.querySelectorAll('#root button')].some(b => b.textContent.trim() === 'Show desk');
      const toggle = document.querySelector('button.tray-toggle');
      r.tray = !!toggle;
      if (!toggle) return { ...r, error: 'sin el botón de la lista de agentes' };
      if (!document.querySelector('.tray-row')) toggle.click();
      const row = await waitFor(() => [...document.querySelectorAll('.tray-row')]
        .find(el => (el.querySelector('.tray-name') || {}).textContent === 'worker'), 5000);
      if (!row) return { ...r, error: 'sin la fila de worker en la lista de agentes' };
      const c = row.getBoundingClientRect();
      row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2, buttons: 2,
        clientX: c.x + c.width / 2, clientY: c.y + c.height / 2, view: window }));
      const item = await waitFor(() => [...document.querySelectorAll('.ctxmenu [role="menuitem"]')].find(b => b.textContent.trim() === 'Open desk'), 5000);
      r.menu = [...document.querySelectorAll('.ctxmenu [role="menuitem"]')].map(b => b.textContent.trim());
      if (!item) return { ...r, error: 'sin la entrada Open desk' };
      item.click();
      r.modal = !!(await waitFor(() => document.querySelector('.tempdesk-panel'), 10000));
      // La lista se cierra (su botón alterna); no forma parte de lo que se mide.
      if (document.querySelector('.tray-row')) { toggle.click(); await pause(300) }
      r.deskInModal = !!(await waitFor(() => document.querySelector('.tempdesk-panel .msgs .msg'), 15000));
      r.popoutClosed = !!(await waitFor(() => S.desk.closed, 5000));
      r.opensDuring = S.opens.length;
      return r;
    },
    // Cerrar el modal devuelve el desk a su ventana, con el rectángulo que tenía.
    async tempDeskClose() {
      const close = document.querySelector('.tempdesk-panel .tempdesk-close');
      if (!close) return { error: 'sin botón close del desk temporal' };
      const before = S.opens.length;
      close.click();
      const opened = await newOpen(before, child => child.document.querySelector('.msgs .msg'), 20000);
      const r = { modalGone: !!(await waitFor(() => !document.querySelector('.tempdesk-panel'), 5000)) };
      if (!opened) return { ...r, error: 'el desk no volvió a un popout' };
      S.desk = opened.child;
      r.name = opened.entry.name;
      r.sameName = opened.entry.name === S.deskName;
      S.deskName = opened.entry.name;
      r.features = opened.entry.features;
      r.requested = parse(opened.entry.features);
      await pause(1000);
      r.child = metrics(S.desk);
      return r;
    },
    // El inbox de worker (PinFrame `node-inbox`) como modal anclado a la ventana:
    // queda encima y en su lugar. Se abre desde el menú de su tarjeta (el botón
    // de Usage no aparece en el fixture, runs 36 y 37).
    // Después de devolver el desk a la ventana principal (deskClose), la vista es
    // la del desk de worker y su tarjeta no tiene menú: se encuadra la org primero.
    async modalPin() {
      const r = {};
      const card = await fitCard(r);
      if (!card) return { ...r, error: 'sin tarjeta del agente' };
      const c = card.getBoundingClientRect();
      card.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2, buttons: 2,
        clientX: c.x + c.width / 2, clientY: c.y + c.height / 2, view: window }));
      const item = await waitFor(() => [...document.querySelectorAll('.ctxmenu [role="menuitem"]')].find(b => b.textContent.trim() === 'Open inbox'), 5000);
      if (!item) return { error: 'sin la entrada Open inbox' };
      item.click();
      const bar = await waitFor(() => [...document.querySelectorAll('#root .modalpin-bar, body > * .modalpin-bar')]
        .find(b => b.querySelector('[aria-label="pin this to the window"]')), 10000);
      if (!bar) return { error: 'el inbox no abrió como modal anclable' };
      const panel = bar.parentElement;
      panel.setAttribute('data-pprobe', 'modal');
      r.title = (bar.textContent || '').trim().slice(0, 40);
      const pin = panel.querySelector('[aria-label="pin this to the window"]');
      if (!pin) return { error: 'sin botón para anclar' };
      pin.click();
      r.pinned = !!(await waitFor(() => panel.classList.contains('modalpin-win'), 5000));
      await pause(500);
      S.modal = panel;
      const rect = box(panel);
      r.rect = rect;
      const center = () => document.elementFromPoint(rect.x + rect.w / 2, rect.y + rect.h / 2);
      r.onTop = panel.contains(center());
      // Un clic en el lienzo, fuera del panel: un pin no se cierra ni se va.
      const px = rect.x > 40 ? rect.x - 25 : rect.x + rect.w + 25;
      const target = document.elementFromPoint(px, rect.y + 40);
      r.clicked = target ? target.className && String(target.className).slice(0, 60) : null;
      if (target && !panel.contains(target)) press(target, px, rect.y + 40);
      await pause(800);
      r.stillPinned = panel.isConnected && panel.classList.contains('modalpin-win');
      const after = box(panel);
      r.samePlace = Math.abs(after.x - rect.x) < 1 && Math.abs(after.y - rect.y) < 1 && Math.abs(after.w - rect.w) < 1 && Math.abs(after.h - rect.h) < 1;
      r.onTopAfter = panel.contains(center());
      r.owner = metrics(window);
      return r;
    },
    // Su pop-out: la ventana abre con el rectángulo del panel, y el modal sigue
    // en la dueña hasta que la ventana lo adopta.
    async modalPopout() {
      const panel = S.modal;
      const button = panel && panel.querySelector('.popout-button');
      if (!button) return { error: 'sin botón Open in new window en el modal' };
      const r = { rect: box(panel), owner: metrics(window) };
      const before = S.opens.length;
      S.watch = '[data-pprobe="modal"]';
      button.click();
      const opened = await newOpen(before, child => child.document.querySelector('[data-pprobe="modal"].modalpin-detached'), 15000);
      S.watch = null;
      if (!opened) return { ...r, error: 'el modal no llegó al popout', opens: S.opens.slice(before) };
      S.modalWindow = opened.child;
      r.name = opened.entry.name;
      r.features = opened.entry.features;
      r.requested = parse(opened.entry.features);
      r.atOpen = opened.entry.watched;
      r.inChild = !!opened.child.document.querySelector('[data-pprobe="modal"]');
      r.ownerHasPanel = !!document.querySelector('[data-pprobe="modal"]');
      r.controls = await waitFor(() => { const c = controls(opened.child); return c.length === 3 && c }, 5000);
      r.child = metrics(opened.child);
      return r;
    },
    // El botón de cerrar de la ventana (closePopout): el modal vuelve a la dueña.
    async modalClose() {
      const w = S.modalWindow;
      const button = w && w.document.querySelector('.popout-window-controls [aria-label="Close window"]');
      if (!button) return { error: 'sin botón Close window' };
      button.click();
      const r = { closed: !!(await waitFor(() => w.closed, 5000)) };
      r.back = !!(await waitFor(() => document.querySelector('[data-pprobe="modal"]'), 5000));
      const panel = document.querySelector('[data-pprobe="modal"]');
      r.pinnedAgain = !!panel && panel.classList.contains('modalpin-win');
      return r;
    },
    // "Return to main window" en el popout del desk: el renderer lo cierra con window.close().
    async deskClose() {
      const w = S.desk;
      if (!w || w.closed) return { skipped: 'el desk no está en un popout' };
      const button = w.document.querySelector('[aria-label="Return to main window"]');
      if (!button) return { error: 'sin botón Return to main window' };
      button.click();
      const r = { closed: !!(await waitFor(() => w.closed, 5000)) };
      r.redocked = !!(await waitFor(() => document.querySelector('.msgs .msg'), 15000));
      r.opens = S.opens.length;
      return r;
    },
    async shot(args) { await shot(args.name); return { name: args.name } },
  };

  return {
    async run(step, args) {
      let value;
      try { value = await steps[step](args || {}) } catch (e) { value = { exception: String(e && e.stack || e) } }
      document.title = 'orgtree-pprobe:' + step + ':' + JSON.stringify(value);
    },
  };
})();

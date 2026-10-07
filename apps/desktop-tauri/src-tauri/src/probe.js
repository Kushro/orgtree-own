// Prueba de diagnóstico del spike (ver probe.rs). Corre en la página del motor
// y devuelve el resultado por document.title, sin IPC. __ECHO__ y __PREFIX__
// los reemplaza Rust.
(async () => {
  const echo = 'http://127.0.0.1:__ECHO__';
  const r = {};
  const timeout = (ms, value) => new Promise(done => setTimeout(() => done(value), ms));

  // #3: autenticación por cookie
  try { r.nav = performance.getEntriesByType('navigation')[0].responseStatus } catch (e) { r.nav = String(e) }
  try { r.http = (await fetch('/api/desktop/identity')).status } catch (e) { r.http = String(e) }
  r.ws = await new Promise(done => {
    const socket = new WebSocket(`ws://${location.host}/api/orgs/probe/ws`);
    const timer = setTimeout(() => done('timeout'), 10000);
    socket.onopen = () => { clearTimeout(timer); socket.close(); done('open') };
    socket.onclose = event => { clearTimeout(timer); done('close:' + event.code) };
  });
  try { await fetch(echo + '/top', { mode: 'no-cors', credentials: 'include' }) } catch (e) {}
  r.iframe = await new Promise(done => {
    const frame = document.createElement('iframe');
    frame.setAttribute('sandbox', 'allow-scripts');
    frame.srcdoc = `<script>fetch('${echo}/iframe', { mode: 'no-cors', credentials: 'include' })
      .then(() => 'sent', e => 'error:' + e)
      .then(result => parent.postMessage({ probeIframe: result, origin: String(self.origin) }, '*'))<\/script>`;
    addEventListener('message', event => { if (event.data && event.data.probeIframe) done(event.data) });
    setTimeout(() => done('timeout'), 10000);
    document.documentElement.appendChild(frame);
  });

  // #4: el shim de window.orgtreeDesktop y la ventana de inicio del renderer real
  const waitFor = async (test, ms) => {
    const end = Date.now() + ms;
    while (Date.now() < end) { const value = test(); if (value) return value; await timeout(200) }
    return null;
  };
  r.shell = await (async () => {
    const out = {};
    const bridge = window.orgtreeDesktop;
    out.bridge = !!bridge;
    if (bridge) {
      try { out.version = await bridge.getAppVersion() } catch (e) { out.version = 'error:' + e }
      try { out.status = (await bridge.getStatus()).state } catch (e) { out.status = 'error:' + e }
      try { out.preferences = typeof (await bridge.getPreferences()).visualTheme } catch (e) { out.preferences = 'error:' + e }
      out.outside = await bridge.openHarnessLink('claude').then(() => 'resolved', e => String(e && e.message || e));
      out.requestOrg = typeof bridge.requestOrg; // omitido a propósito: el renderer abre en la misma ventana
    }
    out.iframeBridge = await new Promise(done => {
      const frame = document.createElement('iframe');
      frame.srcdoc = '<p>sin puente</p>';
      frame.onload = () => { try { done(typeof frame.contentWindow.orgtreeDesktop) } catch (e) { done('error:' + e) } };
      setTimeout(() => done('timeout'), 5000);
      document.documentElement.appendChild(frame);
    });
    const root = document.querySelector('#root');
    out.rendered = !!(root && await waitFor(() => root.children.length, 15000));
    if (out.rendered) {
      const row = await waitFor(() => [...document.querySelectorAll('#root *')]
        .find(el => el.children.length === 0 && el.textContent.trim() === 'spike-fixture'), 15000);
      out.orgListed = !!row;
      if (row) {
        row.click();
        out.orgPath = await waitFor(() => location.pathname.startsWith('/o/') && location.pathname, 10000);
        out.agentShown = !!(await waitFor(() => document.body.innerText.includes('worker'), 15000));
      }
    }
    return out;
  })();

  // #5: el desk del agente en vivo, con una conversación larga
  r.desk = r.shell && r.shell.agentShown ? await (async () => {
    const out = {};
    const card = [...document.querySelectorAll('#root *')]
      .find(el => el.children.length === 0 && el.textContent.trim() === 'worker');
    if (!card) { out.error = 'sin tarjeta del agente'; return out }
    // El lienzo escucha eventos de puntero, no solo click: la secuencia completa.
    const box = card.getBoundingClientRect();
    const at = { bubbles: true, cancelable: true, clientX: box.x + box.width / 2, clientY: box.y + box.height / 2,
      pointerId: 1, pointerType: 'mouse', isPrimary: true, button: 0, buttons: 1, view: window };
    card.dispatchEvent(new PointerEvent('pointerdown', at));
    card.dispatchEvent(new MouseEvent('mousedown', at));
    card.dispatchEvent(new PointerEvent('pointerup', { ...at, buttons: 0 }));
    card.dispatchEvent(new MouseEvent('mouseup', { ...at, buttons: 0 }));
    card.dispatchEvent(new MouseEvent('click', { ...at, buttons: 0 }));
    const msgs = await waitFor(() => document.querySelector('.msgs'), 15000);
    out.opened = !!msgs;
    if (!msgs) return out;
    await waitFor(() => msgs.querySelectorAll('.msg').length, 15000);
    out.toolShown = document.body.innerText.includes('README');
    // en vivo: el texto de los frames node_stream crece mientras miramos
    const beat = () => (msgs.innerText.match(/latido (\d+)/g) || []).map(m => Number(m.split(' ')[1]));
    const first = await waitFor(() => { const b = beat(); return b.length && Math.max(...b) }, 20000);
    const later = await waitFor(() => { const b = beat(); return b.length && Math.max(...b) > (first || 0) && Math.max(...b) }, 10000);
    out.live = { first, later };
    const count = () => msgs.querySelectorAll('.msg').length;
    const scroller = (() => { let el = msgs; while (el && el !== document.body) {
      const style = getComputedStyle(el);
      if (/(auto|scroll)/.test(style.overflowY) && el.scrollHeight > el.clientHeight) return el;
      el = el.parentElement } return null })();
    // conversación larga: el desk pide la página anterior al llegar arriba con el
    // scroll ("earlier messages"); subir hasta que la altura deje de crecer. Las
    // filas del DOM están virtualizadas, así que el avance se mide por la altura.
    const oldestVisible = () => {
      const n = (msgs.innerText.match(/(?:Mensaje|Respuesta) (\d+)/g) || []).map(m => Number(m.split(' ')[1]));
      return n.length ? Math.min(...n) : null;
    };
    out.earlierPages = 0;
    const pagingStart = performance.now();
    for (let page = 0; page < 80 && scroller; page++) {
      const before = scroller.scrollHeight;
      scroller.scrollTop = 0;
      scroller.dispatchEvent(new Event('scroll'));
      await timeout(150);
      if (oldestVisible() === 1 || msgs.innerText.includes('README')) break;
      if (!(await waitFor(() => scroller.scrollHeight > before, 5000))) break;
      out.earlierPages++;
    }
    out.pagingMs = Math.round(performance.now() - pagingStart);
    out.domRows = count();
    // tras cargar todo, arriba: el mensaje más viejo y el chip de la herramienta
    out.oldestLoaded = oldestVisible();
    out.toolShown = msgs.innerText.includes('README');
    // fluidez: recorrer la conversación entera de arriba abajo y medir los cuadros
    if (scroller) {
      const frames = [];
      const duration = 4000;
      const from = scroller.scrollTop, span = Math.max(1, scroller.scrollHeight - scroller.clientHeight - from);
      await new Promise(done => {
        const start = performance.now(); let last = start;
        const step = now => {
          frames.push(now - last); last = now;
          const t = Math.min(1, (now - start) / duration);
          scroller.scrollTop = from + span * t;
          if (t < 1) requestAnimationFrame(step); else done();
        };
        requestAnimationFrame(step);
      });
      frames.shift();
      const sorted = [...frames].sort((a, b) => a - b);
      out.scroll = {
        height: scroller.scrollHeight, frames: frames.length,
        avgMs: Math.round(frames.reduce((a, b) => a + b, 0) / frames.length * 10) / 10,
        p95Ms: Math.round(sorted[Math.floor(sorted.length * 0.95)] * 10) / 10,
        maxMs: Math.round(sorted[sorted.length - 1] * 10) / 10,
        over50ms: frames.filter(f => f > 50).length,
      };
      const numbers = (msgs.innerText.match(/(?:Mensaje|Respuesta) (\d+)/g) || []).map(m => Number(m.split(' ')[1]));
      out.newestLoaded = numbers.length ? Math.max(...numbers) : null;
      // pausa para la captura del CI: el desk con la conversación larga a la vista
      scroller.scrollTop = scroller.scrollHeight / 2;
      document.title = 'orgtree-probe-pause:desk';
      await timeout(3000);
    }
    return out;
  })() : { skipped: true };

  // #6: el popout real del desk ("Open in new window") y su borrador
  r.deskPopout = r.desk && r.desk.opened ? await (async () => {
    const out = {};
    const ownerSample = document.querySelector('.msgs .msgtext') || document.querySelector('.msgs .msg');
    out.ownerFontFamily = ownerSample ? getComputedStyle(ownerSample).fontFamily : '';
    const native = window.open;
    let child = null;
    window.open = function (...args) { child = native.apply(this, args); return child };
    try {
      const button = document.querySelector('#root [title="Open in new window"], #root [aria-label="Open in new window"]');
      if (!button) { out.error = 'sin botón Open in new window'; return out }
      button.click();
      await waitFor(() => child && child.document && child.document.querySelector('.msgs .msg'), 15000);
    } finally { window.open = native }
    out.opened = !!child;
    if (!child) return out;
    const d = child.document;
    out.messagesInChild = d.querySelectorAll('.msg').length;
    out.ownerEmptied = !document.querySelector('.msgs .msg');
    // fuentes: la tipografía del desk en el hijo es la misma y está cargada
    const sample = d.querySelector('.msgtext') || d.querySelector('.msg');
    // el <link> clonado carga asíncrono: esperar a que el hijo tenga la tipografía del dueño
    const styled = Date.now();
    await waitFor(() => sample && child.getComputedStyle(sample).fontFamily === out.ownerFontFamily, 10000);
    out.styledAfterMs = Date.now() - styled;
    const family = sample ? child.getComputedStyle(sample).fontFamily : '';
    out.fontFamily = family;
    out.fontMatchesOwner = family === out.ownerFontFamily;
    await child.document.fonts.ready;
    const firstFamily = family.split(',')[0].trim().replace(/^['"]|['"]$/g, '');
    out.fontLoaded = !!firstFamily && child.document.fonts.check(`14px "${firstFamily}"`);
    out.childFontFaces = [...child.document.fonts].filter(f => f.status === 'loaded').length;
    out.ownerFontFaces = [...document.fonts].filter(f => f.status === 'loaded').length;
    // borrador: escribir en el compositor del popout y verlo en el dueño al volver
    const composer = d.querySelector('textarea');
    if (composer) {
      const setter = Object.getOwnPropertyDescriptor(child.HTMLTextAreaElement.prototype, 'value').set;
      setter.call(composer, 'borrador escrito en el popout');
      composer.dispatchEvent(new child.Event('input', { bubbles: true }));
      await timeout(500);
      child.close();
      // El vigilante de popouts (reap_closed_popouts) revisa cada 400 ms.
      await timeout(2000);
      const back = await waitFor(() => {
        const area = document.querySelector('#root textarea');
        return area && area.value === 'borrador escrito en el popout' && area.value;
      }, 10000);
      out.draftBackInOwner = back === 'borrador escrito en el popout';
      out.redocked = !!document.querySelector('.msgs .msg');
    }
    return out;
  })() : { skipped: true };

  // #6: la secuencia de popout.tsx — window.open('', nombre, features), un shell
  // estándar escrito en el hijo, estilos clonados del dueño y DOM del dueño
  // movido al hijo como un portal de React.
  r.popout = await (async () => {
    const out = {};
    let w;
    try { w = window.open('', 'orgtree-probe-popout', 'popup=yes,left=60,top=60,width=420,height=320') } catch (e) { out.error = String(e); return out }
    out.opened = !!w;
    if (!w) return out;
    window.__orgtreeProbePopout = w;
    try { out.opener = w.opener === window } catch (e) { out.opener = String(e) }
    const d = w.document;
    d.open();
    d.write('<!doctype html><html><head><meta charset="utf-8"></head><body></body></html>');
    d.close();
    d.title = 'Desk de prueba · Orgtree';
    try { out.childOrigin = w.origin; out.sameOrigin = w.origin === window.origin } catch (e) { out.sameOrigin = String(e) }

    // estilos del dueño: un <style> (como Emotion/MUI) y un <link> (como el CSS de Vite)
    const style = document.createElement('style');
    style.textContent = '.probe-box { width: 123px; }';
    document.head.appendChild(style);
    const link = document.createElement('link');
    link.rel = 'stylesheet';
    link.href = 'data:text/css,.probe-box%7Bheight:45px%7D';
    const ownerLoaded = new Promise(done => { link.onload = () => done('load'); link.onerror = () => done('error') });
    document.head.appendChild(link);
    out.ownerLink = await Promise.race([ownerLoaded, timeout(5000, 'timeout')]);
    d.head.appendChild(style.cloneNode(true));
    const copy = link.cloneNode(true);
    const childLoaded = new Promise(done => { copy.onload = () => done('load'); copy.onerror = () => done('error') });
    d.head.appendChild(copy);
    out.childLink = await Promise.race([childLoaded, timeout(5000, 'timeout')]);

    // un borrador creado en el dueño, con su listener, movido al hijo
    const box = document.createElement('div');
    box.className = 'probe-box';
    const input = document.createElement('textarea');
    input.value = 'borrador del dueño';
    let heard = '';
    input.addEventListener('input', () => { heard = input.value });
    box.appendChild(input);
    document.body.appendChild(box);
    d.body.appendChild(box);
    out.moved = box.ownerDocument === d && d.body.contains(input);
    out.draftKept = input.value === 'borrador del dueño';
    const computed = w.getComputedStyle(box);
    out.styles = { width: computed.width, height: computed.height };
    input.value = 'escrito en el popout';
    input.dispatchEvent(new w.Event('input', { bubbles: true }));
    out.draftShared = heard === 'escrito en el popout';
    window.__orgtreeProbeShared = { draft: input.value };
    try { out.sharedState = w.opener.__orgtreeProbeShared.draft === 'escrito en el popout' } catch (e) { out.sharedState = String(e) }
    out.size = [w.innerWidth, w.innerHeight];
    out.childBridge = typeof w.orgtreeDesktop;
    out.closedFlag = w.closed;
    return out;
  })();

  document.title = '__PREFIX__' + JSON.stringify(r);
})();

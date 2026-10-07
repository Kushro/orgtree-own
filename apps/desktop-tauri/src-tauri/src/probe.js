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
    out.closedFlag = w.closed;
    return out;
  })();

  document.title = '__PREFIX__' + JSON.stringify(r);
})();

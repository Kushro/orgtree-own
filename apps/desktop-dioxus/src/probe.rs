//! Prueba de diagnóstico del spike, solo con `ORGTREE_DIOXUS_PROBE=<archivo>`.
//!
//! Con el motor de fixture (org `spike-fixture` con el agente `worker`, una
//! conversación larga y frames en vivo), corre en el webview real un script
//! (`document::eval`) que verifica:
//!
//! - #11, inicio en RSX: la lista de orgs carga desde el cliente Rust y abrir la
//!   org muestra a sus agentes;
//! - #26, organigrama: crear una org, contratar desde el menú (y una
//!   contratación por fuera de la UI que llega por el WebSocket), detener y
//!   reanudar, mover y deshacer, retirar, recontratar y borrar la org;
//! - #28, bandeja, preguntas y atención: un agente del fixture le escribe al
//!   usuario, le pregunta y levanta la bandera en dos tickets. Se verifican las
//!   notificaciones según las preferencias (con la ventana minimizada y al
//!   frente), la deduplicación, el retiro de las resueltas, el parpadeo de la
//!   barra de tareas, el clic (simulado con lo mismo que llama `Activated`),
//!   la cola de atención (responder y descartar preguntas, responder y
//!   descartar banderas) y la bandeja (leer, archivar, responder y "Mark all
//!   read");
//! - #29, docket: el fixture siembra tickets por el ledger real; la lista con
//!   las casillas, los filtros y los totales sin archivados; el detalle
//!   (decisiones, evidencias, artefactos, holders e historial); comentar,
//!   bajar la bandera, responder la pregunta adjunta y asignar con "Staff…";
//!   y los cambios de los agentes (estado, bandera, pregunta) en vivo;
//! - #30, cuentas, proveedores y ajustes: guardar los ajustes de la org y de
//!   la app y releerlos; el diálogo de contratar filtra los tiers por los
//!   proveedores instalados; los proveedores con lo que detecta el shell; una
//!   cuenta administrada de Codex, su login con el `codex` falso del `PATH`
//!   (los rechazos, el ciclo y cancelar), refrescarla y quitarla; y el tema.
//!   Al final, un enlace `https` del desk se abre por la vía controlada y los
//!   raros se muestran como texto;
//! - #12, desk en RSX: la conversación, la herramienta, el Markdown sanitizado,
//!   el texto en vivo, la carga de páginas anteriores hasta el primer mensaje y
//!   los cuadros de un scroll de punta a punta;
//! - #27, desk completo: pensamiento, mail con respuesta citada y adjunto,
//!   avisos y segmentos; enviar un mensaje (queda aceptado en el buzón);
//!   los estados del turno (en cola por el límite de turnos, trabajando con
//!   STOP, detenido con reanudar); el cambio de modelo (en cola a mitad de
//!   turno, con confirmación entre proveedores) y el esfuerzo; y el revelado
//!   de archivos, que rechaza rutas relativas o inexistentes;
//! - #13, varias ventanas: el desk en otra ventana nativa, el borrador
//!   compartido en los dos sentidos y el cierre de la principal con el desk
//!   abierto (la principal se oculta, el desk sigue);
//! - #14, integración nativa: botones de la ventana sin marco, notificación,
//!   bandeja, el arrastre con el mouse real (lo hace el CI en una pausa) y la
//!   instancia única: con todas las ventanas cerradas la app sigue en la
//!   bandeja, una segunda ejecución vuelve a mostrar la principal y Salir
//!   termina la app y el motor.
//!
//! El script avisa por `dioxus.send` cuando el desk está listo para una captura
//! (Rust deja `<archivo>.desk` para el CI) y al final manda el resultado, que se
//! escribe en el archivo. El token nunca pasa por el webview.

use dioxus::prelude::*;
use std::sync::Mutex;

const SCRIPT: &str = r#"
const timeout = ms => new Promise(done => setTimeout(done, ms));
const waitFor = async (test, ms) => {
  const end = Date.now() + ms;
  while (Date.now() < end) { const value = test(); if (value) return value; await timeout(200) }
  return null;
};
const r = {};

// #11: inicio
const home = r.home = {};
home.card = !!(await waitFor(() => document.querySelector('.welcome-card'), 15000));
home.version = (document.querySelector('.welcome-card .build-badge') || {}).textContent || null;
const row = await waitFor(() => [...document.querySelectorAll('.welcome-card nav .org')]
  .find(el => el.textContent.includes('spike-fixture')), 15000);
home.orgListed = !!row;
home.counts = row ? (row.querySelector('.org-counts') || {}).textContent : null;
home.fontFamily = row ? getComputedStyle(row).fontFamily : null;
home.bridge = typeof window.orgtreeDesktop;
// #24: la raíz de datos en uso, a la vista en el inicio
home.dataRoot = (document.querySelector('.welcome-card .dx-data-root code') || {}).textContent || null;
if (row) {
  // pausa para la captura del CI: el inicio con la lista cargada
  dioxus.send({ pause: 'home' });
  await timeout(3000);
}

// #26: organigrama. Crear una org, contratar, detener y reanudar, mover,
// retirar y recontratar desde el menú de agente, y borrar la org.
const chart = r.chart = {};
const setValue = (el, value, type = 'input') => { el.value = value; el.dispatchEvent(new Event(type, { bubbles: true })) };
const view = () => document.querySelector('.dx-org-view');
const agentCard = id => document.querySelector(`.dx-org-view .dx-agent[data-node="${id}"]`);
const parentOf = id => {
  const card = agentCard(id);
  const up = card && card.closest('.node').parentElement.closest('.node');
  return up ? up.querySelector(':scope > .card').getAttribute('data-node') : null;
};
const closeMenu = async () => {
  document.querySelector('.dx-menu-scrim')?.click();
  await waitFor(() => !document.querySelector('.ctxmenu'), 5000);
};
// clic derecho sobre la tarjeta, con coordenadas reales: el menú se abre ahí
const menuFor = async id => {
  await closeMenu();
  const card = id === '@user' ? document.querySelector('.dx-user-card') : agentCard(id);
  if (!card) return null;
  const box = card.getBoundingClientRect();
  card.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true,
    clientX: Math.round(box.left + box.width * 0.4), clientY: Math.round(box.top + box.height / 2) }));
  return await waitFor(() => document.querySelector('.ctxmenu'), 5000);
};
const pick = async (id, label) => {
  const menu = await menuFor(id);
  const entry = menu && [...menu.querySelectorAll('.ctxmenu-item')].find(b => b.textContent.trim() === label);
  if (!entry) { chart.missing = (chart.missing || []).concat(`${id}: ${label}`); await closeMenu(); return false }
  entry.click();
  return true;
};
const hire = async (id, name, grant, pause) => {
  if (!(await pick(id, id === '@user' ? 'Hire a top-level agent…' : 'Hire a subordinate…'))) return false;
  const form = await waitFor(() => document.querySelector('.dx-hire'), 5000);
  if (!form) return false;
  setValue(form.querySelector('#dx-hire-name'), name);
  setValue(form.querySelector('#dx-hire-grant'), String(grant));
  await timeout(300);
  if (pause) { dioxus.send({ pause }); await timeout(3000) }
  form.querySelector('button[type="submit"]').click();
  return !!(await waitFor(() => agentCard(name), 15000));
};
const loads = () => Number(view() ? view().dataset.loads : NaN);
const newOrg = document.querySelector('.welcome-card .dx-new-org');
if (newOrg) {
  newOrg.click();
  const input = await waitFor(() => document.querySelector('.dx-new-org-form input'), 5000);
  if (input) {
    setValue(input, 'Prueba Dioxus');
    await timeout(200);
    document.querySelector('.dx-new-org-form button[type="submit"]').click();
  }
  chart.created = !!(await waitFor(() => view() && document.querySelector('.dx-user-card'), 20000));
  chart.orgName = (document.querySelector('.dx-org-view h2') || {}).textContent || null;
  chart.emptyTree = !!document.querySelector('.dx-empty');
  // sin sondeo: con la org quieta, el árbol no se vuelve a pedir
  await waitFor(() => view() && view().dataset.connected === 'true', 10000);
  // (una relectura en esos segundos solo puede venir de un frame del motor)
  const frames = () => Number(view() ? view().dataset.frames : NaN);
  const idleFrom = { loads: loads(), frames: frames() };
  await timeout(8000);
  chart.idle = { seconds: 8, loads: loads() - idleFrom.loads, frames: frames() - idleFrom.frames };
  chart.hiredTop = await hire('@user', 'jefe', 5, 'chart-hire');
  chart.hiredSub = await hire('jefe', 'ayudante', 0);
  chart.nested = parentOf('ayudante') === 'jefe';
  // un cambio hecho fuera de la UI (el cliente Rust directo) llega por el WebSocket
  dioxus.send({ externalHire: 'externo' });
  chart.external = await dioxus.recv();
  chart.externalShown = !!(await waitFor(() => agentCard('externo'), 15000));
  chart.externalTop = parentOf('externo') === '@user';
  chart.bar = (document.querySelector('.dx-org-view .chip.agents') || {}).textContent || null;
  chart.credits = (document.querySelector('.dx-org-view .dx-credits') || {}).textContent || null;
  chart.model = (agentCard('jefe')?.querySelector('.badge.prov-claude') || {}).textContent || null;
  // el menú de agente, con captura
  const menu = await menuFor('ayudante');
  chart.menu = menu ? [...menu.querySelectorAll('.ctxmenu-item')].map(b => b.textContent.trim() + (b.disabled ? ' (disabled)' : '')) : null;
  if (menu) {
    dioxus.send({ pause: 'chart-menu' });
    await timeout(3000);
    await closeMenu();
  }
  const status = id => agentCard(id) && agentCard(id).dataset.status;
  // detener y reanudar (HaltControl)
  if (await pick('ayudante', 'Halt')) {
    chart.halted = !!(await waitFor(() => status('ayudante') === 'Halted' && agentCard('ayudante').querySelector('.badge.halted'), 15000));
    chart.haltToast = [...document.querySelectorAll('.toast')].map(t => t.textContent).find(t => t.includes('halted')) || null;
    dioxus.send({ pause: 'chart' });
    await timeout(3000);
  }
  if (await pick('ayudante', 'Unhalt')) {
    chart.unhalted = !!(await waitFor(() => status('ayudante') === 'Idle', 15000));
  }
  // interrumpir: solo con un turno en curso, como el STOP del desk
  const m2 = await menuFor('ayudante');
  const interrupt = m2 && [...m2.querySelectorAll('.ctxmenu-item')].find(b => b.textContent.trim() === 'Interrupt');
  chart.interruptDisabledWhenIdle = !!(interrupt && interrupt.disabled);
  await closeMenu();
  // mover al primer nivel, y deshacer desde el aviso
  if (await pick('ayudante', 'Move to…')) {
    const form = await waitFor(() => document.querySelector('.dx-move'), 5000);
    if (form) {
      setValue(form.querySelector('#dx-move-to'), '', 'change');
      await timeout(300);
      form.querySelector('button[type="submit"]').click();
      chart.moved = !!(await waitFor(() => parentOf('ayudante') === '@user', 15000));
      const undo = await waitFor(() => [...document.querySelectorAll('.toast')].find(t => t.textContent.includes('now reports to'))?.querySelector('.toast-undo'), 5000);
      if (undo) undo.click();
      chart.movedBack = !!(await waitFor(() => parentOf('ayudante') === 'jefe', 15000));
    }
  }
  // retirar (con la confirmación de AgentRetireConfirm), plegado, y recontratar
  if (await pick('ayudante', 'Retire…')) {
    const confirm = await waitFor(() => document.querySelector('.dx-confirm'), 5000);
    chart.retireConfirm = confirm ? confirm.querySelector('.confirm-body').textContent : null;
    if (confirm) confirm.querySelector('.danger.solid').click();
    chart.retiredHidden = !!(await waitFor(() => !agentCard('ayudante') && document.querySelector('.dx-tree .tray-arch'), 15000));
    const fold = document.querySelector('.dx-tree .tray-arch');
    if (fold) fold.click();
    chart.retiredShown = !!(await waitFor(() => agentCard('ayudante') && agentCard('ayudante').closest('.node.archived'), 5000));
    if (await pick('ayudante', 'Rehire')) {
      chart.rehired = !!(await waitFor(() => status('ayudante') === 'Idle', 15000));
    }
  }
  chart.loads = loads();
  chart.frames = frames();
  chart.toasts = [...document.querySelectorAll('.toast')].map(t => t.textContent);
  // volver al inicio y borrar la org (con la confirmación de App.tsx)
  document.querySelector('.dx-org-view button.home').click();
  const mine = await waitFor(() => document.querySelector('.welcome-card nav .org[data-slug="prueba-dioxus"]'), 15000);
  chart.listed = !!mine;
  if (mine) {
    mine.querySelector('.org-del').click();
    const confirm = await waitFor(() => document.querySelector('.dx-delete-org'), 5000);
    chart.deleteConfirm = confirm ? confirm.querySelector('h3').textContent : null;
    if (confirm) confirm.querySelector('.danger.solid').click();
    chart.deleted = !!(await waitFor(() => !document.querySelector('.welcome-card nav .org[data-slug="prueba-dioxus"]'), 15000));
  }
}

const fixtureRow = await waitFor(() => [...document.querySelectorAll('.welcome-card nav .org')]
  .find(el => el.textContent.includes('spike-fixture')), 15000);
if (fixtureRow) {
  fixtureRow.click();
  home.orgOpened = !!(await waitFor(() => document.querySelector('.dx-org-view'), 10000));
  home.agentShown = !!(await waitFor(() => document.querySelector('.dx-agent[data-node="worker"]'), 15000));
}

// #28: bandeja, preguntas, cola de atención y notificaciones. El agente
// `worker` (motor de fixture, ledger real) le escribe al usuario, le pregunta
// y levanta la bandera en dos tickets; Rust anota cada decisión de las
// notificaciones (`notify::log_snapshot`).
const attn = r.attention = {};
if (home.agentShown) {
  const call = async message => { dioxus.send(message); return await dioxus.recv() };
  const notes = async () => (await call({ notifyLog: true })) || [];
  const waitNotes = async (test, ms) => {
    const end = Date.now() + ms;
    while (Date.now() < end) { const value = test(await notes()); if (value) return value; await timeout(400) }
    return null;
  };
  const decisions = (l, kind) => l.filter(e => e.type === 'notify' && e.kind === kind).map(e => e.decision);
  const orgView = () => document.querySelector('.dx-org-view');
  const data = key => Number(orgView() ? orgView().dataset[key] : NaN);
  const toasts = () => [...document.querySelectorAll('.dx-org-view .toast')].map(t => t.textContent);
  const toastWith = text => waitFor(() => toasts().find(t => t.includes(text)), 10000);
  const row = key => document.querySelector(`.dx-attn [data-attn-row="${key}"]`);
  const rowKeys = () => [...document.querySelectorAll('.dx-attn [data-attn-row]')].map(e => e.getAttribute('data-attn-row'));
  const selectedKey = () => { const on = document.querySelector('.dx-attn .attn-cell .mailrow.on'); return on ? on.closest('[data-attn-row]').getAttribute('data-attn-row') : null };
  const pane = () => document.querySelector('.dx-attn .attn-mread');
  const type = async (area, text) => {
    setValue(area, text);
    await timeout(300);
    area.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', bubbles: true, cancelable: true }));
  };
  attn.prefs = await call({ notifyPrefs: { notificationsEnabled: true, notifyQuestions: true, notifyUrgentMail: true,
    notifyDocketAttention: true, notifyAllMail: false, notifyWhileFocused: false } });

  // 1. Sin foco (ventana minimizada): llegan un mail urgente, uno de rutina,
  //    una pregunta y dos tickets con bandera. Se muestran la pregunta, el
  //    urgente y los tickets; el de rutina no (tipo apagado por defecto); la
  //    barra de tareas parpadea.
  attn.minimized = await call({ minimize: true });
  for (const kind of ['urgent', 'routine', 'question', 'tickets']) attn['seed_' + kind] = await call({ attentionSeed: kind });
  attn.shownMinimized = await waitNotes(l => {
    const shown = l.filter(e => e.type === 'notify' && e.decision === 'shown').map(e => e.kind);
    return shown.filter(k => k === 'work-attention').length >= 2 && shown.includes('question') && shown.includes('urgent-mail') && shown;
  }, 25000);
  attn.routineMinimized = decisions(await notes(), 'routine');
  attn.flash = (await notes()).filter(e => e.type === 'attention');
  dioxus.send({ pause: 'taskbar' });
  await timeout(3000);
  // 2. "All mail" prendido: el de rutina se muestra; apagado otra vez: se retira.
  await call({ notifyPrefs: { notifyAllMail: true } });
  attn.routineShown = !!(await waitNotes(l => decisions(l, 'routine').includes('shown'), 15000));
  await call({ notifyPrefs: { notifyAllMail: false } });
  attn.routineRetracted = !!(await waitNotes(l => l.some(e => e.type === 'configure' && (e.removed || []).some(x => x.kind === 'routine')), 5000));
  // 3. Con la ventana al frente: un urgente nuevo espera hasta prender "Notify while focused".
  attn.restored = await call({ restore: true });
  attn.seed_urgent2 = await call({ attentionSeed: 'urgent2' });
  attn.focusedHeld = !!(await waitNotes(l => decisions(l, 'urgent-mail').includes('focused'), 15000));
  await call({ notifyPrefs: { notifyWhileFocused: true } });
  attn.focusedShown = !!(await waitNotes(l => decisions(l, 'urgent-mail').filter(d => d === 'shown').length >= 2, 15000));
  await call({ notifyPrefs: { notifyWhileFocused: false } });
  attn.duplicates = (await notes()).filter(e => e.type === 'notify' && e.decision === 'shown').length;
  // los conteos de la barra de la org: 3 sin leer y 1 pregunta; 5 en la cola
  attn.counts = await waitFor(() => data('unread') === 3 && data('asks') === 1 && data('attention') === 5
    && { unread: data('unread'), asks: data('asks'), attention: data('attention') }, 15000)
    || { unread: data('unread'), asks: data('asks'), attention: data('attention') };
  attn.bell = (document.querySelector('.dx-org-view .dx-inbox-bell') || {}).className || null;

  // 4. El clic en la notificación de la pregunta (simulado con lo mismo que
  //    llama `Activated`): la vista pasa a la cola con la pregunta elegida.
  attn.click = await call({ clickNotice: 'question' });
  attn.clickOpened = await waitFor(() => { const k = selectedKey(); return k && k.startsWith('question:') && pane() && pane().querySelector('.askcard') && k }, 10000);
  if (!attn.clickOpened) {
    // sin toast (sin clic que simular): la cola se abre a mano y la prueba sigue
    document.querySelector('.dx-org-view .orgview-tab[data-view="attention"]').click();
    const q = await waitFor(() => rowKeys().find(k => k.startsWith('question:')), 10000);
    if (q) row(q).querySelector('.mailrow').click();
    await waitFor(() => pane() && pane().querySelector('.askcard'), 5000);
  }
  attn.rows = rowKeys();
  attn.rowKinds = [...document.querySelectorAll('.dx-attn [data-attn-row]')].map(e => e.getAttribute('data-attn-kind'));
  dioxus.send({ pause: 'notification-click' });
  await timeout(3000);

  // 5a. Responder la pregunta: la tarjeta se va y su notificación se retira.
  const card = pane() && pane().querySelector('.askcard');
  attn.askText = card ? (card.querySelector('.ask-q') || {}).textContent : null;
  attn.askOptions = card ? [...card.querySelectorAll('.ask-option-label')].map(b => b.textContent) : null;
  if (card) {
    card.querySelector('.ask-row[data-option="Sí"]').click();
    await timeout(300);
    card.querySelector('.ask-submit').click();
    attn.answerToast = await toastWith("resolved worker's batch");
    attn.questionGone = !!(await waitFor(() => !rowKeys().some(k => k.startsWith('question:')), 10000));
    attn.questionRetracted = !!(await waitNotes(l => l.some(e => e.type === 'sync' && (e.removed || []).some(x => x.kind === 'question')), 15000));
  }
  // 5b. El mail urgente: abrirlo lo marca leído (queda mientras está elegido) y se responde.
  const urgentKey = 'mail:' + (attn.seed_urgent || {}).mail;
  if (row(urgentKey)) {
    row(urgentKey).querySelector('.mailrow').click();
    attn.urgentReason = (await waitFor(() => pane() && pane().querySelector('.urgent-why'), 5000) || {}).textContent || null;
    attn.urgentRead = await waitFor(() => row(urgentKey) && !row(urgentKey).querySelector('.mailrow.unread') && 'retained', 10000);
    const area = pane().querySelector('.mail-reply textarea');
    if (area) {
      await type(area, 'Reintentá una vez más, por favor.');
      attn.replyToast = await toastWith('sent to worker');
    }
  }
  // 5c. Primer ticket: responder baja la bandera sin cambiar el estado.
  const [firstTicket, secondTicket] = ((attn.seed_tickets || {}).tickets) || [];
  if (row('ticket:' + firstTicket)) {
    row('ticket:' + firstTicket).querySelector('.mailrow').click();
    attn.flagReason = (await waitFor(() => pane() && pane().querySelector('.docket-attention-body'), 5000) || {}).textContent || null;
    dioxus.send({ pause: 'attention' });
    await timeout(3000);
    const area = pane().querySelector('.mail-reply textarea');
    if (area) await type(area, 'Usá el certificado de prueba.');
    attn.ticketReplyToast = await toastWith('sent to worker');
    attn.ticketReplyGone = !!(await waitFor(() => !row('ticket:' + firstTicket), 15000));
    attn.ticketReplied = await call({ workItem: firstTicket });
  }
  // 5d. Segundo ticket: descartar lo saca en el clic y lo pasa a `blocked`.
  if (row('ticket:' + secondTicket)) {
    row('ticket:' + secondTicket).querySelector('.mailrow').click();
    const dismiss = await waitFor(() => pane() && pane().querySelector('.docket-dismiss'), 5000);
    if (dismiss) {
      dismiss.click();
      attn.dismissGoneAtOnce = !row('ticket:' + secondTicket) || !!(await waitFor(() => !row('ticket:' + secondTicket), 1000));
      attn.dismissToast = await toastWith('dismissed the attention flag');
      attn.ticketDismissed = await call({ workItem: secondTicket });
    }
  }
  // 5e. Una pregunta nueva, descartada con la ✕ (salta todas las pestañas).
  attn.seed_question2 = await call({ attentionSeed: 'question2' });
  const q2 = await waitFor(() => rowKeys().find(k => k.startsWith('question:')), 15000);
  if (q2) {
    row(q2).querySelector('.mailrow').click();
    const close = await waitFor(() => pane() && pane().querySelector('.askcard .dx-ask-dismiss'), 5000);
    if (close) close.click();
    attn.dismissQuestionToast = await toastWith("dismissed worker's question");
    attn.question2Gone = !!(await waitFor(() => !rowKeys().some(k => k.startsWith('question:')), 10000));
  }
  attn.rowsAfter = rowKeys();

  // 6. La bandeja: leer el de rutina, salir de él (queda archivado), responder
  //    el segundo urgente y "Mark all read".
  const bell = document.querySelector('.dx-org-view .dx-inbox-bell');
  if (bell) {
    bell.click();
    const inbox = await waitFor(() => document.querySelector('.dx-inbox .mailer-list'), 10000);
    const mailRow = id => document.querySelector(`.dx-inbox .mailrow[data-mail="${id}"]`);
    const routineId = (attn.seed_routine || {}).mail, urgent2Id = (attn.seed_urgent2 || {}).mail;
    attn.inboxRows = inbox ? [...inbox.querySelectorAll('.mailrow')].map(e => e.getAttribute('data-mail') + (e.classList.contains('unread') ? ' (unread)' : '')) : null;
    attn.inboxUnreadTab = (document.querySelector('.dx-inbox .mail-folders .tab-count') || {}).textContent || null;
    if (mailRow(routineId)) {
      mailRow(routineId).click();
      attn.routineOpened = (await waitFor(() => document.querySelector('.dx-inbox .mailer-read .mailer-body'), 5000) || {}).textContent || null;
      dioxus.send({ pause: 'inbox' });
      await timeout(3000);
    }
    if (mailRow(urgent2Id)) {
      mailRow(urgent2Id).click();
      attn.routineArchived = !!(await waitFor(() => mailRow(routineId) && !mailRow(routineId).classList.contains('unread'), 10000));
      const area = await waitFor(() => document.querySelector('.dx-inbox .mailer-read .mail-reply textarea'), 5000);
      if (area) {
        await type(area, 'Publicalo igual.');
        attn.inboxReplyToast = await toastWith('sent to worker');
        attn.urgent2Read = !!(await waitFor(() => mailRow(urgent2Id) && !mailRow(urgent2Id).classList.contains('unread'), 10000));
      }
    }
    // un mail más, archivado con "Mark all read"
    attn.seed_routine2 = await call({ attentionSeed: 'routine' });
    const markAll = await waitFor(() => document.querySelector('.dx-inbox .dx-mark-all'), 10000);
    if (markAll) {
      markAll.click();
      attn.allRead = !!(await waitFor(() => data('unread') === 0 && !document.querySelector('.dx-inbox .mailrow.unread:not(.ask)'), 10000));
    }
    document.querySelector('.dx-inbox-overlay').click();
    attn.inboxClosed = !!(await waitFor(() => !document.querySelector('.dx-inbox'), 5000));
  }
  // 7. Sin nada pendiente, la barra de tareas deja de parpadear.
  attn.flashStopped = !!(await waitNotes(l => { const a = l.filter(e => e.type === 'attention'); return a.length && a[a.length - 1].pulse === 'Stop' }, 15000));
  attn.empty = (await waitFor(() => document.querySelector('.dx-attn .attn-empty'), 5000) || {}).textContent || null;
  attn.toasts = toasts();
  attn.notifications = await notes();
  // las preferencias vuelven a las de fábrica, y la vista al organigrama
  await call({ notifyPrefs: { notificationsEnabled: true, notifyQuestions: true, notifyUrgentMail: true, notifyTerminalFailures: true,
    notifyDocketAttention: true, notifyAllMail: false, notifyDocuments: false, notifyFrozen: false, notifyWhileFocused: false } });
  const chartTab = document.querySelector('.dx-org-view .orgview-tab[data-view="chart"]');
  if (chartTab) chartTab.click();
  await waitFor(() => document.querySelector('.dx-agent[data-node="worker"]'), 10000);
}

// #29: docket. El fixture siembra tickets por el ledger real, como agentes con
// la herramienta del docket; el usuario los lee, filtra, comenta, descarta la
// bandera, responde la pregunta adjunta y asigna con "Staff…". Los cambios de
// los agentes (estado, bandera, pregunta) llegan solos por el WebSocket.
const dk = r.docket = {};
if (home.agentShown) {
  const call = async message => { dioxus.send(message); return await dioxus.recv() };
  const orgView = () => document.querySelector('.dx-org-view');
  const toasts = () => [...document.querySelectorAll('.dx-org-view .toast')].map(t => t.textContent);
  const toastWith = text => waitFor(() => toasts().find(t => t.includes(text)), 15000);
  const panel = () => document.querySelector('.dx-docket');
  const rowEls = () => [...document.querySelectorAll('.dx-docket .dx-docket-list .docket-row')];
  const rows = () => rowEls().map(e => e.getAttribute('data-ticket'));
  const rowEl = slug => document.querySelector(`.dx-docket .docket-row[data-ticket="${slug}"]`);
  const read = () => document.querySelector('.dx-docket .dx-docket-read');
  const totals = () => { const t = document.querySelector('.dx-docket .dx-docket-totals'); return t ? { text: t.textContent, active: Number(t.dataset.active), attention: Number(t.dataset.attention), shown: Number(t.dataset.shown), archived: Number(t.dataset.archived) } : null };
  const frames = () => Number(orgView() ? orgView().dataset.frames : NaN);
  const choose = (sel, value) => { const el = document.querySelector(sel); if (el) setValue(el, value, 'change'); return !!el };
  const tick = (sel, on) => { const box = document.querySelector(sel); if (box && box.checked !== on) box.click() };
  const typeIn = async (area, text) => {
    setValue(area, text);
    await timeout(300);
    area.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', bubbles: true, cancelable: true }));
  };
  const pick = async slug => {
    const el = rowEl(slug);
    if (!el) return false;
    if (!el.classList.contains('on')) el.click();
    return !!(await waitFor(() => read() && read().querySelector(`.docket-pane-head[data-ticket="${slug}"][data-full="true"]`), 15000));
  };
  const section = name => read() && read().querySelector(`[data-section="${name}"]`);
  const reason = () => (read() && read().querySelector('.dx-state-reason') || {}).textContent || null;

  dk.seed = await call({ docketSeed: { kind: 'seed' } });
  const t = (dk.seed && dk.seed.tickets) || {};
  // 1. Se abre desde el botón de la barra de la org, con el archivo y el backlog afuera.
  const bell = await waitFor(() => document.querySelector('.dx-org-view .dx-docket-bell'), 5000);
  await waitFor(() => bell && Number(bell.dataset.active) > 2, 10000);
  dk.bell = bell ? { attention: Number(bell.dataset.attention), active: Number(bell.dataset.active), count: (bell.querySelector('.eye-count') || {}).textContent || null } : null;
  if (bell) bell.click();
  dk.opened = !!(await waitFor(() => rowEl(t.runtime), 15000));
  dk.rows = rows();
  dk.totals = totals();
  dk.archivedHidden = !rowEl(t.win10);
  dk.backlogHidden = !rowEl(t.memoria);
  // 2. Las casillas: el archivo y el backlog se agregan al final; los totales no cambian.
  tick('.dx-docket .docket-showarchived input', true);
  dk.archivedShown = !!(await waitFor(() => rowEl(t.win10) && rowEl(t.win10).closest('[data-section="archive"]'), 10000));
  dk.totalsWithArchived = totals();
  tick('.dx-docket .docket-showarchived input', false);
  dk.archivedHiddenAgain = !!(await waitFor(() => !rowEl(t.win10), 10000));
  tick('.dx-docket .docket-showbacklog input', true);
  dk.backlogShown = !!(await waitFor(() => rowEl(t.memoria) && rowEl(t.memoria).closest('[data-section="backlog"]'), 10000));
  // 3. Filtros por estado y por dueño, y el arreglo por estado.
  choose('#dx-docket-status', 'blocked'); await timeout(500);
  dk.blockedRows = rowEls().map(e => e.getAttribute('data-ticket') + ':' + e.getAttribute('data-status'));
  dk.totalsFiltered = totals();
  choose('#dx-docket-status', ''); choose('#dx-docket-owner', 'jefe'); await timeout(500);
  dk.jefeRows = rows();
  choose('#dx-docket-owner', 'Unassigned'); await timeout(500);
  dk.unassignedRows = rows();
  choose('#dx-docket-owner', ''); choose('#dx-docket-group', 'status'); await timeout(500);
  dk.statusGroups = [...document.querySelectorAll('.dx-docket .docket-group-head > span:first-child')].map(e => e.textContent);
  choose('#dx-docket-group', 'none'); await timeout(500);
  dk.subItemDepth = rowEl(t.lzma) ? Number(rowEl(t.lzma).dataset.depth) : null;
  // 4. El detalle: descripción, decisiones, evidencias, artefactos, holders e historial.
  if (await pick(t.runtime)) {
    const p = read();
    dk.detail = {
      title: (p.querySelector('.docket-pane-head b') || {}).textContent || null,
      descBold: !!p.querySelector('.dx-docket-desc strong'),
      descRef: (p.querySelector('.dx-docket-desc a.docket-ref') || {}).textContent || null,
      done: [...p.querySelectorAll('.mark-done li')].map(e => e.textContent),
      decisions: [...p.querySelectorAll('.dx-decisions li')].map(e => e.textContent),
      decisionsSummary: (section('decisions')?.querySelector('.docket-detail-section-summary') || {}).textContent || null,
      evidence: [...p.querySelectorAll('.dx-evidence li')].map(e => e.textContent),
      evidenceSummary: (section('evidence')?.querySelector('.docket-detail-section-summary') || {}).textContent || null,
      artifacts: [...p.querySelectorAll('.dx-artifacts .dx-file-chip')].map(e => e.getAttribute('data-file')),
      artifactSummary: (section('artifacts')?.querySelector('.docket-detail-section-summary') || {}).textContent || null,
      holders: [...p.querySelectorAll('.dx-holders li')].map(e => e.getAttribute('data-holder')),
      historyFolded: !!(section('history') && section('history').querySelector('.docket-detail-section-body[hidden]')),
    };
    const toggle = section('history') && section('history').querySelector('.docket-detail-toggle');
    if (toggle) toggle.click();
    await timeout(400);
    dk.detail.history = [...p.querySelectorAll('.dx-history li')].map(e => e.getAttribute('data-op'));
    dk.detail.historyAssign = (p.querySelector('.dx-history li[data-op="assign"]') || {}).textContent || null;
    dioxus.send({ pause: 'docket' });
    await timeout(3000);
    // un artefacto se guarda en Descargas y se revela en el Explorador; nunca se abre
    const chip = p.querySelector('.dx-artifacts .dx-file-chip');
    if (chip) {
      chip.click();
      dk.artifactToast = await toastWith('Shown in folder');
      const path = dk.artifactToast ? dk.artifactToast.replace(/^Shown in folder: /, '') : null;
      dk.artifactFile = path ? await call({ readDownload: path }) : null;
      dioxus.send({ pause: 'docket-artifact' });
      await timeout(3000);
    }
    // una referencia en la descripción abre ese ticket (acá, el mismo)
    const ref = p.querySelector('.dx-docket-desc a.docket-ref');
    if (ref) {
      ref.click();
      dk.refStays = !!(await waitFor(() => read() && read().querySelector(`.docket-pane-head[data-ticket="${t.runtime}"]`), 3000));
    }
  }
  // 5. Comentar: la respuesta va al dueño del ticket.
  if (await pick(t.docs)) {
    dk.replyLabel = (read().querySelector('.docket-reply-label') || {}).textContent || null;
    const area = read().querySelector('.mail-reply textarea');
    if (area) {
      await typeIn(area, '¿Cómo va la documentación?');
      dk.commentToast = await toastWith('sent to jefe');
    }
  }
  // 6. El dueño (un agente) pasa un ticket a blocked con su motivo: llega en vivo.
  if (await pick(t.lzma)) {
    const before = frames();
    dk.statusChange = await call({ docketSeed: { kind: 'status', slug: t.lzma, status: 'blocked', reason: 'El runner no tiene 7-Zip.' } });
    dk.blockedLive = !!(await waitFor(() => rowEl(t.lzma) && rowEl(t.lzma).dataset.status === 'blocked', 15000));
    dk.blockedReason = await waitFor(() => { const why = reason(); return why && why.includes('7-Zip') && why }, 15000);
    dk.liveFrames = frames() - before;
  }
  // 7. dropped: deja la lista en el acto y queda en el archivo, con por qué terminó.
  dk.dropped = await call({ docketSeed: { kind: 'status', slug: t.docs, status: 'dropped', reason: 'Lo cubre docs/spikes/dioxus.md.' } });
  dk.droppedLeft = !!(await waitFor(() => !rowEl(t.docs), 15000));
  tick('.dx-docket .docket-showarchived input', true);
  dk.droppedArchived = !!(await waitFor(() => rowEl(t.docs) && rowEl(t.docs).closest('[data-section="archive"]'), 10000));
  if (await pick(t.docs)) dk.droppedWhy = await waitFor(() => { const why = reason(); return why && why.includes('docs/spikes') && why }, 10000);
  tick('.dx-docket .docket-showarchived input', false);
  // 8. Bandera: el agente la levanta y llega en vivo; la cola de atención (#28) abre el docket.
  dk.flag = await call({ docketSeed: { kind: 'flag', slug: t.runtime, reason: '¿Publico el runtime de 62,8 MB?' } });
  dk.flagLive = !!(await waitFor(() => rowEl(t.runtime) && rowEl(t.runtime).classList.contains('attention'), 15000));
  dk.bellGlow = !!(await waitFor(() => document.querySelector('.dx-org-view .dx-docket-bell.glow'), 10000));
  document.querySelector('.dx-docket-overlay').click();
  await waitFor(() => !panel(), 5000);
  const attnTab = document.querySelector('.dx-org-view .orgview-tab[data-view="attention"]');
  if (attnTab) attnTab.click();
  const attnRow = await waitFor(() => document.querySelector(`.dx-attn [data-attn-row="ticket:${t.runtime}"]`), 10000);
  if (attnRow) {
    attnRow.querySelector('.mailrow').click();
    dk.attentionPaneFull = !!(await waitFor(() => document.querySelector(`.dx-attn .attn-mread .docket-pane-head[data-ticket="${t.runtime}"][data-full="true"]`), 10000));
    const open = await waitFor(() => document.querySelector('.dx-attn .attn-mread .dx-open-docket'), 5000);
    if (open) open.click();
    dk.openedFromAttention = !!(await waitFor(() => panel() && read() && read().querySelector(`.docket-pane-head[data-ticket="${t.runtime}"]`), 10000));
  }
  // 9. Bajar la bandera: "Dismiss with no comment" pasa el ticket a blocked con su motivo.
  const dismiss = await waitFor(() => read() && read().querySelector('.docket-dismiss'), 10000);
  dk.flagReason = (read() && read().querySelector('.docket-attention-body') || {}).textContent || null;
  dioxus.send({ pause: 'docket-flag' });
  await timeout(3000);
  if (dismiss) {
    dismiss.click();
    dk.dismissToast = await toastWith('dismissed the attention flag');
    dk.dismissBlocked = await waitFor(() => { const why = reason(); return rowEl(t.runtime) && rowEl(t.runtime).dataset.status === 'blocked' && why && why.includes('dismissed') && why }, 15000);
    dk.glowGone = !!(await waitFor(() => !document.querySelector('.dx-org-view .dx-docket-bell.glow'), 10000));
  }
  // 10. Una pregunta adjunta al ticket se responde desde su panel.
  dk.question = await call({ docketSeed: { kind: 'question', slug: t.certificado } });
  if (await pick(t.certificado)) {
    dk.questionWaiting = !!(await waitFor(() => rowEl(t.certificado) && rowEl(t.certificado).querySelector('.docket-qwait'), 10000));
    const card = await waitFor(() => read().querySelector('.docket-question-box .askcard'), 15000);
    dk.questionText = card ? (card.querySelector('.ask-q') || {}).textContent : null;
    if (card) {
      card.querySelector('.ask-row[data-option="Sí"]').click();
      await timeout(300);
      card.querySelector('.ask-submit').click();
      dk.answerToast = await toastWith("resolved worker's batch");
      dk.questionGone = !!(await waitFor(() => !read().querySelector('.docket-question-box .askcard'), 15000));
    }
  }
  // 11. Asignar: "Staff…" en el ticket sin dueño del backlog.
  if (await pick(t.memoria)) {
    const tier = await waitFor(() => read().querySelector('.dx-staff-tier'), 15000);
    dk.staffDisclosure = (read().querySelector('.dx-staff-disclosure') || {}).textContent || null;
    if (tier) {
      setValue(tier, 'haiku', 'change');
      await timeout(400);
      const go = read().querySelector('.dx-staff-go');
      if (go) go.click();
      dk.staffToast = await toastWith('Staffed');
      dk.staffedOwner = (await waitFor(() => { const el = rowEl(t.memoria); return el && el.dataset.owner !== 'Unassigned' && !el.closest('[data-section="backlog"]') && el }, 15000) || { dataset: {} }).dataset.owner || null;
    }
  }
  dk.totalsEnd = totals();
  dk.engine = await call({ docketState: true });
  dk.toasts = toasts();
  tick('.dx-docket .docket-showbacklog input', false);
  document.querySelector('.dx-docket-overlay').click();
  dk.closed = !!(await waitFor(() => !panel(), 5000));
  const chartTab = document.querySelector('.dx-org-view .orgview-tab[data-view="chart"]');
  if (chartTab) chartTab.click();
  await waitFor(() => document.querySelector('.dx-agent[data-node="worker"]'), 10000);
}

// #30: ajustes de la org, filtro de tiers al contratar, ajustes de la app,
// proveedores, cuentas y login (con un `codex` falso en el PATH). Rust
// contesta lo que dice el motor y el estado del login (`setup_probe`).
const st = r.setup = {};
const rpc = async message => { dioxus.send(message); return await dioxus.recv() };
if (home.agentShown) {
  const orgToasts = () => [...document.querySelectorAll('.dx-org-view .toast')].map(t => t.textContent);
  const field = sel => document.querySelector(`.dx-org-settings ${sel}`);
  const openOrgSettings = async () => {
    document.querySelector('.dx-org-view .dx-org-settings-open').click();
    return !!(await waitFor(() => field('[data-setting="compact_at"] input'), 5000));
  };
  const closeOrgSettings = async () => {
    document.querySelector('.dx-org-settings-overlay')?.click();
    await waitFor(() => !document.querySelector('.dx-org-settings'), 5000);
  };
  // 1. Ajustes de la org: Basic (créditos, compactación, esfuerzo y org.md), Policies y Autonomy
  st.orgOpened = await openOrgSettings();
  st.orgBefore = await rpc({ settingsState: true });
  st.orgShown = {
    compact: field('[data-setting="compact_at"] input')?.value, grant: field('[data-setting="default_top_grant"] input')?.value,
    effort: field('[data-setting="default_effort"] select')?.value,
  };
  setValue(field('[data-setting="compact_at"] input'), '70');
  setValue(field('[data-setting="default_top_grant"] input'), '7');
  setValue(field('[data-setting="default_effort"] select'), 'low', 'change');
  const md = await waitFor(() => field('textarea.orgmd-editor'), 10000);
  if (md) setValue(md, '# Charter de prueba\n\nEl equipo del spike de Dioxus.');
  field('[data-tab="policies"]').click();
  const cascade = await waitFor(() => field('[data-setting="cascade_hire"] input'), 5000);
  if (cascade) cascade.click();
  await timeout(300);
  st.orgDirty = document.querySelector('.dx-org-settings').dataset.dirty;
  field('[data-tab="basic"]').click();
  await timeout(200);
  dioxus.send({ pause: 'org-settings' });
  await timeout(3000);
  document.querySelector('.dx-org-settings-save').click();
  st.orgSaveToast = await waitFor(() => orgToasts().find(t => /saved|compaction|error/.test(t)), 10000);
  for (let i = 0; i < 30; i++) {
    st.orgAfter = await rpc({ settingsState: true });
    if (st.orgAfter && st.orgAfter.settings && st.orgAfter.settings.compact_at === 70) break;
    await timeout(300);
  }
  // releer: cerrar y abrir el panel muestra lo que dice el motor
  await closeOrgSettings();
  await timeout(500);
  await openOrgSettings();
  await waitFor(() => field('[data-setting="compact_at"] input')?.value === '70', 10000);
  st.orgReread = {
    compact: field('[data-setting="compact_at"] input')?.value, grant: field('[data-setting="default_top_grant"] input')?.value,
    effort: field('[data-setting="default_effort"] select')?.value,
    orgmd: (await waitFor(() => field('textarea.orgmd-editor'), 10000) || {}).value || null,
  };
  field('[data-tab="policies"]').click();
  st.orgReread.cascadeHire = (await waitFor(() => field('[data-setting="cascade_hire"] input'), 5000) || {}).checked;
  // headless se guarda en el acto; con las políticas de Fable en 'halt' (las
  // de fábrica) el motor lo rechaza, y la vista dice por qué
  field('[data-tab="autonomy"]').click();
  const headless = await waitFor(() => field('.dx-headless input'), 5000);
  if (headless) {
    headless.click();
    st.headlessToast = await waitFor(() => orgToasts().find(t => t.includes('headless')), 10000);
    st.headlessUnchanged = (await rpc({ settingsState: true })).settings.headless === false;
  }
  await closeOrgSettings();

  // 2. Contratar: los tiers según los proveedores instalados (`/api/providers`)
  st.providersEngine = await rpc({ providersState: true });
  if (await pick('@user', 'Hire a top-level agent…')) {
    const form = await waitFor(() => document.querySelector('.dx-hire'), 5000);
    const tierSelect = form && await waitFor(() => form.querySelector('#dx-hire-tier[data-providers="known"]'), 10000);
    if (tierSelect) {
      const options = [...tierSelect.options];
      st.hireTiers = options.map(o => o.value);
      st.hireProviders = [...new Set(options.map(o => o.dataset.provider))];
      st.hireDisabled = options.filter(o => o.disabled).map(o => o.value);
      st.orgTiers = await rpc({ orgTiers: true });
      dioxus.send({ pause: 'hire-filter' });
      await timeout(3000);
    }
    const cancel = form && [...form.querySelectorAll('button')].find(b => b.textContent === 'cancel');
    if (cancel) cancel.click();
    await waitFor(() => !document.querySelector('.dx-hire'), 5000);
  }

  // 3. Ajustes de la app: proveedores, harnesses, cuentas y login
  document.querySelector('.dx-org-view button.home').click();
  const openApp = async () => {
    const button = await waitFor(() => document.querySelector('.welcome-card .dx-app-settings-open'), 10000);
    if (button) button.click();
    return await waitFor(() => document.querySelector('.dx-app-settings'), 5000);
  };
  const appPanel = () => document.querySelector('.dx-app-settings');
  const notes = () => [...document.querySelectorAll('.dx-app-settings .dx-settings-note')].map(n => n.textContent);
  st.appOpened = !!(await openApp());
  const groups = await waitFor(() => { const g = [...document.querySelectorAll('.dx-app-settings .acct-provider-group')]; return g.length >= 3 && g }, 15000) || [];
  st.groups = groups.map(g => ({
    provider: g.dataset.provider,
    state: (g.querySelector('.acct-provider-state') || {}).textContent || null,
    harness: (g.querySelector('.dx-harness') || {}).dataset?.detected ?? null,
    download: !!g.querySelector('.dx-harness-link'),
    tiers: [...g.querySelectorAll('.acct-provider-tier')].length,
  }));
  st.harnesses = await rpc({ harnesses: true });
  dioxus.send({ pause: 'providers' });
  await timeout(3000);
  const codexGroup = () => document.querySelector('.dx-app-settings .acct-provider-group[data-provider="openai"]');
  const rowIds = () => [...(codexGroup()?.querySelectorAll('.account-row') || [])].map(r => r.dataset.account);
  const before = rowIds();
  codexGroup()?.querySelector('.dx-add-account')?.click();
  const managed = await waitFor(() => document.querySelector('.dx-add-account-dialog .dx-create-managed'), 5000);
  if (managed) managed.click();
  st.accountId = await waitFor(() => rowIds().find(id => !before.includes(id)), 15000);
  st.accountsAfterAdd = await rpc({ accountsState: true });
  const row = () => st.accountId && document.querySelector(`.dx-app-settings .account-row[data-account="${st.accountId}"]`);
  if (row()) {
    st.rowAuth = row().dataset.auth;
    st.loginIdle = await rpc({ loginStatus: 'codex' });
    row().querySelector('.dx-signin-start').click();
    st.loginStarting = !!(await waitFor(() => row()?.querySelector('.dx-signin[data-phase="starting"]'), 5000));
    for (let i = 0; i < 30; i++) {
      st.loginRunning = await rpc({ loginStatus: 'codex' });
      if ((st.loginRunning.output || '').includes('fake-codex login')) break;
      await timeout(300);
    }
    st.loginChecks = await rpc({ loginChecks: true });
    row()?.scrollIntoView({ block: 'center' });
    dioxus.send({ pause: 'login' });
    await timeout(3000);
    row().querySelector('.dx-signin-cancel').click();
    st.loginCancelled = !!(await waitFor(() => row()?.querySelector('.dx-signin[data-phase="idle"]'), 5000));
    st.loginAfter = await rpc({ loginStatus: 'codex' });
    // refrescar la cuenta (sin sesión) y quitarla
    row().querySelector('.dx-account-refresh').click();
    st.refreshNote = await waitFor(() => notes().find(n => n.includes(': unauthenticated')), 10000);
    row().querySelector('.dx-account-remove').click();
    st.removed = !!(await waitFor(() => !row(), 10000));
    st.removeNote = notes().find(n => n.includes('removed')) || null;
    st.accountsAfterRemove = await rpc({ accountsState: true });
  }

  // 4. Runtime: límite de turnos y tiempos de turno, guardados y releídos
  const tab = async id => { appPanel().querySelector(`[data-tab="${id}"]`).click(); await timeout(300) };
  await tab('runtime');
  const limit = await waitFor(() => document.querySelector('#app-settings-max-concurrent-turns'), 10000);
  st.runtimeBefore = await rpc({ runtimeState: true });
  const toggle = key => document.querySelector(`.dx-app-settings [data-setting="${key}"] input`);
  if (limit) {
    setValue(limit, '8');
    await timeout(200);
    limit.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', bubbles: true, cancelable: true }));
    for (let i = 0; i < 20; i++) { st.runtimeAfter = await rpc({ runtimeState: true }); if (st.runtimeAfter.max_concurrent_turns === 8) break; await timeout(300) }
    toggle('working_checkups_enabled')?.click();
    await timeout(600);
    toggle('wait_for_mcp_tools_enabled')?.click();
    for (let i = 0; i < 20; i++) {
      st.runtimeAfter = await rpc({ runtimeState: true });
      if (st.runtimeAfter.working_checkups_enabled === false && st.runtimeAfter.wait_for_mcp_tools_enabled === true) break;
      await timeout(300);
    }
    // releer: cerrar y abrir el panel
    document.querySelector('.dx-settings-close').click();
    await waitFor(() => !appPanel(), 5000);
    await openApp();
    await tab('runtime');
    await waitFor(() => document.querySelector('#app-settings-max-concurrent-turns'), 10000);
    await timeout(500);
    st.runtimeReread = {
      limit: document.querySelector('#app-settings-max-concurrent-turns')?.value,
      checkups: toggle('working_checkups_enabled')?.checked, mcp: toggle('wait_for_mcp_tools_enabled')?.checked,
      notifications: document.querySelectorAll('.dx-app-settings [data-pref]').length,
    };
    document.querySelector('#app-settings-max-concurrent-turns')?.scrollIntoView({ block: 'center' });
    dioxus.send({ pause: 'runtime' });
    await timeout(3000);
    // volver a como estaba
    const back = document.querySelector('#app-settings-max-concurrent-turns');
    setValue(back, String(st.runtimeBefore.max_concurrent_turns || 16));
    back.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', bubbles: true, cancelable: true }));
    await timeout(600);
    toggle('working_checkups_enabled')?.click();
    await timeout(600);
    toggle('wait_for_mcp_tools_enabled')?.click();
    await timeout(600);
    st.runtimeRestored = await rpc({ runtimeState: true });
  }

  // 5. Display: el tema se guarda en las preferencias y se aplica en el acto
  await tab('display');
  const themeSelect = await waitFor(() => document.querySelector('.dx-app-settings .dx-theme-select'), 5000);
  if (themeSelect) {
    setValue(themeSelect, 'codex', 'change');
    st.themeApplied = await waitFor(() => document.querySelector('#dx-theme[data-theme="codex"]')
      && getComputedStyle(document.documentElement).getPropertyValue('--accent').trim(), 5000);
    st.themePrefs = await rpc({ prefs: true });
    dioxus.send({ pause: 'theme' });
    await timeout(3000);
    setValue(document.querySelector('.dx-app-settings .dx-theme-select'), 'claude', 'change');
    st.themeBack = await waitFor(() => document.querySelector('#dx-theme[data-theme="claude"]')
      && getComputedStyle(document.documentElement).getPropertyValue('--accent').trim(), 5000);
  }
  document.querySelector('.dx-settings-close').click();
  st.appClosed = !!(await waitFor(() => !appPanel(), 5000));
  // volver a la org del fixture para el desk
  const again = await waitFor(() => [...document.querySelectorAll('.welcome-card nav .org')].find(el => el.textContent.includes('spike-fixture')), 15000);
  if (again) again.click();
  st.backInOrg = !!(await waitFor(() => document.querySelector('.dx-agent[data-node="worker"]'), 15000));
}

// #12: desk
const desk = r.desk = {};
const card = document.querySelector('.dx-agent[data-node="worker"]');
if (card) {
  card.click();
  const msgs = await waitFor(() => document.querySelector('.dx-desk .msgs'), 15000);
  desk.opened = !!msgs;
  if (msgs) {
    await waitFor(() => msgs.querySelectorAll('.msg').length, 15000);
    const beat = () => (msgs.textContent.match(/latido (\d+)/g) || []).map(m => Number(m.split(' ')[1]));
    const first = await waitFor(() => { const b = beat(); return b.length && Math.max(...b) }, 20000);
    const later = await waitFor(() => { const b = beat(); return b.length && Math.max(...b) > (first || 0) && Math.max(...b) }, 10000);
    desk.live = { first, later };
    const numbers = () => (msgs.textContent.match(/(?:Mensaje|Respuesta) (\d+)/g) || []).map(m => Number(m.split(' ')[1]));
    const oldest = () => { const n = numbers(); return n.length ? Math.min(...n) : null };
    desk.earlierPages = 0;
    const pagingStart = performance.now();
    for (let page = 0; page < 40; page++) {
      // hasta que no queden páginas anteriores (el aviso "earlier messages" desaparece)
      if (!document.querySelector('.dx-desk .dx-earlier')) break;
      const before = msgs.querySelectorAll('.msg').length;
      msgs.scrollTop = 0;
      msgs.dispatchEvent(new Event('scroll'));
      if (!(await waitFor(() => msgs.querySelectorAll('.msg').length > before, 8000))) break;
      desk.earlierPages++;
    }
    desk.pagingMs = Math.round(performance.now() - pagingStart);
    msgs.scrollTop = 0;
    await timeout(300);
    desk.domRows = msgs.querySelectorAll('.msg').length;
    desk.oldestLoaded = oldest();
    desk.toolShown = !!msgs.querySelector('.tools.tchip') && msgs.textContent.includes('README');
    // Markdown sanitizado: **negrita** y lista se renderizan, el onerror inyectado no
    desk.markdownBold = !!msgs.querySelector('.msgtext strong');
    desk.markdownList = !!msgs.querySelector('.msgtext ul li');
    desk.injectionBlocked = !msgs.querySelector('[onerror]') && document.title !== 'inyectado';
    // fluidez: recorrer la conversación entera de arriba abajo y medir los cuadros
    const frames = [];
    const duration = 4000, span = Math.max(1, msgs.scrollHeight - msgs.clientHeight);
    await new Promise(done => {
      const start = performance.now(); let last = start;
      const step = now => {
        frames.push(now - last); last = now;
        const t = Math.min(1, (now - start) / duration);
        msgs.scrollTop = span * t;
        if (t < 1) requestAnimationFrame(step); else done();
      };
      requestAnimationFrame(step);
    });
    frames.shift();
    const sorted = [...frames].sort((a, b) => a - b);
    desk.scroll = {
      height: msgs.scrollHeight, frames: frames.length,
      avgMs: Math.round(frames.reduce((a, b) => a + b, 0) / frames.length * 10) / 10,
      p95Ms: Math.round(sorted[Math.floor(sorted.length * 0.95)] * 10) / 10,
      maxMs: Math.round(sorted[sorted.length - 1] * 10) / 10,
      over50ms: frames.filter(f => f > 50).length,
    };
    const n = numbers();
    desk.newestLoaded = n.length ? Math.max(...n) : null;
    // pausa para la captura del CI: el desk con la conversación larga a la vista
    msgs.scrollTop = msgs.scrollHeight / 2;
    dioxus.send({ pause: 'desk' });
    await timeout(3000);

    // #27: desk completo. El contenido sembrado está al final de la conversación.
    const full = r.deskFull = {};
    const bottom = async () => { msgs.scrollTop = msgs.scrollHeight; await timeout(400) };
    await bottom();
    const typed = () => [...msgs.querySelectorAll('.typed-input')].find(t => t.querySelector('.event-mail'));
    const rich = await waitFor(typed, 10000);
    full.segmentText = !!(rich && [...rich.querySelectorAll('.msg.user.msgtext')].some(e => e.textContent.includes('Revisá el informe')));
    full.mailFrom = rich ? [...rich.querySelectorAll('.event-mail .event-actor')].map(e => e.textContent) : [];
    full.mailBold = !!(rich && rich.querySelector('.event-mail .event-prose strong'));
    full.mailTime = rich ? (rich.querySelector('.event-mail time') || {}).textContent || null : null;
    full.notice = rich ? (rich.querySelector('.event-notices .event-prose') || {}).textContent || null : null;
    full.replyQuote = rich ? (rich.querySelector('.reply-preview blockquote') || {}).textContent || null : null;
    full.attachment = rich ? (rich.querySelector('.attach-chip') || {}).textContent || null : null;
    const thought = [...msgs.querySelectorAll('button.thoughtline')].pop();
    full.thinking = thought ? thought.textContent : null;
    if (thought) {
      thought.click();
      full.thinkingOpens = !!(await waitFor(() => (thought.parentElement.querySelector('.thoughtbody') || {}).textContent?.includes('Pienso'), 3000));
    }
    full.localLinks = [...msgs.querySelectorAll('.md a.local-file')].map(a => a.getAttribute('data-local-path'));
    // el motor dejó un aviso de reinicio sin leer: una fila pendiente
    full.pendingAtOpen = msgs.querySelectorAll('.pendrow').length;
    await bottom();
    dioxus.send({ pause: 'desk-content' });
    await timeout(3000);

    // revelar: una ruta relativa (el adjunto) y una inexistente se muestran como texto
    const reveals = () => [...document.querySelectorAll('.dx-desk .toast.dx-reveal')].map(t => t.textContent);
    const toasts = () => [...document.querySelectorAll('.dx-desk .toast')].map(t => t.textContent);
    const toastWith = text => waitFor(() => toasts().find(t => t.includes(text)), 8000);
    const chip = rich && rich.querySelector('.attach-chip');
    if (chip) { chip.click(); full.revealRelative = await waitFor(() => reveals().find(t => t.includes('uploads/informe.txt')), 5000) }
    const missing = [...msgs.querySelectorAll('.md a.local-file')].find(a => a.getAttribute('data-local-path').endsWith('falta.log'));
    if (missing) { missing.click(); full.revealMissing = await waitFor(() => reveals().find(t => t.includes('falta.log')), 5000) }
    // #30: un enlace relativo, de otro esquema o con usuario no navega ni abre
    // nada: se muestra como texto (el https se abre al final, en la etapa externa)
    const links = () => [...document.querySelectorAll('.dx-desk .toast.dx-link')].map(t => t.textContent);
    for (const a of [...msgs.querySelectorAll('.md a:not(.local-file)')].filter(a => /^uploads|^ssh:|usuario/.test(a.getAttribute('href')))) a.click();
    full.linksShown = await waitFor(() => { const l = links(); return l.length >= 3 && l }, 5000);
    full.notNavigated = !!document.querySelector('.dx-desk .msgs');

    // estado del turno: inactivo, en cola por el límite de turnos, trabajando
    const label = () => (document.querySelector('.dx-desk header .turn-status-label') || {}).textContent || null;
    full.idle = await waitFor(() => label() === 'Idle' && 'Idle', 10000);
    const fixtureState = async state => { dioxus.send({ fixtureState: state }); return await dioxus.recv() };
    full.queuedSet = await fixtureState('queued');
    full.queued = await waitFor(() => label() === 'Queued' && 'Queued', 10000);
    full.slotBanner = (await waitFor(() => document.querySelector('.dx-desk .slot-queued-warning'), 5000) || {}).textContent || null;
    dioxus.send({ pause: 'desk-queued' });
    await timeout(3000);
    await fixtureState('working');
    full.working = await waitFor(() => label() === 'Active' && 'Active', 10000);
    full.slotBannerGone = !!(await waitFor(() => !document.querySelector('.dx-desk .slot-queued-warning'), 5000));
    const stop = await waitFor(() => document.querySelector('.dx-desk .cc-send.stop'), 10000);
    full.stopShown = !!stop;
    if (stop) { stop.click(); full.stopResult = await toastWith('no provider call') }

    // enviar a mitad de turno: el mensaje no interrumpe, queda en el buzón
    const ta = document.querySelector('.dx-desk .cc-composer textarea');
    const send = async text => {
      setValue(ta, text);
      await timeout(400);
      ta.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', bubbles: true, cancelable: true }));
      const row = await waitFor(() => [...msgs.querySelectorAll('.pendrow')].find(e => e.textContent.includes(text)), 10000);
      return { pending: !!row, mode: await waitFor(() => (document.querySelector('.dx-desk .sendmode') || {}).textContent, 5000), cleared: ta.value === '' };
    };
    full.sendMidTurn = await send('Hola desde Dioxus a mitad de turno');
    full.stillWorking = label() === 'Active';

    // cambio de modelo a mitad de turno: pide confirmación y queda en cola
    const model = () => document.querySelector('.dx-desk select.dx-model');
    full.models = model() ? [...model().options].map(o => o.value) : null;
    const choose = async tier => { const m = model(); setValue(m, tier, 'change'); await timeout(400) };
    const confirmBox = () => document.querySelector('.dx-desk .dx-switch-confirm');
    await choose('sonnet');
    const queueAsk = await waitFor(confirmBox, 5000);
    full.queueTitle = queueAsk ? queueAsk.querySelector('h3').textContent : null;
    if (queueAsk) {
      queueAsk.querySelector('.danger.solid').click();
      full.queuedToast = await toastWith('QUEUED, not switched');
      full.queuedMark = (await waitFor(() => document.querySelector('.dx-desk header .queued-mark'), 10000) || {}).textContent || null;
    }
    // elegir el modelo actual cancela el cambio en cola
    await choose('haiku');
    full.cancelToast = await toastWith('CANCELLED the queued switch');
    full.queuedMarkGone = !!(await waitFor(() => !document.querySelector('.dx-desk header .queued-mark'), 10000));

    await fixtureState('idle');
    full.idleAgain = await waitFor(() => label() === 'Idle' && 'Idle', 10000);
    // a otro proveedor: la confirmación de la división de linaje; cancelar no cambia nada
    await choose('sol');
    const cross = await waitFor(confirmBox, 5000);
    full.crossTitle = cross ? cross.querySelector('h3').textContent : null;
    full.crossBody = cross ? cross.querySelector('.confirm-body').textContent : null;
    if (cross) [...cross.querySelectorAll('button')].find(b => b.textContent === 'cancel').click();
    full.crossCancelled = !!(await waitFor(() => !confirmBox(), 5000)) && model().value === 'haiku';
    // dentro del mismo proveedor y fuera de un turno: un clic
    await choose('sonnet');
    full.directSwitch = !!(await waitFor(() => model().value === 'sonnet', 10000)) && !confirmBox();
    await choose('haiku');
    full.directBack = !!(await waitFor(() => model().value === 'haiku', 10000));

    // esfuerzo: el popover de cinco puntos
    const eff = () => document.querySelector('.dx-desk .cc-eff');
    full.effortBefore = eff() ? eff().textContent : null;
    if (eff()) {
      eff().click();
      const low = await waitFor(() => document.querySelector('.dx-desk .eff-pop .eff-dot[title="low"]'), 3000);
      full.effortLevels = [...document.querySelectorAll('.dx-desk .eff-pop .eff-dot')].map(d => d.title);
      if (low) low.click();
      full.effortToast = await toastWith('thinking effort: low');
      full.effortAfter = await waitFor(() => eff().textContent === 'low' && eff().classList.contains('set') && 'low', 10000);
    }

    // detenido, con reanudar: el mail queda sin leer hasta reanudar
    const halt = () => document.querySelector('.dx-desk header .halt-control');
    if (halt()) {
      halt().click();
      full.halted = !!(await waitFor(() => document.querySelector('.dx-desk header .badge.halted') && halt().textContent === 'Unhalt', 15000));
      full.haltedBanner = (await waitFor(() => document.querySelector('.dx-desk .halted-send-warning'), 5000) || {}).textContent || null;
      full.sendHalted = await send('Mensaje con el agente detenido');
      await bottom();
      dioxus.send({ pause: 'desk-halted' });
      await timeout(3000);
      halt().click();
      full.unhalted = !!(await waitFor(() => !document.querySelector('.dx-desk header .badge.halted') && label() === 'Idle', 15000));
      full.haltedBannerGone = !document.querySelector('.dx-desk .halted-send-warning');
    }
    full.toasts = toasts();
  }
}

// #14: ventana sin marco. Los botones propios maximizan y restauran; el CI
// arrastra la ventana desde el header con el mouse real durante la pausa.
const native = r.native = {};
const controls = () => document.querySelector('.dx-desk header .window-controls');
native.controls = controls() ? controls().querySelectorAll('.window-control').length : 0;
const maximize = controls() && controls().querySelector('[aria-label="Maximize window"]');
if (maximize) {
  maximize.click();
  native.maximized = !!(await waitFor(() => controls().querySelector('[aria-label="Restore window"]'), 5000));
  const restore = controls().querySelector('[aria-label="Restore window"]');
  if (restore) restore.click();
  native.restored = !!(await waitFor(() => controls().querySelector('[aria-label="Maximize window"]'), 5000));
  await timeout(500);
}
const region = el => { const s = getComputedStyle(el); return s.getPropertyValue('app-region') || s.getPropertyValue('-webkit-app-region') || s.webkitAppRegion || '' };
const title = document.querySelector('.dx-desk header h2');
if (title) {
  const b = title.getBoundingClientRect();
  const x = b.right + 24, y = b.top + b.height / 2;
  const at = document.elementFromPoint(x, y);
  native.drag = { x: Math.round(x), y: Math.round(y), dpr: devicePixelRatio, region: at ? region(at) : null };
}
dioxus.send({ native });
await dioxus.recv();

// #13: el desk en otra ventana con el borrador compartido. La otra mitad de
// esta etapa corre en la ventana del desk (POPOUT_SCRIPT).
const multi = r.multiwindow = {};
const ta = document.querySelector('.dx-desk .cc-composer textarea');
multi.composer = !!ta;
const button = document.querySelector('.dx-desk .dx-popout');
if (ta && button) {
  ta.value = 'escrito en la principal';
  ta.dispatchEvent(new Event('input', { bubbles: true }));
  await timeout(300);
  button.click();
  multi.mirroredFromPopout = !!(await waitFor(() => ta.value === 'escrito en el popout', 30000));
  multi.mainDraft = ta.value;
  dioxus.send({ pause: 'popout' });
  await timeout(3000);
}

// #27: revelar un archivo que existe (al final: abre una ventana del
// administrador de archivos, que no tiene que tapar la prueba de arrastre).
const reveal = r.reveal = {};
const existing = [...document.querySelectorAll('.dx-desk .md a.local-file')].find(a => a.getAttribute('data-local-path').endsWith('informe.txt'));
reveal.path = existing ? existing.getAttribute('data-local-path') : null;
if (existing) {
  existing.click();
  reveal.toast = await waitFor(() => [...document.querySelectorAll('.dx-desk .toast.dx-reveal.ok')].map(t => t.textContent).find(t => t.includes('informe.txt')), 5000);
  reveal.notNavigated = !!document.querySelector('.dx-desk .msgs');
  dioxus.send({ pause: 'reveal' });
  await timeout(3000);
}

// #30: un enlace https del contenido del agente se abre en el navegador por la
// vía controlada en Rust (al final: el navegador taparía la ventana). El CI
// cuenta los navegadores antes y busca el que abrió la URL.
const ext = r.external = {};
const web = [...document.querySelectorAll('.dx-desk .md a:not(.local-file)')].find(a => a.getAttribute('href') === 'https://example.com/orgtree');
ext.found = !!web;
if (web) {
  ext.ciBefore = await rpc({ waitCi: 'external-before' });
  web.click();
  ext.toast = await waitFor(() => [...document.querySelectorAll('.dx-desk .toast.dx-external')].map(t => t.textContent).find(t => t.includes('example.com/orgtree')), 5000);
  ext.notNavigated = !!document.querySelector('.dx-desk .msgs');
  ext.log = await rpc({ externalLog: true });
  dioxus.send({ pause: 'external' });
  await timeout(3000);
}
dioxus.send({ done: r });
"#;

/// Los pedidos de la etapa #28 que la página le hace a Rust. `None` si el
/// mensaje no es de esta etapa.
async fn attention_probe(client: &orgtree_engine_client::Client, message: &serde_json::Value) -> Option<serde_json::Value> {
    use serde_json::json;
    let window = dioxus::desktop::window();
    if let Some(patch) = message.get("notifyPrefs").and_then(|v| v.as_object()) {
        return Some(crate::notify::set_prefs(patch));
    }
    if message.get("notifyLog").is_some() {
        return Some(serde_json::Value::Array(crate::notify::log_snapshot()));
    }
    if message.get("minimize").is_some() {
        window.set_minimized(true);
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        return Some(json!({ "minimized": window.window.is_minimized(), "focused": window.window.is_focused() }));
    }
    if message.get("restore").is_some() {
        crate::native::show_main();
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        return Some(json!({ "minimized": window.window.is_minimized(), "focused": window.window.is_focused() }));
    }
    if let Some(kind) = message.get("attentionSeed").and_then(|v| v.as_str()) {
        let body = json!({ "kind": kind });
        return Some(match client.post::<serde_json::Value>("/api/fixture/attention", &body).await {
            Ok(value) => value,
            Err(e) => json!({ "error": e.to_string() }),
        });
    }
    if let Some(kind) = message.get("clickNotice").and_then(|v| v.as_str()) {
        // el clic en el toast: el CI no puede hacer clic en el centro de
        // notificaciones, así que se llama a lo mismo que el handler `Activated`
        let found = client.notifications().await.ok().and_then(|n| n.notices.into_iter().find(|n| n.kind == kind));
        let tag = found.as_ref().and_then(|n| crate::notify::tag_of(&n.org, &n.id));
        if let Some(tag) = &tag {
            crate::notify::click(tag);
        }
        return Some(json!({ "clicked": tag.is_some(), "tag": tag, "source_id": found.and_then(|n| n.source_id) }));
    }
    // #29: el fixture siembra el docket o un agente lo cambia (`POST /api/fixture/docket`)
    if let Some(body) = message.get("docketSeed") {
        return Some(match client.post::<serde_json::Value>("/api/fixture/docket", body).await {
            Ok(value) => value,
            Err(e) => json!({ "error": e.to_string() }),
        });
    }
    // #29: lo que dice el motor de cada ticket, para compararlo con la vista
    if message.get("docketState").is_some() {
        let work = client.work_items_view("spike-fixture", true, true).await.ok()?;
        let all = [Some(&work.items), work.archived.as_ref(), work.backlogged.as_ref()];
        let state: serde_json::Map<String, serde_json::Value> = all
            .into_iter()
            .flatten()
            .flatten()
            .map(|i| (i.slug.clone(), json!({ "status": i.status, "owner": i.owner_node(), "flagged": i.manual_attention.is_some(), "blocked_reason": i.blocked_reason, "archived": i.archived })))
            .collect();
        return Some(json!({ "items": state, "counts": work.counts }));
    }
    // #29: el archivo que se guardó en Descargas (solo ahí), para ver que es el artefacto
    if let Some(path) = message.get("readDownload").and_then(|v| v.as_str()) {
        let inside = std::path::Path::new(path).components().any(|c| c.as_os_str() == "Orgtree");
        return Some(match std::fs::read_to_string(path) {
            Ok(text) if inside => json!({ "exists": true, "text": text }),
            Ok(_) => json!({ "exists": true, "outside": true }),
            Err(e) => json!({ "exists": false, "error": e.to_string() }),
        });
    }
    if let Some(slug) = message.get("workItem").and_then(|v| v.as_str()) {
        let work = client.work_items("spike-fixture").await.ok()?;
        let all = [Some(&work.items), work.attention.as_ref(), work.archived.as_ref(), work.backlogged.as_ref()];
        let item = all.into_iter().flatten().flatten().find(|i| i.slug == slug)?;
        return Some(json!({ "status": item.status, "flagged": item.manual_attention.is_some() }));
    }
    None
}

/// Los pedidos de la etapa #30 que la página le hace a Rust: lo que dice el
/// motor (ajustes de la org y de la app, proveedores, cuentas), el estado del
/// login, los harnesses detectados, las preferencias y los enlaces externos.
/// `None` si el mensaje no es de esta etapa.
async fn setup_probe(client: &orgtree_engine_client::Client, message: &serde_json::Value) -> Option<serde_json::Value> {
    use serde_json::json;
    let err = |e: orgtree_engine_client::ClientError| json!({ "error": e.to_string() });
    if message.get("settingsState").is_some() {
        let tree = match client.tree("spike-fixture").await {
            Ok(tree) => tree,
            Err(e) => return Some(err(e)),
        };
        let s = orgtree_engine_client::OrgSettings::from_tree(&tree);
        let md = client.org_md("spike-fixture").await.ok().map(|m| m.content);
        return Some(json!({
            "settings": { "max_top_grant": s.max_top_grant, "default_top_grant": s.default_top_grant, "compact_at": s.compact_at,
                          "default_effort": s.default_effort, "cascade_hire": s.cascade_hire, "cascade_alloc": s.cascade_alloc, "headless": s.headless },
            "orgmd": md,
        }));
    }
    if message.get("orgTiers").is_some() {
        return Some(match client.tree("spike-fixture").await {
            Ok(tree) => json!(tree.tiers.keys().collect::<Vec<_>>()),
            Err(e) => err(e),
        });
    }
    if message.get("providersState").is_some() {
        return Some(match client.providers().await {
            Ok(p) => json!(p
                .providers
                .iter()
                .map(|p| (p.id.clone(), json!({ "installed": p.status.installed, "hire_enabled": p.hire_enabled, "offer": format!("{:?}", p.offer()) })))
                .collect::<serde_json::Map<_, _>>()),
            Err(e) => err(e),
        });
    }
    if message.get("runtimeState").is_some() {
        return Some(match client.runtime_settings().await {
            Ok(r) => json!({ "max_concurrent_turns": r.max_concurrent_turns, "working_checkups_enabled": r.working_checkups_enabled,
                             "wait_for_mcp_tools_enabled": r.wait_for_mcp_tools_enabled, "warming_enabled": r.warming_enabled }),
            Err(e) => err(e),
        });
    }
    if message.get("accountsState").is_some() {
        return Some(match client.accounts().await {
            Ok(registry) => json!(registry
                .accounts
                .iter()
                .map(|a| json!({ "id": a.id, "provider": a.provider, "kind": a.credential.kind, "auth": a.standing.auth, "ambient": a.ambient }))
                .collect::<Vec<_>>()),
            Err(e) => err(e),
        });
    }
    if message.get("harnesses").is_some() {
        // la ruta del CLI no sale del shell: solo si está
        return Some(json!(crate::harnesses::detect().iter().map(|h| json!({ "id": h.id, "detected": h.detected(), "url": h.url })).collect::<Vec<_>>()));
    }
    if let Some(provider) = message.get("loginStatus").and_then(|v| v.as_str()) {
        let provider = crate::login::provider(provider).ok()?;
        return Some(json!(crate::login::logins().status(provider)));
    }
    if message.get("loginChecks").is_some() {
        // con el login de codex en curso: un proveedor inventado, un harness no
        // detectado, un segundo login del mismo y un código pegado a codex
        let logins = crate::login::logins();
        let missing = crate::harnesses::detect().into_iter().find(|h| !h.detected() && h.id != "codex").map(|h| h.id);
        let refused = missing.map(|id| logins.start(id, Default::default(), crate::login::EngineAccess::current()));
        let again = logins.start("codex", Default::default(), crate::login::EngineAccess::current());
        return Some(json!({
            "badProvider": crate::login::provider("codex --help").err(),
            "missingId": missing,
            "missing": refused,
            "again": again,
            "code": logins.submit_code("codex", "123456").err(),
            "harnessLinkBad": crate::harnesses::link("https://example.com").is_none(),
        }));
    }
    if message.get("prefs").is_some() {
        return Some(crate::notify::prefs());
    }
    if message.get("externalLog").is_some() {
        return Some(json!(crate::external::log_snapshot()));
    }
    None
}

/// La mitad de la etapa #13 que corre en la ventana del desk.
const POPOUT_SCRIPT: &str = r#"
const timeout = ms => new Promise(done => setTimeout(done, ms));
const waitFor = async (test, ms) => {
  const end = Date.now() + ms;
  while (Date.now() < end) { const value = test(); if (value) return value; await timeout(200) }
  return null;
};
const r = {};
const ta = await waitFor(() => document.querySelector('.dx-desk .cc-composer textarea'), 15000);
r.opened = !!ta;
r.messages = (await waitFor(() => document.querySelectorAll('.dx-desk .msg').length, 15000)) || 0;
r.fontFamily = getComputedStyle(document.body).fontFamily;
r.bridge = typeof window.orgtreeDesktop;
r.mirroredFromMain = !!(await waitFor(() => ta && ta.value === 'escrito en la principal', 15000));
if (ta) {
  ta.value = 'escrito en el popout';
  ta.dispatchEvent(new Event('input', { bubbles: true }));
}
dioxus.send({ ready: r });
// Rust avisa cuando ya cerró (ocultó) la ventana principal.
await dioxus.recv();
await timeout(500);
dioxus.send({ afterOwnerClose: { alive: true, draft: ta && ta.value, messages: document.querySelectorAll('.dx-desk .msg').length } });
"#;

/// #24, la app instalada (`ORGTREE_DIOXUS_PROBE_MODE=installed`): el motor
/// empaquetado sobre una raíz nueva, sin orgs. La UI RSX carga con el CSS del
/// renderer, la lista de orgs llega (vacía) y la raíz de datos está a la vista.
const INSTALLED_SCRIPT: &str = r#"
const timeout = ms => new Promise(done => setTimeout(done, ms));
const waitFor = async (test, ms) => {
  const end = Date.now() + ms;
  while (Date.now() < end) { const value = test(); if (value) return value; await timeout(200) }
  return null;
};
const r = {};
// #30: una instalación nueva, sin orgs, abre el primer uso; sin harnesses en el
// PATH explica qué instalar, con los enlaces oficiales. Saltearlo lo guarda.
const onboard = await waitFor(() => document.querySelector('.dx-onboarding'), 30000);
const ob = r.onboarding = { shown: !!onboard };
if (onboard) {
  ob.harnesses = [...onboard.querySelectorAll('.dx-onboard-harness')].map(h => ({ id: h.dataset.harness, detected: h.dataset.detected === 'true', link: !!h.querySelector('.dx-harness-link') }));
  ob.noHarness = (onboard.querySelector('.dx-no-harness') || {}).textContent || null;
  ob.themes = onboard.querySelectorAll('.onboard-theme').length;
  ob.newOrg = !!onboard.querySelector('.dx-new-org');
  ob.fontFamily = getComputedStyle(onboard).fontFamily;
  dioxus.send({ pause: 'onboarding' });
  await timeout(3000);
  onboard.querySelector('.dx-onboard-skip').click();
  ob.skipped = !!(await waitFor(() => !document.querySelector('.dx-onboarding') && document.querySelector('.welcome-card nav'), 10000));
  dioxus.send({ prefs: true });
  ob.prefs = await dioxus.recv();
}
const card = await waitFor(() => document.querySelector('.welcome-card'), 30000);
r.card = !!card;
r.version = (document.querySelector('.welcome-card .build-badge') || {}).textContent || null;
r.fontFamily = card ? getComputedStyle(card).fontFamily : null;
r.bridge = typeof window.orgtreeDesktop;
// la lista viene del motor por el cliente Rust: en una raíz nueva no hay orgs
r.orgList = !!(await waitFor(() => document.querySelector('.welcome-card nav'), 30000));
r.orgs = document.querySelectorAll('.welcome-card nav .org').length;
r.empty = !!(await waitFor(() => [...document.querySelectorAll('.welcome-card .dim')].find(el => el.textContent.includes('no organizations')), 5000));
r.listError = (document.querySelector('.welcome-card .org-freshness') || {}).textContent || null;
r.dataRoot = (document.querySelector('.welcome-card .dx-data-root code') || {}).textContent || null;
r.mode = (document.querySelector('.welcome-card .dx-launch-mode') || {}).textContent || null;
// pausa para la captura del CI: la app instalada con la raíz a la vista
dioxus.send({ pause: 'installed' });
await timeout(3000);
dioxus.send({ done: r });
"#;

fn installed_mode() -> bool {
    std::env::var("ORGTREE_DIOXUS_PROBE_MODE").is_ok_and(|mode| mode == "installed")
}

async fn installed_probe() {
    let mut eval = document::eval(INSTALLED_SCRIPT);
    let page = loop {
        match eval.recv::<serde_json::Value>().await {
            Ok(message) => {
                if let Some(name) = message.get("pause").and_then(|v| v.as_str()) {
                    marker(name);
                } else if message.get("prefs").is_some() {
                    let _ = eval.send(crate::notify::prefs());
                } else if let Some(done) = message.get("done") {
                    break done.clone();
                }
            }
            Err(error) => break serde_json::json!({ "error": error.to_string() }),
        }
    };
    record("installed", page);
    if let Ok(launch) = crate::launch() {
        let engine = crate::ENGINE.lock().unwrap().as_ref().map(|e| e.data_root().display().to_string());
        record("launch", serde_json::json!({
            "packaged": launch.packaged,
            "data_root": launch.options.data_root,
            "engine_data_root": engine,
            "python": launch.options.python,
            "bootstrap_postgres": launch.options.bootstrap_postgres,
            "descriptor": launch.descriptor,
        }));
    }
    // Salir, como desde la bandeja: cierra todo y apaga el motor.
    crate::native::quit();
}

/// El último estado de arranque (por ejemplo, por qué el motor no arrancó),
/// para que el CI lo vea aunque la UI no llegue a montarse.
pub fn startup_status(text: &str) {
    record("startup_status", serde_json::json!(text));
    // En la prueba de la app instalada, un arranque fallido termina la app: el CI
    // no espera en vano y diagnostica.
    if installed_mode() && report_path().is_some() && text.starts_with("El motor no arrancó") {
        crate::native::quit();
    }
}

/// Reporte compartido por las dos ventanas.
static REPORT: Mutex<Option<serde_json::Map<String, serde_json::Value>>> = Mutex::new(None);
/// La principal ya se cerró (se ocultó): la ventana del desk sigue con su parte.
static OWNER_CLOSED: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// Llegó el aviso de una segunda ejecución.
static SECOND_INSTANCE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Una segunda ejecución avisó a esta instancia (#14), que ya mostró su ventana.
pub fn second_instance(message: &str) {
    if report_path().is_none() {
        return;
    }
    let mut visible = None;
    crate::windows::with_main(|main| visible = Some(main.window.is_visible()));
    record("second_instance", serde_json::json!({ "message": message, "main_visible": visible }));
    SECOND_INSTANCE.notify_one();
}

/// Lo que el CI necesita para verificar la ventana nativa: el HWND (para
/// arrastrarla con el mouse real), si tiene marco y si hay bandeja.
fn native_shell() -> serde_json::Value {
    let window = dioxus::desktop::window();
    #[cfg(windows)]
    let hwnd = {
        use dioxus::desktop::tao::platform::windows::WindowExtWindows;
        Some(window.window.hwnd() as isize)
    };
    #[cfg(not(windows))]
    let hwnd: Option<isize> = None;
    serde_json::json!({
        "hwnd": hwnd,
        "decorated": window.window.is_decorated(),
        "scale": window.window.scale_factor(),
        "tray": crate::native::tray_ready(),
        "notify": crate::native::notify("Orgtree", "Notificación nativa del spike de Dioxus").map(|_| true).unwrap_or_else(|e| { let _ = e; false }),
    })
}

/// Espera a que el CI termine lo que hace durante una pausa (`<salida>.<nombre>-done`).
async fn wait_for_ci(name: &str) -> bool {
    let Some(out) = report_path() else { return false };
    let mut done = out.into_os_string();
    done.push(format!(".{name}-done"));
    for _ in 0..200 {
        if std::path::Path::new(&done).exists() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    false
}

fn report_path() -> Option<std::path::PathBuf> {
    std::env::var_os("ORGTREE_DIOXUS_PROBE").filter(|v| !v.is_empty()).map(std::path::PathBuf::from)
}

fn record(key: &str, value: serde_json::Value) {
    let Some(out) = report_path() else { return };
    let mut report = REPORT.lock().unwrap();
    let report = report.get_or_insert_with(Default::default);
    report.insert(key.to_string(), value);
    let _ = std::fs::write(out, serde_json::to_vec_pretty(report).unwrap_or_default());
}

fn marker(name: &str) {
    let Some(out) = report_path() else { return };
    let mut marker = out.into_os_string();
    marker.push(format!(".{name}"));
    let _ = std::fs::write(marker, b"");
}

#[component]
pub fn Probe() -> Element {
    let client = crate::engine_client();
    use_future(move || {
        let client = client.clone();
        async move {
        if report_path().is_none() {
            return;
        }
        if installed_mode() {
            return installed_probe().await;
        }
        let mut eval = document::eval(SCRIPT);
        let report = loop {
            match eval.recv::<serde_json::Value>().await {
                Ok(message) => {
                    if let Some(name) = message.get("pause").and_then(|v| v.as_str()) {
                        marker(name);
                    } else if let Some(page) = message.get("native") {
                        // #14: datos para el CI, pausa mientras arrastra la ventana, y seguir.
                        record("native", serde_json::json!({ "page": page, "shell": native_shell() }));
                        marker("native");
                        let finished = wait_for_ci("native").await;
                        record("native_ci_done", serde_json::json!(finished));
                        let _ = eval.send(serde_json::json!(true));
                    } else if let Some(name) = message.get("externalHire").and_then(|v| v.as_str()) {
                        // #26: una contratación por fuera de la UI, con el cliente Rust directo;
                        // la UI tiene que verla llegar por el WebSocket de la org.
                        let request = orgtree_engine_client::OpRequest::hire(None, "haiku", name, 0, None);
                        let result = match client.op("prueba-dioxus", &request).await {
                            Ok(r) => serde_json::json!({ "node": r.node, "warnings": r.warnings }),
                            Err(e) => serde_json::json!({ "error": e.to_string() }),
                        };
                        let _ = eval.send(result);
                    } else if let Some(state) = message.get("fixtureState").and_then(|v| v.as_str()) {
                        // #27: el motor de fixture simula el estado del turno (solo en el fixture).
                        let body = serde_json::json!({ "node": "worker", "state": state });
                        let result = match client.post::<serde_json::Value>("/api/fixture/turn-state", &body).await {
                            Ok(value) => value,
                            Err(e) => serde_json::json!({ "error": e.to_string() }),
                        };
                        let _ = eval.send(result);
                    } else if let Some(name) = message.get("waitCi").and_then(|v| v.as_str()) {
                        // #30: una pausa en la que el CI hace algo y avisa con `<salida>.<nombre>-done`
                        marker(name);
                        let finished = wait_for_ci(name).await;
                        let _ = eval.send(serde_json::json!(finished));
                    } else if let Some(reply) = setup_probe(&client, &message).await {
                        // #30: ajustes, proveedores, cuentas, login, preferencias y enlaces
                        let _ = eval.send(reply);
                    } else if let Some(reply) = attention_probe(&client, &message).await {
                        // #28: preferencias, ventana, siembra del fixture, registro y clic
                        let _ = eval.send(reply);
                    } else if let Some(done) = message.get("done") {
                        break done.clone();
                    }
                }
                Err(error) => break serde_json::json!({ "error": error.to_string() }),
            }
        };
        // `native` ya quedó guardado con la parte del shell durante la pausa.
        for key in ["home", "chart", "attention", "docket", "setup", "desk", "deskFull", "multiwindow", "reveal", "external", "error"] {
            if let Some(value) = report.get(key) {
                record(key, value.clone());
            }
        }
        // #13: cerrar la principal con el desk abierto en otra ventana.
        if crate::windows::popout_count() == 0 {
            return;
        }
        let main = dioxus::desktop::window();
        main.close();
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        record("owner_close", serde_json::json!({
            "main_visible": main.window.is_visible(),
            "popouts": crate::windows::popout_count(),
        }));
        OWNER_CLOSED.notify_one();
        // #14: el desk se cierra solo; sin ventanas visibles, la app sigue en la bandeja.
        for _ in 0..50 {
            if crate::windows::popout_count() == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        record("tray_only", serde_json::json!({
            "main_visible": main.window.is_visible(),
            "popouts": crate::windows::popout_count(),
        }));
        marker("tray");
        // El CI lanza una segunda ejecución: tiene que volver a mostrar la principal.
        let woke = tokio::time::timeout(std::time::Duration::from_secs(60), SECOND_INSTANCE.notified()).await.is_ok();
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        record("quit", serde_json::json!({ "after_second_instance": woke, "main_visible": main.window.is_visible() }));
        // Salir, como desde el menú de la bandeja: cierra todo y apaga el motor.
        crate::native::quit();
        }
    });
    rsx! {}
}

/// La parte de la prueba que corre en la ventana del desk (#13). Al terminar
/// cierra su ventana: con la principal oculta, la app tiene que salir sola.
#[component]
pub fn PopoutProbe() -> Element {
    use_future(|| async move {
        if report_path().is_none() {
            return;
        }
        let mut eval = document::eval(POPOUT_SCRIPT);
        match eval.recv::<serde_json::Value>().await {
            Ok(message) => record("popout", message.get("ready").cloned().unwrap_or(message)),
            Err(error) => return record("popout", serde_json::json!({ "error": error.to_string() })),
        }
        OWNER_CLOSED.notified().await;
        let _ = eval.send(serde_json::json!(true));
        match eval.recv::<serde_json::Value>().await {
            Ok(message) => record("popout_after_owner_close", message.get("afterOwnerClose").cloned().unwrap_or(message)),
            Err(error) => record("popout_after_owner_close", serde_json::json!({ "error": error.to_string() })),
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        dioxus::desktop::window().close();
    });
    rsx! {}
}

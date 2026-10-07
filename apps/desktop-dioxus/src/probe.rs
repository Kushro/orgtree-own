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
    // un enlace externo o relativo no navega ni abre nada: se muestra como texto
    const links = () => [...document.querySelectorAll('.dx-desk .toast.dx-link')].map(t => t.textContent);
    for (const a of [...msgs.querySelectorAll('.md a:not(.local-file)')].filter(a => /example\.com|^uploads/.test(a.getAttribute('href')))) a.click();
    full.linksShown = await waitFor(() => { const l = links(); return l.length >= 2 && l }, 5000);
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
    if let Some(slug) = message.get("workItem").and_then(|v| v.as_str()) {
        let work = client.work_items("spike-fixture").await.ok()?;
        let all = [Some(&work.items), work.attention.as_ref(), work.archived.as_ref(), work.backlogged.as_ref()];
        let item = all.into_iter().flatten().flatten().find(|i| i.slug == slug)?;
        return Some(json!({ "status": item.status, "flagged": item.manual_attention.is_some() }));
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
        for key in ["home", "chart", "attention", "desk", "deskFull", "multiwindow", "reveal", "error"] {
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

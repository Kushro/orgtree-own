// Etapa final de la prueba (#21, ver probe.rs), cuando el CI la pide: abre de
// verdad el Explorador y el navegador con el puente. Va al final porque esas
// ventanas taparían la prueba de arrastre. __PREFIX__ y __TARGET__ los pone Rust.
(async () => {
  if (window.__orgtreeLateProbeRan) return;
  window.__orgtreeLateProbeRan = true;
  const r = {};
  const bridge = window.orgtreeDesktop;
  // Un .cmd: el Explorador lo tiene que seleccionar, nunca ejecutar.
  try { r.reveal = await bridge.revealFile(__TARGET__) } catch (e) { r.reveal = 'error:' + e }
  try { r.charters = await bridge.openCharterFolder() } catch (e) { r.charters = 'error:' + e }
  try { await bridge.openHarnessLink('codex'); r.harnessLink = 'resolved' } catch (e) { r.harnessLink = 'error:' + e }
  r.target = __TARGET__;
  document.title = '__PREFIX__' + JSON.stringify(r);
})();

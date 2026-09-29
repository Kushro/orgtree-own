"""The real App's v3 window chrome in Chromium: screenshots and geometry.

    node apps/desktop/renderer/tests/appchrome-build.mjs <bundle>
    python -B apps/desktop/renderer/tests/appchrome_probe.py <bundle> <outdir>

Writes <outdir>/chrome-*.png and <outdir>/chrome.json. Reports numbers only;
it asserts that the fixture actually mounted (a header and a status strip
exist), so an empty page can never read as a clean measurement.
"""
from __future__ import annotations

import functools
import http.server
import json
import sys
import threading
from pathlib import Path

from playwright.sync_api import sync_playwright

BUNDLE = Path(sys.argv[1]).resolve()
OUT = Path(sys.argv[2]).resolve()
OUT.mkdir(parents=True, exist_ok=True)
assert (BUNDLE / 'appchrome-fixture.js').exists(), 'INERT: bundle missing'


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if '.' not in self.path.split('?')[0].rsplit('/', 1)[-1]:
            self.path = '/index.html'
        super().do_GET()


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(BUNDLE)))
threading.Thread(target=server.serve_forever, daemon=True).start()
base = f'http://127.0.0.1:{server.server_port}/o/studio'

GEOMETRY = """() => {
  const r = (el) => { if (!el) return null; const b = el.getBoundingClientRect();
    return { top: b.top, left: b.left, right: b.right, bottom: b.bottom, width: b.width, height: b.height } }
  const header = document.querySelector('.shell-header')
  const actions = [...document.querySelectorAll('.shell-header-actions > *, .shell-header-actions .kill > *')]
  const vis = (el) => { const s = getComputedStyle(el); return s.display !== 'none' && s.visibility !== 'hidden' && el.getClientRects().length > 0 }
  const visibleText = (el) => [...el.querySelectorAll('*')].concat([el]).filter(e => vis(e))
    .flatMap(e => [...e.childNodes].filter(n => n.nodeType === 3).map(n => n.textContent.trim())).filter(Boolean).join(' ')
  const buttons = [...document.querySelectorAll('.shell-header button')].filter(vis).map(b => {
    const s = getComputedStyle(b)
    return { cls: b.className, aria: b.getAttribute('aria-label'), title: b.getAttribute('title'),
      text: visibleText(b), rect: r(b), border: s.borderTopWidth + ' ' + s.borderTopStyle + ' ' + s.borderTopColor,
      background: s.backgroundColor, radius: s.borderTopLeftRadius }
  })
  const main = document.querySelector('main')
  const ms = main ? getComputedStyle(main) : null
  return {
    viewport: { w: innerWidth, h: innerHeight },
    header: r(header), statusbar: r(document.querySelector('.shell-statusbar')),
    stage: r(document.querySelector('.canvas-stage')), viewportEl: r(document.querySelector('.viewport')),
    controls: r(document.querySelector('.window-controls')),
    close: r(document.querySelector('.window-control.close')),
    mainPadding: ms && ms.padding,
    buttons,
    modes: r(document.querySelector('.shell-header-modes')),
    modesText: document.querySelector('.shell-header-modes') ? visibleText(document.querySelector('.shell-header-modes')) : null,
    knob: r(document.querySelector('.shell-switch-knob')),
    kill: r(document.querySelector('.shell-header .kill')),
    killParts: [...document.querySelectorAll('.shell-header .kill > *')].filter(vis).map(e => ({ cls: e.className, rect: r(e) })),
    actionsBox: r(document.querySelector('.shell-header-actions')),
    version: [...document.querySelectorAll('.shell-header *, .shell-statusbar *')].filter(e => vis(e) && /3\\.0\\.0-alpha\\.0/.test(e.textContent || '') && !e.children.length).map(e => ({ cls: e.className, text: e.textContent.trim() })),
  }
}"""

result = {'url': base, 'cases': {}, 'errors': []}
try:
    with sync_playwright() as pw:
        browser = pw.chromium.launch(channel='msedge')
        for name, query, width, action in [
            ('canvas', '', 1600, None),
            ('attention', '', 1600, 'attention'),
            ('killswitch', '?killswitch=1', 1600, None),
            ('armed', '', 1600, 'arm'),
            ('menu', '', 1600, 'menu'),
            ('narrow', '', 700, None),
            ('min-armed', '', 640, 'arm'),
            ('min-killswitch', '?killswitch=1', 640, None),
            ('home', '?view=home', 1200, None),
        ]:
            page = browser.new_page(viewport={'width': width, 'height': 820})
            page.on('pageerror', lambda e: result['errors'].append(str(e)))
            page.goto(base + query)
            page.locator('.shell-header').wait_for(timeout=15000)
            if 'view=home' not in query:
                page.locator('.shell-statusbar').wait_for(timeout=15000)
            page.wait_for_timeout(1200)
            if action == 'attention':
                # base: a two-radio group; tip: one switch
                sw = page.locator('.shell-header [role="switch"]')
                (sw if sw.count() else page.locator('.shell-modes [role="radio"]').nth(1)).click()
                page.wait_for_timeout(600)
            if action == 'arm':
                kill = page.locator(".shell-header .kill-latch, .shell-header .kill > button:last-child").first
                before = page.evaluate(GEOMETRY)
                result['cases']['armed-before'] = before
                kill.click()
                page.wait_for_timeout(400)
            if action == 'menu':
                page.locator('.shell-menu-button').click()
                page.wait_for_timeout(300)
            geom = page.evaluate(GEOMETRY)
            if action == 'menu':
                geom['menuItems'] = page.locator('.shell-menu-panel [role="menuitem"]').all_inner_texts()
                geom['menuText'] = page.locator('.shell-menu-panel').inner_text()
            result['cases'][name] = geom
            page.screenshot(path=str(OUT / f'chrome-{name}.png'))
            page.screenshot(path=str(OUT / f'chrome-{name}-top.png'),
                            clip={'x': 0, 'y': 0, 'width': width, 'height': 60})
            page.close()
        browser.close()
finally:
    server.shutdown()

(OUT / 'chrome.json').write_text(json.dumps(result, indent=2), encoding='utf8')
print(json.dumps({k: {kk: v.get(kk) for kk in ('header', 'statusbar', 'mainPadding', 'close', 'modesText', 'kill')}
                  for k, v in result['cases'].items()}, indent=1))
print('errors', result['errors'][:5])

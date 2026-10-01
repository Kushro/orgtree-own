// pinscrolldrag_electron_probe.cjs — real app (appchrome fixture, fake data,
// private userData, no engine) with the Attention desk pinned flush to the
// canvas top. Maps the header's native hit test (WM_NCHITTEST: D = drag,
// . = client, # = frame) before and after maximizing; SCROLL=1 first fills the
// desk with a long transcript scrolled to its end, which is what knocked the
// header above the panel out of the drag region (see pinscrolldrag.test.tsx).
// Without NODRAG=1 it also drives real mouse drags (SetCursorPos +
// mouse_event) on the header above the panel; that needs an interactive
// desktop — from a service session (session 0) the cursor calls fail and no
// drag happens, so read only the hit maps there.
//   node tests/appchrome-build.mjs <bundle>
//   env BUNDLE=<bundle> UDATA=<empty dir> OUTF=<result json> SCROLL=1 NODRAG=1 [CTL=1]
//   <repo>/node_modules/electron/dist/electron.exe tests/pinscrolldrag_electron_probe.cjs
// Never run against the live app.
const { app, BrowserWindow, screen } = require('electron')
const http = require('http'), fs = require('fs'), path = require('path'), cp = require('child_process')
const BUNDLE = process.env.BUNDLE, OUTF = process.env.OUTF
app.setPath('userData', process.env.UDATA)
const types = { '.js': 'text/javascript', '.css': 'text/css', '.html': 'text/html' }
const srv = http.createServer((q, r) => {
  const f = q.url.split('?')[0] === '/' || q.url.startsWith('/o/') ? '/index.html' : q.url.split('?')[0]
  const p = path.join(BUNDLE, f)
  if (!fs.existsSync(p)) { r.writeHead(404); return r.end() }
  r.writeHead(200, { 'Content-Type': types[path.extname(p)] || 'text/plain' }); r.end(fs.readFileSync(p))
})
const PS_HEAD = `Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class W{[DllImport("user32.dll")]public static extern IntPtr SendMessage(IntPtr h,uint m,IntPtr w,IntPtr l);[DllImport("user32.dll")]public static extern bool SetCursorPos(int x,int y);[DllImport("user32.dll")]public static extern void mouse_event(uint f,uint x,uint y,uint d,IntPtr e);[DllImport("user32.dll")]public static extern bool SetForegroundWindow(IntPtr h);[StructLayout(LayoutKind.Sequential)]public struct P{public int X;public int Y;}[DllImport("user32.dll")]public static extern bool GetCursorPos(out P p);}'
`
const runPs = (name, body) => {
  const f = path.join(path.dirname(OUTF), name); fs.writeFileSync(f, PS_HEAD + body)
  return new Promise((res, rej) => cp.execFile('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', f],
    (e, o, er) => e ? rej(new Error(String(er) + o)) : res(o.trim())))
}
const hit = (hwnd, pts) => runPs('hit.ps1', `$h=[IntPtr]${hwnd}
foreach($a in @(${pts.map(p => `@(${p[0]},${p[1]})`).join(',')})){ $x=[int]$a[0];$y=[int]$a[1]
$l=[IntPtr](($y -shl 16) -bor ($x -band 0xFFFF)); "" + [W]::SendMessage($h,0x84,[IntPtr]::Zero,$l).ToInt64() }`).then(o => o.split(/\r?\n/))
// one real left-button drag: press at (x,y) screen px, move by (dx,dy) in steps, release
const drag = (hwnd, x, y, dx, dy) => runPs('drag.ps1', `$p=New-Object W+P; [void][W]::GetCursorPos([ref]$p)
[void][W]::SetForegroundWindow([IntPtr]${hwnd}); Start-Sleep -Milliseconds 150
[void][W]::SetCursorPos(${x},${y}); Start-Sleep -Milliseconds 120
[W]::mouse_event(2,0,0,0,[IntPtr]::Zero); Start-Sleep -Milliseconds 120
for($i=1;$i -le 10;$i++){ [void][W]::SetCursorPos(${x}+[int](${dx}*$i/10),${y}+[int](${dy}*$i/10)); Start-Sleep -Milliseconds 30 }
Start-Sleep -Milliseconds 100; [W]::mouse_event(4,0,0,0,[IntPtr]::Zero); Start-Sleep -Milliseconds 200
[void][W]::SetCursorPos($p.X,$p.Y); "ok"`)
const sleep = ms => new Promise(r => setTimeout(r, ms))
process.on('unhandledRejection', e => { fs.writeFileSync(OUTF + '.err', String(e && e.stack || e)); app.exit(1) })
app.whenReady().then(() => srv.listen(0, '127.0.0.1', async () => {
  const port = srv.address().port
  const area = screen.getPrimaryDisplay().workArea
  const win = new BrowserWindow({ width: Math.min(1200, area.width - 120), height: Math.min(760, area.height - 80),
    x: area.x + 40, y: area.y + 30, frame: false, show: true, webPreferences: { contextIsolation: true, sandbox: true } })
  const url = `http://127.0.0.1:${port}/?view=attention`
  await win.loadURL(url)
  const pin = { [JSON.stringify(['studio', 'attention-desk'])]: { rect: { x: 300, y: 0, w: 420, h: 360 }, z: 1 } }
  await win.webContents.executeJavaScript(`localStorage.setItem('orgtree-modal-pins', ${JSON.stringify(JSON.stringify(pin))});
    localStorage.setItem('orgtree-modal-open', JSON.stringify([{kind:'attention-desk',org:'studio'}]));
    localStorage.setItem('orgtree-desktop-last-org','studio')`)
  await win.loadURL(url)
  await sleep(3000)
  const out = { steps: [] }
  out.info = await win.webContents.executeJavaScript(`(() => {
    const b = s => { const e = document.querySelector(s); if (!e) return null; const r = e.getBoundingClientRect(); return [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)] }
    const o = {}; for (const s of ['.shell-header', '.canvas-stage', '.window-drag-margin', '.modalpin-win', '.modalpin-resize-frame']) o[s] = b(s)
    const w = document.querySelector('.modalpin-win'); const anc = []
    for (let e = w; e; e = e.parentElement) anc.push(e.tagName + '.' + String(e.className).slice(0, 30) + ':' + getComputedStyle(e).getPropertyValue('-webkit-app-region'))
    o.ancestors = anc
    o.scrollers = w ? [w, ...w.querySelectorAll('*')].filter(e => { const s = getComputedStyle(e); return /(auto|scroll)/.test(s.overflowY) && e.scrollHeight > e.clientHeight }).map(e => e.className + ' ' + e.scrollHeight + '/' + e.clientHeight) : null
    return o })()`)
  if (process.env.SCROLL === '1') {
    // a long desk transcript, scrolled to its newest end (what a real desk shows)
    out.scrolled = await win.webContents.executeJavaScript(`(() => {
      const w = document.querySelector('.modalpin-win'); const box = document.createElement('div')
      for (let i = 0; i < 120; i++) { const d = document.createElement('div'); d.textContent = 'transcript line ' + i; d.style.height = '24px'; box.appendChild(d) }
      w.appendChild(box); w.scrollTop = w.scrollHeight; return [w.scrollTop, w.scrollHeight, w.clientHeight] })()`)
    await sleep(600)
  }
  await win.webContents.capturePage().then(i => fs.writeFileSync(OUTF + '.png', i.toPNG()))
  const hwnd = win.getNativeWindowHandle().readBigUInt64LE(0).toString()
  const map = async (label) => {
    const cb = win.getContentBounds(); const ys = [4, 12, 20, 28, 34]; const pts = []
    const xs = []; for (let x = 10; x < cb.width; x += 30) xs.push(x)
    for (const y of ys) for (const x of xs) { const p = screen.dipToScreenPoint({ x: cb.x + x, y: cb.y + y }); pts.push([p.x, p.y]) }
    const codes = await hit(hwnd, pts)
    const rows = ys.map((y, k) => 'y=' + y + ' ' + codes.slice(k * xs.length, (k + 1) * xs.length).map(c => c === '2' ? 'D' : c === '1' ? '.' : '#').join(''))
    out.steps.push({ label, bounds: win.getBounds(), maximized: win.isMaximized(), map: rows })
  }
  await map('initial')
  if (process.env.NODRAG === '1') {
    win.maximize(); await sleep(1200); await map('maximized')
    win.unmaximize(); await sleep(1200); await map('restored')
  } else {
    // header point above the panel, 100px in from its left edge (CTL=1: the
    // title area at x=70, a spot that drags with or without the fix)
    const target = async () => {
      const r = await win.webContents.executeJavaScript(`(() => { const w = document.querySelector('.modalpin-win').getBoundingClientRect(); const h = document.querySelector('.shell-header').getBoundingClientRect(); return [Math.round(${process.env.CTL === "1"} ? 70 : w.left + 100), Math.round(h.top + h.height / 2)] })()`)
      const cb = win.getContentBounds(); return screen.dipToScreenPoint({ x: cb.x + r[0], y: cb.y + r[1] })
    }
    for (const n of [1, 2]) {
      const before = win.getBounds(); const p = await target()
      await drag(hwnd, p.x, p.y, 60, 40); await sleep(500)
      out.steps.push({ label: 'drag' + n + ' unmaximized', at: p, before, after: win.getBounds(), moved: JSON.stringify(before) !== JSON.stringify(win.getBounds()) })
    }
    win.maximize(); await sleep(1200); await map('maximized')
    for (const n of [3, 4, 5]) {
      const before = win.getBounds(); const wasMax = win.isMaximized(); const p = await target()
      await drag(hwnd, p.x, p.y, 80, 120); await sleep(800)
      out.steps.push({ label: 'drag' + n + (wasMax ? ' from maximized' : ' after restore'), at: p, before, after: win.getBounds(), maximizedAfter: win.isMaximized(), moved: JSON.stringify(before) !== JSON.stringify(win.getBounds()) })
      await map('after drag' + n)
    }
  }
  fs.writeFileSync(OUTF, JSON.stringify(out, null, 1))
  app.quit()
}))

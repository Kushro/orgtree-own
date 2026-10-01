// pinheader_electron_probe.cjs - asks Windows (WM_NCHITTEST) which header pixels drag the window,
// in a real frameless Electron window showing the app on fake data, with the Attention desk pinned.
//   node tests/appchrome-build.mjs <bundle>
//   env: BUNDLE=<bundle> UDATA=<empty dir> OUTF=<result file> PIN=<modal-pins json> [HIDE=<js>]
//   <repo>/node_modules/electron/dist/electron.exe tests/pinheader_electron_probe.cjs
// Result map: D = drag area, . = not, # = window border. Rows are every 2px down the header.
// Never run against the live app: it uses its own UDATA and the fake-data bundle.
const { app, BrowserWindow, screen } = require('electron')
const http = require('http'), fs = require('fs'), path = require('path'), cp = require('child_process')
const BUNDLE = process.env.BUNDLE, PIN = process.env.PIN || '', OUTF = process.env.OUTF
app.setPath('userData', process.env.UDATA)
const types = { '.js': 'text/javascript', '.css': 'text/css', '.html': 'text/html' }
const srv = http.createServer((q, r) => {
  const f = q.url.split('?')[0] === '/' ? '/index.html' : q.url.split('?')[0]
  const p = path.join(BUNDLE, f)
  if (!fs.existsSync(p)) { r.writeHead(404); return r.end() }
  r.writeHead(200, { 'Content-Type': types[path.extname(p)] || 'text/plain' }); r.end(fs.readFileSync(p))
})
const ps = (hwnd, pts) => {
  const arr = pts.map(p => `@(${p[0]},${p[1]})`).join(',')
  const script = `Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class W{[DllImport("user32.dll")]public static extern IntPtr SendMessage(IntPtr h,uint m,IntPtr w,IntPtr l);}'
$h=[IntPtr]${hwnd}
foreach($a in @(${arr})){ $x=[int]$a[0];$y=[int]$a[1]
$l=[IntPtr](($y -shl 16) -bor ($x -band 0xFFFF)); $r=[W]::SendMessage($h,0x84,[IntPtr]::Zero,$l); "$x,$y=" + $r.ToInt64() }`
  const f = path.join(path.dirname(OUTF), 'hit.ps1'); fs.writeFileSync(f, script); return new Promise((res, rej) => cp.execFile('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', f], (e, o, er) => e ? rej(new Error(String(er) + o)) : res(o)))
}
process.on("unhandledRejection", e => { fs.writeFileSync(process.env.OUTF + ".err", String(e && e.stack || e)); app.exit(1) })
app.whenReady().then(() => srv.listen(0, '127.0.0.1', async () => {
  const port = srv.address().port
  const win = new BrowserWindow({ width: 1400, height: 900, x: 100, y: 60, frame: false, show: true,
    webPreferences: { contextIsolation: true, sandbox: true } })
  await win.loadURL(`http://127.0.0.1:${port}/?view=attention`)
  if (PIN) {
    await win.webContents.executeJavaScript(`localStorage.setItem('orgtree-modal-pins', ${JSON.stringify(PIN)}); localStorage.setItem('orgtree-desktop-last-org','studio')`)
    await win.loadURL(`http://127.0.0.1:${port}/?view=attention`)
  }
  await new Promise(r => setTimeout(r, 3000))
  if (process.env.TOGGLE) { win.maximize(); await new Promise(r => setTimeout(r, 1500)); win.unmaximize(); await new Promise(r => setTimeout(r, 2000)) }
  if (process.env.HIDE) { await win.webContents.executeJavaScript(process.env.HIDE); await new Promise(r => setTimeout(r, 500)) }
  const info = await win.webContents.executeJavaScript(`(() => {
    const out = {}; const b = s => { const e = document.querySelector(s); if (!e) return null; const r = e.getBoundingClientRect(); return [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)] }
    for (const s of ['.shell-header', '.shell-header-main', '.canvas-stage', '.window-drag-margin', '.viewport', '.modalpin-win', '.modalpin-resize-frame', '.attn-stage']) out[s] = b(s)
    out.noDrag = [...document.querySelectorAll('*')].filter(e => getComputedStyle(e).getPropertyValue('-webkit-app-region') === 'no-drag').map(e => e.tagName + '.' + String(e.className).slice(0, 40)).slice(0, 5)
    return out })()`)
  await win.webContents.capturePage().then(i => fs.writeFileSync(OUTF + '.png', i.toPNG()))
  const hwnd = win.getNativeWindowHandle().readBigUInt64LE(0).toString()
  const cb = win.getContentBounds()
  const pts = []; const ys = Array.from({ length: 23 }, (_, i) => i * 2)
  const cw = cb.width
  for (const y of ys) for (let x = 4; x < cw; x += 20) { const p = screen.dipToScreenPoint({ x: cb.x + x, y: cb.y + y }); pts.push([p.x, p.y]) }
  const res = await ps(hwnd, pts)
  const codes = res.trim().split(/\r?\n/).map(l => l.split('=')[1]); const n = Math.ceil((cw - 4) / 20); let map = ''; ys.forEach((y, k) => { map += 'y=' + y + ' ' + codes.slice(k * n, (k + 1) * n).map(c => c === '2' ? 'D' : c === '1' ? '.' : '#').join('') + String.fromCharCode(10) }); fs.writeFileSync(OUTF, JSON.stringify({ cb, info }) + String.fromCharCode(10) + map)
  app.quit()
}))

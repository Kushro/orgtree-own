"""Screenshots of the circular layout (real layout() + real NodeSquare cards) in Edge."""
import pathlib, subprocess, sys, tempfile
from playwright.sync_api import sync_playwright
HERE = pathlib.Path(__file__).resolve().parent
out = pathlib.Path(sys.argv[1]); out.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory() as d:
    subprocess.run(['node', str(HERE / 'circular-build.mjs'), d], check=True)
    with sync_playwright() as pw:
        b = pw.chromium.launch(channel='msedge', headless=True)
        for org in ('small', 'large'):
            pg = b.new_page(viewport={'width': 1600, 'height': 1000})
            errs = []; pg.on('pageerror', lambda e: errs.append(str(e)))
            pg.goto((pathlib.Path(d) / 'probe.html').as_uri() + f'?org={org}')
            pg.wait_for_selector('#root > div')
            n = pg.evaluate('window.count')
            path = out / f'circular-{org}.png'
            pg.locator('#root > div').screenshot(path=str(path))
            print(org, n, 'agents+eye', path, errs)
        b.close()

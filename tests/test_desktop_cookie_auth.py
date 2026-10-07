"""TokenGate: desktop token as header or HttpOnly cookie, and the WS Origin check.

The Tauri shell cannot add headers to a WebView2 WebSocket handshake, so it
stores the per-boot token as an HttpOnly cookie on the engine origin. Cookies
are scoped by host, not port, so the gate trusts a cookie only with proof the
request comes from the engine's own page. The header path (Electron, tools)
keeps working unchanged. A minimal ASGI app stands in for the API, so these
controls exercise only the transport gate.
"""
import inspect
import unittest

import import_provenance  # noqa: F401  asserts orgtree resolves inside this checkout
from engine.launch import DESKTOP_COOKIE, TokenGate
from fastapi import FastAPI, WebSocket
from fastapi.testclient import TestClient
from starlette.websockets import WebSocketDisconnect

TOKEN = 'cookie-test-token'
ORIGIN = 'http://127.0.0.1:24680'
FOREIGN = 'http://127.0.0.1:8080'

inner = FastAPI()


@inner.get('/api/ping')
def ping() -> dict[str, bool]:
    return {'ok': True}


@inner.websocket('/ws')
async def ws(socket: WebSocket) -> None:
    await socket.accept()
    await socket.send_text('hello')
    await socket.close()


def cookie(value: str = TOKEN) -> str:
    return f'{DESKTOP_COOKIE}={value}'


class CookieHttp(unittest.TestCase):
    def setUp(self):
        self.client = TestClient(TokenGate(inner, TOKEN, origin=ORIGIN))

    def get(self, **headers):
        return self.client.get('/api/ping', headers=headers).status_code

    def test_header_still_works(self):
        self.assertEqual(self.get(**{'X-Orgtree-Desktop-Token': TOKEN}), 200)
        self.assertEqual(self.get(**{'X-Orgtree-Desktop-Token': 'wrong'}), 401)
        self.assertEqual(self.get(), 401)

    def test_valid_cookie_from_the_engine_page(self):
        self.assertEqual(self.get(Cookie=cookie(), **{'Sec-Fetch-Site': 'same-origin', 'Origin': ORIGIN}), 200)
        # a top-level navigation the shell itself starts
        self.assertEqual(self.get(Cookie=cookie(), **{'Sec-Fetch-Site': 'none'}), 200)

    def test_invalid_cookie(self):
        self.assertEqual(self.get(Cookie=cookie('wrong'), **{'Sec-Fetch-Site': 'same-origin'}), 401)
        self.assertEqual(self.get(Cookie=cookie(''), **{'Sec-Fetch-Site': 'same-origin'}), 401)
        self.assertEqual(self.get(Cookie=f'other={TOKEN}', **{'Sec-Fetch-Site': 'same-origin'}), 401)

    def test_cookie_from_another_page_is_refused(self):
        # another local server's page shares the host, and so the cookie jar
        self.assertEqual(self.get(Cookie=cookie(), Origin=FOREIGN), 401)
        for site in ('same-site', 'cross-site'):
            self.assertEqual(self.get(Cookie=cookie(), **{'Sec-Fetch-Site': site}), 401, site)
        # a sandboxed agent iframe has an opaque origin
        self.assertEqual(self.get(Cookie=cookie(), Origin='null', **{'Sec-Fetch-Site': 'cross-site'}), 401)

    def test_a_shadowing_cookie_does_not_lock_out_the_real_one(self):
        # a page on another port can set a same-name cookie for the host
        self.assertEqual(self.get(Cookie=f'{cookie("planted")}; theme=dark; {cookie()}',
                                  **{'Sec-Fetch-Site': 'same-origin'}), 200)

    def test_no_cookie_auth_without_a_configured_origin(self):
        client = TestClient(TokenGate(inner, TOKEN))
        self.assertEqual(client.get('/api/ping', headers={'Cookie': cookie(), 'Sec-Fetch-Site': 'same-origin'}).status_code, 401)
        self.assertEqual(client.get('/api/ping', headers={'X-Orgtree-Desktop-Token': TOKEN}).status_code, 200)

    def test_tokens_compare_in_constant_time(self):
        source = inspect.getsource(TokenGate)
        self.assertIn('hmac.compare_digest', source)
        self.assertNotIn('!= self.token', source)


class CookieWebSocket(unittest.TestCase):
    def setUp(self):
        self.client = TestClient(TokenGate(inner, TOKEN, origin=ORIGIN))

    def connect(self, **headers) -> str:
        with self.client.websocket_connect('/ws', headers=headers) as socket:
            return socket.receive_text()

    def refused(self, **headers) -> int:
        with self.assertRaises(WebSocketDisconnect) as caught:
            self.connect(**headers)
        return caught.exception.code

    def test_cookie_with_the_engine_origin(self):
        self.assertEqual(self.connect(Cookie=cookie(), Origin=ORIGIN), 'hello')

    def test_cookie_with_a_foreign_origin(self):
        self.assertEqual(self.refused(Cookie=cookie(), Origin=FOREIGN), 4401)
        self.assertEqual(self.refused(Cookie=cookie(), Origin='null'), 4401)

    def test_cookie_without_origin(self):
        # browsers always send Origin on a WebSocket; a cookie alone proves nothing
        self.assertEqual(self.refused(Cookie=cookie()), 4401)

    def test_invalid_cookie(self):
        self.assertEqual(self.refused(Cookie=cookie('wrong'), Origin=ORIGIN), 4401)

    def test_header_keeps_working_and_foreign_origin_is_refused(self):
        self.assertEqual(self.connect(**{'X-Orgtree-Desktop-Token': TOKEN}), 'hello')
        self.assertEqual(self.connect(**{'X-Orgtree-Desktop-Token': TOKEN, 'Origin': ORIGIN}), 'hello')
        self.assertEqual(self.refused(**{'X-Orgtree-Desktop-Token': TOKEN, 'Origin': FOREIGN}), 4401)
        self.assertEqual(self.refused(), 4401)


if __name__ == '__main__':
    unittest.main()

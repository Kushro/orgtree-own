"""El feed del updater de Tauri (#23): arma y verifica `latest.json`.

`tauri-plugin-updater` lee un `latest.json` con la versión, la URL del
instalador NSIS y su firma minisign (la que escribe `tauri signer sign`, en
base64). La app solo instala lo que verifica con la clave pública que lleva
compilada, y la firma tiene que nombrar la misma versión que anuncia el feed
(`requireSignedVersion`).

- `make`: arma el feed a partir del instalador, su `.sig`, la versión y la URL.
- `verify`: verifica, como lo hace el plugin, que la firma del feed es la del
  instalador con esa clave pública y que nombra la versión anunciada. Sale con
  1 y el motivo si algo no cierra. Es Ed25519 en Python puro (RFC 8032), así
  que corre en cualquier runner sin instalar nada.

Uso:
  python updater-feed.py make --installer X.exe --signature X.exe.sig \\
      --version 0.1.42 --url https://.../X.exe --out latest.json
  python updater-feed.py verify --feed latest.json --installer X.exe \\
      --pubkey <clave pública en base64, o un archivo .pub>
"""
from __future__ import annotations

import argparse
import base64
import datetime
import hashlib
import json
from pathlib import Path
import re
import sys

# Las claves de plataforma que busca el plugin en Windows x64, en su orden.
PLATFORMS = ("windows-x86_64-nsis", "windows-x86_64")
SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$")


# ------------------------------------------------------------ Ed25519 (RFC 8032)

_P = 2**255 - 19
_Q = 2**252 + 27742317777372353535851937790883648493
_D = -121665 * pow(121666, _P - 2, _P) % _P
_SQRT_M1 = pow(2, (_P - 1) // 4, _P)


def _add(a, b):
    x = (a[1] - a[0]) * (b[1] - b[0]) % _P
    y = (a[1] + a[0]) * (b[1] + b[0]) % _P
    c = 2 * a[3] * b[3] * _D % _P
    d = 2 * a[2] * b[2] % _P
    e, f, g, h = y - x, d - c, d + c, y + x
    return (e * f, g * h, f * g, e * h)


def _mul(s, point):
    result = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            result = _add(result, point)
        point = _add(point, point)
        s >>= 1
    return result


def _equal(a, b):
    return (a[0] * b[2] - b[0] * a[2]) % _P == 0 and (a[1] * b[2] - b[1] * a[2]) % _P == 0


def _recover_x(y, sign):
    if y >= _P:
        return None
    x2 = (y * y - 1) * pow(_D * y * y + 1, _P - 2, _P)
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P:
        x = x * _SQRT_M1 % _P
    if (x * x - x2) % _P:
        return None
    if (x & 1) != sign:
        x = _P - x
    return x


def _decompress(data: bytes):
    y = int.from_bytes(data, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    x = _recover_x(y, sign)
    return None if x is None else (x, y, 1, x * y % _P)


_GY = 4 * pow(5, _P - 2, _P) % _P
_G = (_recover_x(_GY, 0), _GY, 1, _recover_x(_GY, 0) * _GY % _P)


def ed25519_verify(public: bytes, message: bytes, signature: bytes) -> bool:
    if len(public) != 32 or len(signature) != 64:
        return False
    a = _decompress(public)
    r = _decompress(signature[:32])
    s = int.from_bytes(signature[32:], "little")
    if a is None or r is None or s >= _Q:
        return False
    h = int.from_bytes(hashlib.sha512(signature[:32] + public + message).digest(), "little") % _Q
    return _equal(_mul(s, _G), _add(r, _mul(h, a)))


# ------------------------------------------------------------------ minisign

class FeedError(Exception):
    pass


def _b64_text(value: str, what: str) -> str:
    try:
        return base64.b64decode(value.strip(), validate=True).decode("utf-8")
    except Exception as exc:  # noqa: BLE001
        raise FeedError(f"{what}: no es base64 de texto ({exc})") from exc


def public_key(value: str) -> tuple[bytes, bytes]:
    """(keynum, clave) de una clave pública de `tauri signer` (base64 del .pub)."""
    lines = [line for line in _b64_text(value, "clave pública").splitlines() if line.strip()]
    if len(lines) < 2:
        raise FeedError("clave pública: faltan líneas")
    raw = base64.b64decode(lines[1])
    if len(raw) != 42 or raw[:2] != b"Ed":
        raise FeedError("clave pública: no es una clave minisign Ed25519")
    return raw[2:10], raw[10:]


def verify_signature(data: bytes, signature: str, pubkey: str) -> str:
    """Verifica la firma de `tauri signer sign` y devuelve el comentario de confianza."""
    keynum, key = public_key(pubkey)
    lines = [line for line in _b64_text(signature, "firma").splitlines() if line.strip()]
    if len(lines) < 4 or not lines[2].startswith("trusted comment: "):
        raise FeedError("firma: formato minisign inesperado")
    raw = base64.b64decode(lines[1])
    if len(raw) != 74 or raw[:2] not in (b"Ed", b"ED"):
        raise FeedError("firma: algoritmo desconocido")
    if raw[2:10] != keynum:
        raise FeedError("firma: la hizo otra clave (keynum distinto)")
    message = hashlib.blake2b(data, digest_size=64).digest() if raw[:2] == b"ED" else data
    if not ed25519_verify(key, message, raw[10:]):
        raise FeedError("firma: no corresponde al instalador")
    trusted = lines[2][len("trusted comment: "):]
    if not ed25519_verify(key, raw[10:] + trusted.encode("utf-8"), base64.b64decode(lines[3])):
        raise FeedError("firma: el comentario de confianza fue alterado")
    return trusted


def signed_version(trusted: str) -> str | None:
    for part in trusted.split("\t"):
        if part.startswith("version:"):
            return part[len("version:"):]
    return None


# --------------------------------------------------------------------- feed

def make(args: argparse.Namespace) -> int:
    if not SEMVER.match(args.version):
        raise FeedError(f"versión no semver: {args.version}")
    signature = Path(args.signature).read_text(encoding="utf-8").strip()
    entry = {"url": args.url, "signature": signature}
    feed = {
        "version": args.version,
        "notes": args.notes,
        "pub_date": datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "platforms": {name: entry for name in PLATFORMS},
    }
    Path(args.out).write_text(json.dumps(feed, indent=2) + "\n", encoding="utf-8")
    print(f"feed {args.out}: {args.version} -> {args.url}")
    return 0


def verify(args: argparse.Namespace) -> int:
    pubkey = args.pubkey
    if Path(pubkey).is_file():
        pubkey = Path(pubkey).read_text(encoding="utf-8").strip()
    feed = json.loads(Path(args.feed).read_text(encoding="utf-8"))
    version = feed.get("version")
    if not isinstance(version, str) or not SEMVER.match(version.lstrip("v")):
        raise FeedError(f"feed: versión inválida {version!r}")
    if args.expect_version and version != args.expect_version:
        raise FeedError(f"feed: anuncia {version}, se esperaba {args.expect_version}")
    platforms = feed.get("platforms") or {}
    entry = next((platforms[name] for name in PLATFORMS if name in platforms), None)
    if not entry or not entry.get("url") or not entry.get("signature"):
        raise FeedError(f"feed: falta la plataforma ({' o '.join(PLATFORMS)})")
    trusted = verify_signature(Path(args.installer).read_bytes(), entry["signature"], pubkey)
    if signed_version(trusted) != version.lstrip("v"):
        raise FeedError(f"feed: la firma es de la versión {signed_version(trusted)!r} y el feed anuncia {version}")
    print(f"feed {args.feed}: {version}, firma válida ({trusted})")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    m = sub.add_parser("make")
    m.add_argument("--installer", required=True)
    m.add_argument("--signature", required=True)
    m.add_argument("--version", required=True)
    m.add_argument("--url", required=True)
    m.add_argument("--notes", default="")
    m.add_argument("--out", required=True)
    v = sub.add_parser("verify")
    v.add_argument("--feed", required=True)
    v.add_argument("--installer", required=True)
    v.add_argument("--pubkey", required=True)
    v.add_argument("--expect-version")
    args = parser.parse_args()
    try:
        return make(args) if args.command == "make" else verify(args)
    except (FeedError, OSError, ValueError) as exc:
        print(f"updater-feed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""VPN servers for testing RyzikOS's VPN in QEMU, with a real Xray.

Starts Xray with one inbound per protocol RyzikOS speaks, a TLS 1.3
site for REALITY to borrow, and a web server on port 8124 that serves
the subscription (/sub, base64 links with profile-title and
subscription-userinfo headers, as panels send them) and small pages
(/probe for the delay test, /page.html). Inside QEMU the host is
10.0.2.2, and through the VPN the pages are at 127.0.0.1:8124.

    python3 scripts/vpn-test-server.py /path/to/xray

Runs until killed. Prints "ready" when everything listens.
"""

import base64
import http.server
import json
import os
import ssl
import subprocess
import sys
import tempfile
import threading

XRAY = sys.argv[1] if len(sys.argv) > 1 else "xray"
HOST = "10.0.2.2"
UUID = "eb4061f5-b228-4351-abdd-63f002d617b4"
PASSWORD = "ryzikos-test"

d = tempfile.mkdtemp()
cert, key = os.path.join(d, "cert.pem"), os.path.join(d, "key.pem")
subprocess.run(
    ["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
     "-nodes", "-keyout", key, "-out", cert, "-days", "30", "-subj", "/CN=test.local"],
    check=True, capture_output=True)
keys = subprocess.run([XRAY, "x25519"], check=True, capture_output=True, text=True).stdout
private = keys.split("PrivateKey:")[1].split()[0]
public = keys.split("PublicKey):")[1].split()[0] if "PublicKey):" in keys else keys.split("Public key:")[1].split()[0]
SID = "6ba85179e30d4fc2"

tls = {"certificates": [{"certificateFile": cert, "keyFile": key}]}
reality = {"show": False, "target": "127.0.0.1:8443", "serverNames": ["test.local"],
           "privateKey": private, "shortIds": ["", SID]}


def vless(port, flow, security, settings):
    return {"port": port, "protocol": "vless",
            "settings": {"clients": [{"id": UUID, "flow": flow}], "decryption": "none"},
            "streamSettings": {"network": "tcp", "security": security,
                               security + "Settings" if security != "none" else "x": settings}}


inbounds = [
    vless(4431, "xtls-rprx-vision", "reality", reality),
    vless(4432, "", "tls", tls),
    vless(4433, "xtls-rprx-vision", "tls", tls),
    {"port": 4434, "protocol": "trojan", "settings": {"clients": [{"password": PASSWORD}]},
     "streamSettings": {"network": "tcp", "security": "tls", "tlsSettings": tls}},
    {"port": 4435, "protocol": "shadowsocks",
     "settings": {"method": "chacha20-ietf-poly1305", "password": PASSWORD, "network": "tcp"}},
    {"port": 4436, "protocol": "shadowsocks",
     "settings": {"method": "aes-256-gcm", "password": PASSWORD, "network": "tcp"}},
    vless(4437, "", "reality", reality),
]
# UPSTREAM=host:port sends what leaves Xray through an HTTP proxy
# (for sandboxes whose only way out is one)
outbound = {"protocol": "freedom"}
if os.environ.get("UPSTREAM"):
    up_host, up_port = os.environ["UPSTREAM"].rsplit(":", 1)
    outbound = {"protocol": "http", "settings": {"servers": [{"address": up_host, "port": int(up_port)}]}}
config = {"log": {"loglevel": os.environ.get("XRAY_LOG", "warning")}, "inbounds": inbounds,
          "outbounds": [outbound],
          # the test pages stay local
          "routing": {"rules": [{"type": "field", "ip": ["127.0.0.1"], "outboundTag": "direct"}]}}
config["outbounds"][0]["tag"] = "out"
config["outbounds"].append({"protocol": "freedom", "tag": "direct"})
with open(os.path.join(d, "config.json"), "w") as f:
    json.dump(config, f)

links = [
    f"vless://{UUID}@{HOST}:4431?type=tcp&security=reality&pbk={public}&fp=chrome&sni=test.local&sid={SID}&flow=xtls-rprx-vision#Reality%20Vision",
    f"vless://{UUID}@{HOST}:4432?type=tcp&security=tls&sni=test.local#VLESS%20TLS",
    f"vless://{UUID}@{HOST}:4433?type=tcp&security=tls&sni=test.local&flow=xtls-rprx-vision#VLESS%20TLS%20Vision",
    f"trojan://{PASSWORD}@{HOST}:4434?security=tls&sni=test.local#Trojan",
    "ss://" + base64.urlsafe_b64encode(f"chacha20-ietf-poly1305:{PASSWORD}".encode()).decode().rstrip("=") + f"@{HOST}:4435#SS%20ChaCha",
    "ss://" + base64.b64encode(f"aes-256-gcm:{PASSWORD}@{HOST}:4436".encode()).decode() + "#SS%20AES",
    f"vless://{UUID}@{HOST}:4437?type=tcp&security=reality&pbk={public}&sni=test.local&sid=#Reality%20no%20flow",
    f"vless://{UUID}@{HOST}:4438?type=ws&security=tls#WebSocket",
]
SUB = base64.b64encode("\n".join(links).encode())
PAGE = b"<html><head><title>Through the VPN</title></head><body><h1>VPN works</h1>" + b"x" * 60000 + b"</body></html>"


class Web(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/sub":
            body, extra = SUB, {"profile-title": "base64:" + base64.b64encode("Тест VPN".encode()).decode(),
                                "subscription-userinfo": "upload=1073741824; download=2147483648; total=107374182400; expire=1790000000"}
        elif self.path == "/probe":
            self.send_response(204)
            self.end_headers()
            return
        else:
            body, extra = PAGE, {}
        self.send_response(200)
        self.send_header("Content-Type", "text/html" if body is PAGE else "text/plain")
        self.send_header("Content-Length", str(len(body)))
        for k, v in extra.items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


class Site(http.server.BaseHTTPRequestHandler):
    """The site REALITY borrows; the big page tests TLS inside the tunnel."""

    def do_GET(self):
        body = PAGE if self.path == "/page.html" else b"real site"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(cert, key)


class TlsServer(http.server.ThreadingHTTPServer):
    """TLS with the handshake on the connection's own thread, so a
    client that stops halfway doesn't hold up the others."""

    def get_request(self):
        sock, addr = self.socket.accept()
        return ctx.wrap_socket(sock, server_side=True, do_handshake_on_connect=False), addr

    def handle_error(self, request, client_address):
        pass


web = http.server.ThreadingHTTPServer(("0.0.0.0", 8124), Web)
site = TlsServer(("127.0.0.1", 8443), Site)
for s in (web, site):
    threading.Thread(target=s.serve_forever, daemon=True).start()
xray = subprocess.Popen([XRAY, "run", "-c", os.path.join(d, "config.json")])
print("private key:", private)
print("links:", *links, sep="\n  ")
print("ready", flush=True)
try:
    xray.wait()
finally:
    xray.kill()

#!/usr/bin/env python3
"""Serve SDK/out over the LAN so the phone can fetch a .sis directly.

Two things a plain `python3 -m http.server` gets wrong for this job:

  - MIME type. Served as application/octet-stream, the S60 browser saves the file
    as an unknown blob and you have to hunt for it in the file manager. Served as
    application/vnd.symbian.install, the browser hands it straight to the
    installer.

  - The index page. The E72's browser is from 2009: no flexbox, no viewport meta
    worth trusting, and a narrow 320px screen. Big plain links, nothing else.

Binds to 0.0.0.0, so anything on the LAN can read this folder while it runs.
Stop it with Ctrl-C or by killing the process.
"""

import http.server
import os
import socket
import socketserver
import sys

PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8000
ROOT = sys.argv[1] if len(sys.argv) > 1 else "."

SYMBIAN_TYPES = {
    ".sis": "application/vnd.symbian.install",
    ".sisx": "x-epoc/x-sisx-app",
    ".exe": "application/octet-stream",
    ".elf": "application/octet-stream",
}


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=ROOT, **kw)

    def guess_type(self, path):
        ext = os.path.splitext(path)[1].lower()
        if ext in SYMBIAN_TYPES:
            return SYMBIAN_TYPES[ext]
        return super().guess_type(path)

    def list_directory(self, path):
        try:
            names = sorted(os.listdir(path))
        except OSError:
            self.send_error(404, "No permission to list directory")
            return None

        rows = []
        for name in names:
            full = os.path.join(path, name)
            if os.path.isdir(full):
                continue
            size = os.path.getsize(full)
            kb = (size + 1023) // 1024
            rows.append(
                f'<p><a href="{name}">{name}</a><br>'
                f'<small>{kb} KB</small></p><hr>'
            )

        body = (
            "<html><head><title>Rust Symbian SDK</title>"
            '<meta http-equiv="Content-Type" content="text/html; charset=utf-8">'
            "</head><body>"
            "<h2>Rust Symbian SDK</h2>"
            + "".join(rows)
            + "</body></html>"
        ).encode("utf-8")

        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        import io

        return io.BytesIO(body)

    def log_message(self, fmt, *args):
        # One line per request, so a failed download from the phone is visible.
        sys.stderr.write(f"{self.client_address[0]} {fmt % args}\n")
        sys.stderr.flush()


def lan_ip():
    """The address a device on the LAN would reach us on."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("192.168.255.255", 1))
        return s.getsockname()[0]
    except OSError:
        return "127.0.0.1"
    finally:
        s.close()


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    root = os.path.abspath(ROOT)
    with Server(("0.0.0.0", PORT), Handler) as httpd:
        print(f"serving {root}")
        print(f"  http://{lan_ip()}:{PORT}/")
        sys.stdout.flush()
        httpd.serve_forever()

#!/usr/bin/env python3
"""A TCP echo server, so the phone's first socket has something certain to talk to.

    python3 tools/echo.py [port]        # default 7654

This is the gate for the whole network layer. Screen 1 of examples/netprobe connects
here and nothing after it is worth attempting until bytes go out and come back, because
everything downstream assumes a working socket.

It is deliberately the least interesting server possible. No DNS, no TLS, no protocol —
the phone resolves nothing and negotiates nothing, so a failure has exactly one place to
be. The next screen adds DNS, the one after that adds the internet, and each adds one
unknown rather than several.

What it prints is chosen for reading from across the room while holding a phone:

    18:42:03  connect 192.168.1.10:49213
    18:42:03    recv 12  "hello from E7"
    18:42:03    sent 12
    18:42:09  close  192.168.1.10:49213  (12 bytes each way)

The greeting is the other half of a real test. A server that only echoes proves the
phone can send *and then* receive; one that speaks first also proves the client issues a
read before it has anything to say — which is a separate bug, and the reason
TcpStream::on_event issues a read the moment it is connected.
"""

import socket
import socketserver
import sys
import threading
import time

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 7654

# Sent the instant a client connects, before it says anything.
GREETING = b"symbian-echo ready\n"


def stamp():
    return time.strftime("%H:%M:%S")


def printable(data, limit=24):
    """Bytes as a short readable string, with non-printables as dots.

    Truncated because a probe sends short strings and a stray megabyte would scroll the
    interesting lines away.
    """
    text = "".join(chr(b) if 0x20 <= b < 0x7F else "." for b in data[:limit])
    return text + ("…" if len(data) > limit else "")


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        peer = f"{self.client_address[0]}:{self.client_address[1]}"
        print(f"{stamp()}  connect {peer}", flush=True)

        rx = tx = 0
        try:
            self.request.sendall(GREETING)
            tx += len(GREETING)
            print(f"{stamp()}    sent {len(GREETING)}  greeting", flush=True)

            while True:
                # Small reads on purpose: it makes the server hand the phone data in
                # several pieces, which is exactly the case a client that treats one
                # completion as a whole message gets wrong.
                data = self.request.recv(64)
                if not data:
                    break
                rx += len(data)
                print(f'{stamp()}    recv {len(data)}  "{printable(data)}"', flush=True)
                self.request.sendall(data)
                tx += len(data)
                print(f"{stamp()}    sent {len(data)}", flush=True)
        except (ConnectionResetError, BrokenPipeError) as e:
            # A phone that loses its bearer resets rather than closing, and that is not
            # a server fault — worth naming so it is not read as one.
            print(f"{stamp()}  reset  {peer}  ({e})", flush=True)
        finally:
            print(f"{stamp()}  close  {peer}  ({rx} in, {tx} out)", flush=True)


class Server(socketserver.ThreadingTCPServer):
    # Otherwise a restart within the TIME_WAIT window fails to bind, which during a
    # debugging session is every restart.
    allow_reuse_address = True
    daemon_threads = True


def lan_addresses():
    """The addresses a phone could plausibly reach, so the port is not the only thing
    printed. Guessing wrong here is a whole debugging session."""
    out = []
    try:
        # Connecting a UDP socket sends nothing but picks the source address the routing
        # table would use — which is the one to type into the phone.
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("8.8.8.8", 53))
        out.append(s.getsockname()[0])
        s.close()
    except OSError:
        pass
    return out


def main():
    with Server(("0.0.0.0", PORT), Echo) as srv:
        addrs = lan_addresses()
        print(f"echo listening on 0.0.0.0:{PORT}")
        for a in addrs:
            print(f"  reachable at {a}:{PORT}  <- this is the address the phone needs")
        print("  Ctrl-C to stop\n")
        thread = threading.Thread(target=srv.serve_forever, daemon=True)
        thread.start()
        try:
            while True:
                time.sleep(0.5)
        except KeyboardInterrupt:
            print("\nstopping")


if __name__ == "__main__":
    main()

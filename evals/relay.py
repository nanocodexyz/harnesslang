import socket
import socketserver
import sys
import threading

SPLICE_CHUNK = 65536


def pump(source, sink):
    try:
        while True:
            data = source.recv(SPLICE_CHUNK)
            if not data:
                break
            sink.sendall(data)
    except OSError:
        pass
    finally:
        try:
            sink.shutdown(socket.SHUT_WR)
        except OSError:
            pass


class Relay(socketserver.BaseRequestHandler):
    upstream = ("", 0)

    def handle(self):
        try:
            outbound = socket.create_connection(self.upstream)
        except OSError:
            return
        back = threading.Thread(target=pump, args=(outbound, self.request), daemon=True)
        back.start()
        pump(self.request, outbound)
        back.join()
        outbound.close()


class Server(socketserver.ThreadingMixIn, socketserver.TCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    if len(sys.argv) != 4:
        raise SystemExit("usage: relay.py ADDRESS PORT UPSTREAM_HOST:PORT")
    host, port = sys.argv[3].rsplit(":", 1)
    Relay.upstream = (host, int(port))
    Server((sys.argv[1], int(sys.argv[2])), Relay).serve_forever()


main()

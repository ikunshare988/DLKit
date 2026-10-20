#!/usr/bin/env python3
"""DLKit 基准测试用的本地服务器。

可以精确控制：单连接限速、首字节延迟、文件大小、ETag/Range。
用来把「下载速度」拆成可复现的数字，而不是拿真实站点碰运气。

用法：
    python tools/bench_server.py --port 8899 --rate-mbps 4 --latency-ms 20

    GET /file.bin?size=125829120   按指定大小返回文件（默认 100MB）
    GET /page                      一个含 40 个下载链接的页面（测解析速度）
    GET /bump                      让 ETag 变化（测「链接跟踪」）
"""

import argparse
import hashlib
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

BLOCK = 64 * 1024


class State:
    def __init__(self):
        self.generation = 1
        self.lock = threading.Lock()

    def bump(self):
        with self.lock:
            self.generation += 1
            return self.generation


STATE = State()
ARGS = None

# 固定内容：所有请求共用，保证多连接拼接后校验值一致
PAYLOAD = hashlib.sha256(b"dlkit-bench").digest() * (BLOCK // 32)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        if ARGS.verbose:
            super().log_message(fmt, *args)

    def _size(self, query):
        try:
            n = int(query.get("size", [0])[0])
            return n if n > 0 else ARGS.size_mb * 1024 * 1024
        except ValueError:
            return ARGS.size_mb * 1024 * 1024

    def _etag(self, size):
        return '"g%d-%d"' % (STATE.generation, size)

    def _common(self, size, status=200, extra=None):
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(size))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("ETag", self._etag(size))
        self.send_header("Last-Modified", time.strftime("%a, %d %b %Y %H:%M:%S GMT", time.gmtime()))
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()

    def do_HEAD(self):
        time.sleep(ARGS.latency_ms / 1000.0)
        q = parse_qs(urlparse(self.path).query)
        if urlparse(self.path).path == "/bump":
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        size = self._size(q)
        if self.headers.get("If-None-Match") == self._etag(size):
            self.send_response(304)
            self.send_header("ETag", self._etag(size))
            self.end_headers()
            return
        self._common(size)

    def do_GET(self):
        path = urlparse(self.path).path
        time.sleep(ARGS.latency_ms / 1000.0)
        if path == "/bump":
            gen = STATE.bump()
            body = ("generation=%d" % gen).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if path == "/page":
            self._page()
            return

        q = parse_qs(urlparse(self.path).query)
        size = self._size(q)
        if self.headers.get("If-None-Match") == self._etag(size):
            self.send_response(304)
            self.send_header("ETag", self._etag(size))
            self.end_headers()
            return

        start, end = 0, size - 1
        rng = self.headers.get("Range")
        if rng and rng.startswith("bytes="):
            try:
                a, b = rng[6:].split("-", 1)
                start = int(a) if a else 0
                end = int(b) if b else size - 1
                end = min(end, size - 1)
            except ValueError:
                pass
            self._common(end - start + 1, 206,
                         {"Content-Range": "bytes %d-%d/%d" % (start, end, size)})
        else:
            self._common(size)

        remaining = end - start + 1
        sent = 0
        began = time.time()
        while remaining > 0:
            n = min(BLOCK, remaining)
            try:
                self.wfile.write(PAYLOAD[:n])
            except (BrokenPipeError, ConnectionResetError):
                return
            sent += n
            remaining -= n
            if ARGS.rate_mbps > 0:
                due = sent / (ARGS.rate_mbps * 1024 * 1024)
                delta = due - (time.time() - began)
                if delta > 0:
                    time.sleep(delta)

    def _page(self):
        rows = []
        for i in range(40):
            size = (1 + i % 8) * 1024 * 1024
            rows.append(
                '<li><a href="/file%d.bin?size=%d">Release %d (build %d)</a></li>'
                % (i, size, i, 100 + i)
            )
        body = (
            "<html><head><meta charset='utf-8'><title>Bench downloads</title></head>"
            "<body><h1>Bench downloads</h1><ul>%s</ul></body></html>" % "".join(rows)
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    global ARGS
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, default=8899)
    p.add_argument("--size-mb", type=int, default=100)
    p.add_argument("--rate-mbps", type=float, default=0,
                   help="单连接限速（MB/s），0 表示不限")
    p.add_argument("--latency-ms", type=int, default=0, help="每个请求的首字节延迟")
    p.add_argument("--verbose", action="store_true")
    ARGS = p.parse_args()
    srv = ThreadingHTTPServer(("127.0.0.1", ARGS.port), Handler)
    srv.daemon_threads = True
    print("bench server on %d (rate=%s MB/s, latency=%dms)"
          % (ARGS.port, ARGS.rate_mbps, ARGS.latency_ms), flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()

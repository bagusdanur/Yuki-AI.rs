#!/usr/bin/env python3
"""Mock VOICEVOX engine (Fase 6): /version, /speakers, /audio_query,
/synthesis. Mencatat query terakhir ke $VV_LOG. Audio synthesis
deterministik dari skala query: WAV:speed,pitch,int,speaker."""
import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs

LOG = os.environ.get("VV_LOG", "/tmp/vv-log.json")


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _json(self, code, obj):
        body = obj if isinstance(obj, (bytes, str)) else json.dumps(obj)
        if isinstance(body, str):
            body = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        u = urlparse(self.path)
        if u.path == "/version":
            # VOICEVOX asli mengembalikan string JSON (dengan kutip).
            return self._json(200, b'"0.25.0"')
        if u.path == "/speakers":
            return self._json(200, [{
                "name": "Yuki-chan",
                "styles": [{"id": 14, "name": "Normal"}, {"id": 15, "name": "Amaama"}],
            }])
        return self._json(404, {"detail": "not found"})

    def do_POST(self):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        if u.path == "/audio_query":
            text = (q.get("text") or [""])[0]
            speaker = (q.get("speaker") or [""])[0]
            with open(LOG, "w") as f:
                json.dump({"text": text, "speaker": speaker}, f)
            return self._json(200, {
                "accentPhrases": [],
                "intonationScale": 1,
                "kana": "",
                "outputSamplingRate": 24000,
                "outputStereo": False,
                "pitchScale": 0,
                "postPhonemeLength": 0.1,
                "prePhonemeLength": 0.1,
                "speedScale": 1,
                "volumeScale": 1,
            })
        if u.path == "/synthesis":
            speaker = (q.get("speaker") or [""])[0]
            query = json.loads(raw or b"{}")
            body = "WAV:speed={} pitch={} int={} speaker={}".format(
                query.get("speedScale"), query.get("pitchScale"),
                query.get("intonationScale"), speaker)
            data = body.encode()
            self.send_response(200)
            self.send_header("Content-Type", "audio/wav")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        return self._json(404, {"detail": "not found"})


if __name__ == "__main__":
    HTTPServer(("127.0.0.1", 50021), H).serve_forever()

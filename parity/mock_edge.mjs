// Mock server Edge TTS (Fase 6): meniru protokol WebSocket
// speech.platform.bing.com secukupnya untuk msedge-tts asli DAN klien
// Rust: terima speech.config, terima ssml, balas 2 frame audio biner
// (format Edge asli: 2 byte panjang header BE + header + payload)
// lalu text frame turn.end. Setiap SSML dicatat ke $MOCK_EDGE_LOG.
import { createRequire } from 'node:module'
import fs from 'node:fs'

const require = createRequire('/home/hatch/workspace/yuki-ai-rust/original/package.json')
const { WebSocketServer } = require('ws')

const LOG = process.env.MOCK_EDGE_LOG || '/tmp/edge-log.jsonl'
const CHUNK1 = Buffer.from('FAKE-MP3-CHUNK-1-')
const CHUNK2 = Buffer.from('CHUNK-2')

const wss = new WebSocketServer({ port: 50031 })
wss.on('connection', (ws) => {
  ws.on('message', (data, isBinary) => {
    if (isBinary) return
    const text = data.toString()
    if (text.includes('Path:speech.config')) {
      const m = text.match(/"outputFormat":\s*"([^"]+)"/)
      fs.appendFileSync(LOG, JSON.stringify({ type: 'config', outputFormat: m?.[1] || null }) + '\n')
      return
    }
    if (text.includes('Path:ssml')) {
      const rid = /X-RequestId:([0-9a-f]+)/.exec(text)?.[1]
      const voice = /<voice name="([^"]+)"/.exec(text)?.[1]
      const prosody = /<prosody pitch="([^"]+)" rate="([^"]+)" volume="([^"]+)"/.exec(text)
      const input = /<prosody[^>]*>\s*([\s\S]*?)\s*<\/prosody>/.exec(text)?.[1]?.trim()
      fs.appendFileSync(LOG, JSON.stringify({
        type: 'ssml', requestId: rid, voice,
        pitch: prosody?.[1], rate: prosody?.[2], volume: prosody?.[3], input
      }) + '\n')
      const frame = (chunk) => {
        const header = Buffer.from(
          `X-RequestId:${rid}\r\nContent-Type:audio/mpeg\r\nX-StreamId:${rid}\r\nPath:audio\r\n`, 'utf8')
        const len = Buffer.alloc(2)
        len.writeUInt16BE(header.length, 0)
        return Buffer.concat([len, header, chunk])
      }
      ws.send(`X-RequestId:${rid}\r\nContent-Type:application/json; charset=utf-8\r\nPath:turn.start\r\n\r\n{}`, { binary: false })
      ws.send(frame(CHUNK1), { binary: true })
      ws.send(frame(CHUNK2), { binary: true })
      ws.send(`X-RequestId:${rid}\r\nContent-Type:application/json; charset=utf-8\r\nPath:turn.end\r\n\r\n{}`, { binary: false })
    }
  })
})
console.log('mock edge on 50031')

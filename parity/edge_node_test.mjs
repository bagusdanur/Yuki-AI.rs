// Uji klien Edge Node asli (msedge-tts) melawan mock lokal (Fase 6):
// statik WSS_URL di-patch ke mock, lalu synthesize() dari lib/tts.js
// repo asli dipanggil dengan engine edge. Audio ditulis ke file argv.
import { createRequire } from 'node:module'
import fs from 'node:fs'

const require = createRequire('/home/hatch/workspace/yuki-ai-rust/original/package.json')
const { MsEdgeTTS } = require('msedge-tts')
MsEdgeTTS.WSS_URL = 'ws://127.0.0.1:50031/edge'

const { synthesize } = await import(
  'file:///home/hatch/workspace/yuki-ai-rust/original/lib/tts.js')

const settings = {
  engine: 'edge',
  voicevoxSpeaker: 14,
  voicevoxUrl: 'http://127.0.0.1:50021',
  voicevoxEnabled: true,
  voicevoxKeepAlive: true,
  voicevoxPm2Name: 'voicevox',
  edgeVoice: 'id-ID-GadisNeural',
  edgePitch: '+3Hz',
  edgeRate: '+0%'
}
const { buffer, mime } = await synthesize(
  'Halo Yuki, *melambaikan tangan* apa kabar?', 'senang', settings)
fs.writeFileSync(process.argv[2], buffer)
console.log('mime:', mime, 'bytes:', buffer.length)
// msedge-tts membiarkan WebSocket terbuka (di server produksi itu wajar);
// skrip uji harus keluar eksplisit.
process.exit(0)

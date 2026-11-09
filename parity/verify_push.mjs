// Verifikasi kriptografis satu pengiriman Web Push yang terekam mock:
// 1) Tanda tangan JWT VAPID (ES256) valid terhadap kunci publik VAPID.
// 2) Body aes128gcm (RFC 8291) terdekripsi dengan kunci subscription.
// Cara pakai: node verify_push.mjs <file-rekaman> <keys.json>
import crypto from 'node:crypto'
import fs from 'node:fs'

const [, , recordPath, keysPath] = process.argv
const record = JSON.parse(fs.readFileSync(recordPath, 'utf8'))
const keys = JSON.parse(fs.readFileSync(keysPath, 'utf8'))

const authHeader = record.headers['Authorization'] || record.headers['authorization'] || ''
const m = authHeader.match(/vapid t=([^,]+), k=(\S+)/)
if (!m) { console.log(JSON.stringify({ ok: false, error: 'header vapid tidak ditemukan' })); process.exit(1) }
const [, jwt, pubKeyB64] = m
if (pubKeyB64 !== keys.vapidPublic) { console.log(JSON.stringify({ ok: false, error: 'kunci k tidak cocok' })); process.exit(1) }

const [h, c, s] = jwt.split('.')
const point = Buffer.from(keys.vapidPublic, 'base64url')
const jwk = {
  kty: 'EC', crv: 'P-256',
  x: point.subarray(1, 33).toString('base64url'),
  y: point.subarray(33, 65).toString('base64url')
}
const verifyKey = crypto.createPublicKey({ key: jwk, format: 'jwk' })
const sigOk = crypto.verify(
  'sha256',
  Buffer.from(`${h}.${c}`),
  { key: verifyKey, dsaEncoding: 'ieee-p1363' },
  Buffer.from(s, 'base64url')
)
const claims = JSON.parse(Buffer.from(c, 'base64url').toString())

const body = Buffer.from(record.body_b64, 'base64')
const salt = body.subarray(0, 16)
const idLen = body[20]
const serverPublic = body.subarray(21, 21 + idLen)
const ciphertext = body.subarray(21 + idLen)

const ecdh = crypto.createECDH('prime256v1')
ecdh.setPrivateKey(Buffer.from(keys.subscription.p256dhPrivate, 'base64url'))
const shared = ecdh.computeSecret(serverPublic)
const uaPublic = Buffer.from(keys.subscription.p256dh, 'base64url')
const authSecret = Buffer.from(keys.subscription.auth, 'base64url')

const keyInfo = Buffer.concat([Buffer.from('WebPush: info\0'), uaPublic, serverPublic])
const ikm = Buffer.from(crypto.hkdfSync('sha256', shared, authSecret, keyInfo, 32))
const cek = Buffer.from(crypto.hkdfSync('sha256', ikm, salt, Buffer.from('Content-Encoding: aes128gcm\0'), 16))
const nonce = Buffer.from(crypto.hkdfSync('sha256', ikm, salt, Buffer.from('Content-Encoding: nonce\0'), 12))

const tag = ciphertext.subarray(ciphertext.length - 16)
const data = ciphertext.subarray(0, ciphertext.length - 16)
const decipher = crypto.createDecipheriv('aes-128-gcm', cek, nonce)
decipher.setAuthTag(tag)
const plain = Buffer.concat([decipher.update(data), decipher.final()])
// Buang padding: byte terakhir non-nol adalah delimiter 0x02.
let end = plain.length
while (end > 0 && plain[end - 1] === 0) end -= 1
const payload = plain.subarray(0, end - 1).toString()
console.log(JSON.stringify({ ok: true, sigOk, claims, payload: JSON.parse(payload) }))

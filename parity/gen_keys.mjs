// Buat pasangan kunci VAPID + kunci subscription Web Push untuk uji paritas.
import crypto from 'node:crypto'

function ecPair() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync('ec', { namedCurve: 'P-256' })
  const pub = publicKey.export({ format: 'jwk' })
  const priv = privateKey.export({ format: 'jwk' })
  const point = Buffer.concat([
    Buffer.from([0x04]),
    Buffer.from(pub.x, 'base64url'),
    Buffer.from(pub.y, 'base64url')
  ])
  return {
    publicB64: point.toString('base64url'),
    privateB64: Buffer.from(priv.d, 'base64url').toString('base64url')
  }
}

const vapid = ecPair()
const sub = ecPair()
const out = {
  vapidPublic: vapid.publicB64,
  vapidPrivate: vapid.privateB64,
  subscription: {
    p256dh: sub.publicB64,
    p256dhPrivate: sub.privateB64,
    auth: crypto.randomBytes(16).toString('base64url')
  }
}
console.log(JSON.stringify(out))

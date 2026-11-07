// Decoding worker: owns the wasm receiver so camera frames, QR detection
// and (slow, deliberately) scrypt decryption never block the UI thread.
//
// in:  {type:"frame", bitmap: ImageBitmap, maxDim, mode?: "receive"|"key"}   (bitmap transferred)
//      {type:"tx-start", name, data: ArrayBuffer, mode, key, version} | {type:"tx-next", count} | {type:"tx-stop"}
//      {type:"open", passphrase?: string, identity?: string}
//      {type:"reset"}
//      {type:"keygen"} | {type:"keyinfo", public}
// out: {type:"ready"} | {type:"error", message}
//      {type:"progress", status, ms, received, needed, complete, lastFound, lastNew, totalCodes, totalRejected}
//      {type:"opened", ok, error?, name?, data?: ArrayBuffer, auto?}
//      {type:"key", secret?, public, fingerprint, qr: {width, modules}}
//      {type:"keyscan", public?, fingerprint?}           (mode "key" frames)
//      {type:"tx-started", ok, error?, sourceFrames, ciphertextLen} | {type:"tx-qrs", codes: [{width, modules}]}

import { CourierQR, rgbaToGray } from "./courierqr.js";

let cq = null;
let canvas = null;
let ctx = null;
let gray = null;

CourierQR.load(new URL("./courierqr.wasm", import.meta.url))
  .then((m) => {
    cq = m;
    postMessage({ type: "ready" });
  })
  .catch((e) => postMessage({ type: "error", message: `Couldn't start the decoder: ${e.message}` }));

onmessage = ({ data: msg }) => {
  if (!cq) {
    msg.bitmap?.close?.();
    return;
  }
  switch (msg.type) {
    case "frame":
      return onFrame(msg.bitmap, msg.maxDim || 1280, msg.mode || "receive");
    case "tx-start": {
      const r = cq.txStart({ ...msg, data: new Uint8Array(msg.data) });
      return postMessage({ type: "tx-started", ...r });
    }
    case "tx-next": {
      const codes = [];
      for (let i = 0; i < (msg.count || 1); i++) {
        const q = cq.txNextQr();
        if (q) codes.push(q);
      }
      return postMessage({ type: "tx-qrs", codes }, codes.map((c) => c.modules.buffer));
    }
    case "tx-stop":
      return cq.txStop();
    case "open":
      return onOpen(msg);
    case "keygen": {
      const k = cq.generateKeys();
      return postMessage({ type: "key", ...k, fingerprint: cq.fingerprint(k.public), qr: cq.qr(k.public) });
    }
    case "keyinfo":
      return postMessage({ type: "key", public: msg.public, fingerprint: cq.fingerprint(msg.public), qr: cq.qr(msg.public) });
    case "reset":
      cq.reset();
      return postMessage({ type: "progress", status: 5, ms: 0, ...cq.progress() });
  }
};

function onFrame(bitmap, maxDim, mode) {
  const t0 = performance.now();
  // Downscale big camera frames: QR detection cost grows with pixel count,
  // and 1280px is plenty when the codes fill a good part of the view.
  const scale = Math.min(1, maxDim / Math.max(bitmap.width, bitmap.height));
  const w = Math.max(1, Math.round(bitmap.width * scale));
  const h = Math.max(1, Math.round(bitmap.height * scale));
  if (!canvas || canvas.width !== w || canvas.height !== h) {
    canvas = new OffscreenCanvas(w, h);
    ctx = canvas.getContext("2d", { willReadFrequently: true });
  }
  ctx.drawImage(bitmap, 0, 0, w, h);
  bitmap.close();
  gray = rgbaToGray(ctx.getImageData(0, 0, w, h).data, gray);
  if (mode === "key") {
    // Pairing: looking for a computer's age1… public key, not a transfer.
    const raw = cq.scanFirst(gray, w, h);
    const text = raw ? new TextDecoder().decode(raw).trim() : "";
    const fp = text.toLowerCase().startsWith("age1") ? cq.fingerprint(text) : null;
    return postMessage({ type: "keyscan", public: fp ? text.toLowerCase() : null, fingerprint: fp });
  }
  const status = cq.scan(gray, w, h);
  postMessage({ type: "progress", status, ms: performance.now() - t0, ...cq.progress() });
}

function onOpen({ passphrase, identity, auto = false }) {
  const r = identity != null ? cq.openIdentity(identity) : cq.openPassphrase(passphrase ?? "");
  if (!r.ok) return postMessage({ type: "opened", ok: false, error: r.error, code: r.code, auto });
  const { name, data } = cq.takeFile();
  postMessage({ type: "opened", ok: true, name, data: data.buffer, auto }, [data.buffer]);
}

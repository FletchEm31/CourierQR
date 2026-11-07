// Decoding worker: owns the wasm receiver so camera frames, QR detection
// and (slow, deliberately) scrypt decryption never block the UI thread.
//
// in:  {type:"frame", bitmap: ImageBitmap, maxDim}   (bitmap is transferred)
//      {type:"open", passphrase?: string, identity?: string}
//      {type:"reset"}
// out: {type:"ready"} | {type:"error", message}
//      {type:"progress", status, ms, received, needed, complete, lastFound, lastNew, totalCodes, totalRejected}
//      {type:"opened", ok, error?, name?, data?: ArrayBuffer}

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
      return onFrame(msg.bitmap, msg.maxDim || 1280);
    case "open":
      return onOpen(msg);
    case "reset":
      cq.reset();
      return postMessage({ type: "progress", status: 5, ms: 0, ...cq.progress() });
  }
};

function onFrame(bitmap, maxDim) {
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
  const status = cq.scan(gray, w, h);
  postMessage({ type: "progress", status, ms: performance.now() - t0, ...cq.progress() });
}

function onOpen({ passphrase, identity }) {
  const r = identity != null ? cq.openIdentity(identity) : cq.openPassphrase(passphrase ?? "");
  if (!r.ok) return postMessage({ type: "opened", ok: false, error: r.error, code: r.code });
  const { name, data } = cq.takeFile();
  postMessage({ type: "opened", ok: true, name, data: data.buffer }, [data.buffer]);
}

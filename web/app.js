// CourierQR receiver UI: camera -> worker (wasm) -> unlock -> save.

const $ = (id) => document.getElementById(id);
const views = { scan: $("scan"), pair: $("pair"), send: $("send"), unlock: $("unlock"), done: $("done") };

const MAX_DIM = 1280; // longest side handed to the decoder

let worker;
let workerReady = false;
let stream = null;
let busy = false; // a frame is in the worker; don't queue more (backpressure)
let running = false;
let file = null; // {name, data: Uint8Array}
let codeTimes = []; // timestamps of newly accepted pieces, for the rate readout
let scanMode = "receive"; // "receive" a transfer, or "key": look for a computer's public key

// ---------------------------------------------------------------- setup

function show(name) {
  for (const [k, el] of Object.entries(views)) el.hidden = k !== name;
  const tab = { scan: "receive", unlock: "receive", done: "receive", send: "send", pair: "keys" }[name];
  for (const t of ["receive", "send", "keys"]) $(`tab-${t}`).setAttribute("aria-selected", String(t === tab));
}

function startWorker() {
  worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  worker.onmessage = ({ data: m }) => {
    if (m.type === "ready") workerReady = true;
    else if (m.type === "error") fatal(m.message);
    else if (m.type === "progress") onProgress(m);
    else if (m.type === "opened") onOpened(m);
    else if (m.type === "key") onKey(m);
    else if (m.type === "keyscan") onKeyScan(m);
    else if (m.type === "tx-started") onTxStarted(m);
    else if (m.type === "tx-qrs") onTxQrs(m);
  };
  worker.onerror = (e) => fatal(`Decoder crashed: ${e.message || "unknown error"}`);
}

function fatal(msg) {
  $("pieces").textContent = msg;
  $("pieces").classList.add("error");
}

if ("serviceWorker" in navigator && window.isSecureContext) {
  const hadController = !!navigator.serviceWorker.controller;
  navigator.serviceWorker.register("sw.js").catch(() => {});
  // A new version took over: reload into it right away (unless we're in the
  // middle of something), instead of making people relaunch the app twice.
  navigator.serviceWorker.addEventListener("controllerchange", () => {
    if (!hadController) return; // first install, not an update
    const busyNow = running || tx.running || !views.unlock.hidden || !views.done.hidden;
    if (busyNow) pendingReload = true;
    else location.reload();
  });
}
let pendingReload = false;

// ---------------------------------------------------------------- camera

async function startCamera(deviceId) {
  stopCamera();
  const video = { width: { ideal: 1920 }, height: { ideal: 1080 }, frameRate: { ideal: 30 } };
  if (deviceId) video.deviceId = { exact: deviceId };
  else video.facingMode = { ideal: "environment" };

  try {
    stream = await navigator.mediaDevices.getUserMedia({ video, audio: false });
  } catch (e) {
    const hint = location.protocol !== "https:" && location.hostname !== "localhost"
      ? " The camera only works over HTTPS (or localhost)."
      : "";
    fatal(`Camera unavailable: ${e.name}.${hint}`);
    return;
  }

  const track = stream.getVideoTracks()[0];
  // Ask for continuous autofocus where the browser supports it (Android Chrome).
  try {
    const caps = track.getCapabilities?.() ?? {};
    if (caps.focusMode?.includes("continuous")) {
      await track.applyConstraints({ advanced: [{ focusMode: "continuous" }] });
    }
  } catch { /* optional */ }

  const v = $("video");
  v.srcObject = stream;
  await v.play().catch(() => {});
  $("idle").hidden = true;
  $("stop").hidden = false;
  running = true;
  listCameras(track.getSettings().deviceId);
  pump();
}

function stopCamera() {
  running = false;
  stream?.getTracks().forEach((t) => t.stop());
  stream = null;
  $("video").srcObject = null;
}

async function listCameras(activeId) {
  const cams = (await navigator.mediaDevices.enumerateDevices()).filter((d) => d.kind === "videoinput");
  const sel = $("camera");
  sel.hidden = cams.length < 2;
  sel.replaceChildren(
    ...cams.map((c, i) => {
      const o = new Option(c.label || `Camera ${i + 1}`, c.deviceId);
      o.selected = c.deviceId === activeId;
      return o;
    }),
  );
}

// Grab the newest camera frame whenever the worker is free.
function pump() {
  if (!running) return;
  const v = $("video");
  const next = () =>
    v.requestVideoFrameCallback ? v.requestVideoFrameCallback(pump) : requestAnimationFrame(pump);

  if (!busy && workerReady && v.readyState >= 2 && v.videoWidth) {
    busy = true;
    createImageBitmap(v)
      .then((bitmap) => worker.postMessage({ type: "frame", bitmap, maxDim: MAX_DIM, mode: scanMode }, [bitmap]))
      .catch(() => { busy = false; });
  }
  next();
}

// ---------------------------------------------------------------- progress

function onProgress(m) {
  busy = false;
  const now = performance.now();
  for (let i = 0; i < (m.lastNew || 0); i++) codeTimes.push(now);
  codeTimes = codeTimes.filter((t) => now - t < 3000);

  if (m.lastNew > 0 || m.status === 3) {
    const f = document.querySelector(".frame");
    f.classList.add("hit");
    setTimeout(() => f.classList.remove("hit"), 120);
  }

  if (m.needed > 0) {
    const pct = m.complete ? 100 : Math.min(99, Math.floor((100 * m.received) / m.needed));
    $("bar").style.width = `${pct}%`;
    $("meter").setAttribute("aria-valuenow", String(pct));
    $("pieces").classList.remove("error");
    $("pieces").textContent = `${m.received} / ~${m.needed} pieces`;
    $("rate").textContent = `${(codeTimes.length / 3).toFixed(1)} new/s · ${m.ms.toFixed(0)} ms/frame`;
  } else if (m.lastFound > 0 && m.status < 0) {
    $("pieces").textContent = "That QR code isn't from CourierQR";
  }

  if (m.complete && running) {
    stopCamera();
    $("unlock-sub").textContent = `${m.received} pieces received. The file is still encrypted; unlock it on this device.`;
    $("unlock-key").hidden = true;
    loadKey().then((key) => {
      if (key) {
        // Paired phone: try the device key first, silently.
        $("unlock-sub").textContent = `${m.received} pieces received. Unlocking with this phone's device key…`;
        show("unlock");
        unlocking(true);
        worker.postMessage({ type: "open", identity: key.secret, auto: true });
      } else {
        show("unlock");
        $("pass").focus();
      }
    });
  }
}

// ---------------------------------------------------------------- unlock

$("pass-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const passphrase = $("pass").value;
  $("pass").value = "";
  unlocking(true);
  worker.postMessage({ type: "open", passphrase });
});

$("identity").addEventListener("change", async (e) => {
  const f = e.target.files?.[0];
  e.target.value = "";
  if (!f) return;
  unlocking(true);
  worker.postMessage({ type: "open", identity: await f.text() });
});

function unlocking(on) {
  $("unlock-btn").disabled = on;
  $("unlock-btn").textContent = on ? "Unlocking…" : "Unlock";
  if (on) $("unlock-err").hidden = true;
}

function onOpened(m) {
  unlocking(false);
  if (!m.ok && m.auto) {
    // Not addressed to this phone's key: fall back to the passphrase.
    $("unlock-key").hidden = false;
    $("pass").focus();
    return;
  }
  if (!m.ok) {
    $("unlock-err").textContent =
      m.code === -2 ? "That didn't unlock it. Wrong passphrase or key." : m.error;
    $("unlock-err").hidden = false;
    $("pass").focus();
    return;
  }
  file = { name: m.name || "received.bin", data: new Uint8Array(m.data) };
  renderFile();
  show("done");
}

// ---------------------------------------------------------------- received

function renderFile() {
  $("fname").textContent = file.name;
  $("fmeta").textContent = formatBytes(file.data.length);
  const text = asText(file.data);
  $("preview").hidden = text == null;
  $("copy").hidden = text == null;
  if (text != null) $("preview").textContent = text.length > 4000 ? `${text.slice(0, 4000)}\n…` : text;
  $("share").hidden = !canShare();
}

function asText(bytes) {
  if (bytes.length > 2_000_000) return null;
  try {
    const t = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return /[\u0000-\u0008\u000E-\u001F]/.test(t.slice(0, 4096)) ? null : t;
  } catch {
    return null;
  }
}

function blob() {
  return new Blob([file.data], { type: "application/octet-stream" });
}

function canShare() {
  try {
    return !!navigator.canShare?.({ files: [new File([""], "x.txt")] });
  } catch {
    return false;
  }
}

$("save").addEventListener("click", () => {
  const url = URL.createObjectURL(blob());
  const a = Object.assign(document.createElement("a"), { href: url, download: safeName(file.name) });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
});

$("share").addEventListener("click", async () => {
  try {
    await navigator.share({ files: [new File([blob()], safeName(file.name))] });
  } catch { /* user cancelled */ }
});

$("copy").addEventListener("click", async () => {
  await navigator.clipboard.writeText(asText(file.data) ?? "");
  $("copy").textContent = "Copied ✓";
  setTimeout(() => ($("copy").textContent = "Copy text"), 1500);
});

function safeName(name) {
  const base = name.split(/[\\/]/).pop().replace(/[\u0000-\u001F]/g, "").trim();
  return base && base !== "." && base !== ".." ? base : "received.bin";
}

function formatBytes(n) {
  return n < 1024 ? `${n} bytes` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(2)} MB`;
}

// ---------------------------------------------------------------- device key

// The secret half lives in IndexedDB on this phone and nowhere else.
const DB = "courierqr";
function idb(mode, fn) {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open(DB, 1);
    open.onupgradeneeded = () => open.result.createObjectStore("keys");
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const tx = open.result.transaction("keys", mode);
      const req = fn(tx.objectStore("keys"));
      tx.oncomplete = () => resolve(req?.result);
      tx.onerror = () => reject(tx.error);
    };
  });
}
const loadKey = () => idb("readonly", (st) => st.get("device")).catch(() => null);
const saveKey = (k) => idb("readwrite", (st) => st.put(k, "device"));
const deleteKey = () => idb("readwrite", (st) => st.delete("device"));

let shownKey = null;

async function openPairView() {
  stopCamera();
  $("idle").hidden = false;
  $("stop").hidden = true;
  const key = await loadKey();
  $("nokey").hidden = !!key;
  $("haskey").hidden = !key;
  if (key) worker.postMessage({ type: "keyinfo", public: key.public });
  await renderComputer();
  show("pair");
}

// ---- the computer this phone sends to (its public key only)
const loadComputer = () => idb("readonly", (st) => st.get("computer")).catch(() => null);
let pendingComputer = null;

async function renderComputer() {
  const c = await loadComputer();
  $("nocomp").hidden = !!c || !!pendingComputer;
  $("confirmcomp").hidden = !pendingComputer;
  $("hascomp").hidden = !c || !!pendingComputer;
  if (c) $("compfp").textContent = c.fingerprint;
}

$("scancomp").addEventListener("click", () => {
  scanMode = "key";
  $("idle-text").textContent = "Point the camera at the code from `courierqr pair show`.";
  $("pieces").textContent = "Looking for the computer's key…";
  $("rate").textContent = "";
  $("bar").style.width = "0";
  show("scan");
  startCamera();
});

function onKeyScan(m) {
  busy = false;
  if (scanMode !== "key" || !m.public) return;
  stopCamera();
  scanMode = "receive";
  resetScanText();
  pendingComputer = { public: m.public, fingerprint: m.fingerprint, added: Date.now() };
  $("compfp-new").textContent = m.fingerprint;
  openPairView();
}

$("compyes").addEventListener("click", async () => {
  await idb("readwrite", (st) => st.put(pendingComputer, "computer"));
  navigator.storage?.persist?.().catch(() => {});
  pendingComputer = null;
  renderComputer();
});
$("compno").addEventListener("click", () => {
  pendingComputer = null;
  renderComputer();
});
let delCompArmed = null;
$("delcomp").addEventListener("click", async () => {
  if (!delCompArmed) {
    $("delcomp").textContent = "Tap again to forget";
    delCompArmed = setTimeout(() => {
      delCompArmed = null;
      $("delcomp").textContent = "Forget this computer";
    }, 4000);
    return;
  }
  clearTimeout(delCompArmed);
  delCompArmed = null;
  $("delcomp").textContent = "Forget this computer";
  await idb("readwrite", (st) => st.delete("computer"));
  renderComputer();
});

function resetScanText() {
  $("idle-text").innerHTML = "Point your camera at the CourierQR window.<br>Order doesn't matter, and missed frames are fine.";
  $("pieces").textContent = "waiting for codes…";
}

async function onKey(m) {
  if (m.secret) {
    await saveKey({ secret: m.secret, public: m.public, created: Date.now() });
    // Ask the browser not to evict our storage (matters most on iOS).
    navigator.storage?.persist?.().catch(() => {});
    $("nokey").hidden = true;
    $("haskey").hidden = false;
  }
  shownKey = m.public;
  $("paircmd").textContent = `courierqr pair add phone ${m.public}`;
  $("fp").textContent = m.fingerprint;
  $("sharekey").hidden = !navigator.share;
  drawQr($("keyqr"), m.qr);
}

function drawQr(canvas, qr) {
  const ctx = canvas.getContext("2d");
  const quiet = 4;
  const n = qr.width + 2 * quiet;
  const scale = Math.floor(canvas.width / n);
  const off = Math.floor((canvas.width - n * scale) / 2);
  ctx.fillStyle = "#fff";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.fillStyle = "#000";
  for (let y = 0; y < qr.width; y++) {
    for (let x = 0; x < qr.width; x++) {
      if (qr.modules[y * qr.width + x]) ctx.fillRect(off + (x + quiet) * scale, off + (y + quiet) * scale, scale, scale);
    }
  }
}

$("genkey").addEventListener("click", () => worker.postMessage({ type: "keygen" }));
$("copykey").addEventListener("click", async () => {
  await navigator.clipboard.writeText($("paircmd").textContent);
  $("copykey").textContent = "Copied ✓";
  setTimeout(() => ($("copykey").textContent = "Copy command"), 1500);
});
$("sharekey").addEventListener("click", () =>
  navigator.share({ text: $("paircmd").textContent }).catch(() => {}),
);
// Two taps to delete: no browser dialogs.
let delArmed = null;
$("delkey").addEventListener("click", async () => {
  if (!delArmed) {
    $("delkey").textContent = "Tap again to delete. Files sent to this key can't be opened after";
    delArmed = setTimeout(() => {
      delArmed = null;
      $("delkey").textContent = "Delete this device key";
    }, 4000);
    return;
  }
  clearTimeout(delArmed);
  delArmed = null;
  await deleteKey();
  shownKey = null;
  $("delkey").textContent = "Delete this device key";
  $("nokey").hidden = false;
  $("haskey").hidden = true;
});

// ---------------------------------------------------------------- sending

let sendFile = null; // {name, data: Uint8Array}
const tx = { running: false, paused: false, timer: 0, queue: [], waiting: false, shown: 0, k: 0, grid: 1, fps: 6, lock: null };

async function openSendView() {
  stopCamera();
  $("idle").hidden = false;
  $("stop").hidden = true;
  const c = await loadComputer();
  $("to-device").disabled = !c;
  $("to-device-label").textContent = c ? `Paired computer (${c.fingerprint})` : "Paired computer (pair one under Keys)";
  if (c && !$("to-device").dataset.touched) $("to-device").checked = true;
  syncSendForm();
  show("send");
}

function syncSendForm() {
  const byPass = $("to-pass").checked;
  $("sendpass").hidden = !byPass;
  $("sendgo").disabled = !sendFile || (byPass && $("sendpass").value.length < 12);
  if (sendFile) {
    const v = Number($("qrv").value), grid = Number($("grid").value), fps = Number($("fps").value);
    // ~ciphertext ≈ compressed size + ~250 B age overhead; estimate assumes no compression
    const perFrame = Math.max(1, workerCapacity(v) - 28);
    const frames = Math.ceil((sendFile.data.length + 250) / perFrame);
    const secs = frames / (fps * grid);
    // Real-world pickup is a fraction of the codes shown (webcams: ~1 in 5).
    const realistic = secs * 4;
    const human = (t) => (t < 90 ? `${Math.round(t)} s` : t < 5400 ? `${Math.round(t / 60)} min` : `${(t / 3600).toFixed(1)} h`);
    const big = realistic > 180;
    $("pickmeta").classList.toggle("warn", big);
    $("pickmeta").textContent =
      `${formatBytes(sendFile.data.length)} · up to ${frames} codes · roughly ${human(secs)}–${human(realistic)} to a webcam` +
      (big ? " (too big for a webcam; this is a notes-and-keys channel)" : "");
  } else {
    $("pickmeta").textContent = "";
    $("pickmeta").classList.remove("warn");
  }
}
// QR byte capacities at ECC L (matches cq_qr_capacity)
const workerCapacity = (v) => ({ 8: 192, 10: 271, 12: 367, 15: 520 })[v] ?? 271;

$("pick").addEventListener("change", async (e) => {
  const f = e.target.files?.[0];
  if (!f) return;
  sendFile = { name: f.name, data: new Uint8Array(await f.arrayBuffer()) };
  $("pickname").textContent = f.name;
  $("sendtext").value = "";
  syncSendForm();
});

// Typed/pasted text becomes a small .txt file: the sweet spot for this channel.
$("sendtext").addEventListener("input", () => {
  const text = $("sendtext").value;
  if (text.trim()) {
    const stamp = new Date().toISOString().slice(0, 16).replace("T", "_").replace(":", "-");
    sendFile = { name: `note_${stamp}.txt`, data: new TextEncoder().encode(text) };
    $("pickname").textContent = "Choose a file…";
    $("pick").value = "";
  } else if (!$("pick").files?.length) {
    sendFile = null;
  }
  syncSendForm();
});
for (const id of ["to-device", "to-pass"]) $(id).addEventListener("change", () => { $("to-device").dataset.touched = "1"; syncSendForm(); });
for (const id of ["sendpass", "qrv", "grid", "fps"]) $(id).addEventListener("input", syncSendForm);

$("sendgo").addEventListener("click", async () => {
  $("send-err").hidden = true;
  const byPass = $("to-pass").checked;
  const key = byPass ? $("sendpass").value : (await loadComputer())?.public;
  if (!key) return;
  $("sendgo").disabled = true;
  $("sendgo").textContent = byPass ? "Encrypting…" : "Starting…";
  tx.grid = Number($("grid").value);
  tx.fps = Number($("fps").value);
  const data = sendFile.data.slice().buffer;
  worker.postMessage(
    { type: "tx-start", name: sendFile.name, data, mode: byPass ? "passphrase" : "device", key, version: Number($("qrv").value) },
    [data],
  );
});

function onTxStarted(m) {
  $("sendgo").textContent = "Start sending";
  syncSendForm();
  if (!m.ok) {
    $("send-err").textContent = m.error;
    $("send-err").hidden = false;
    return;
  }
  $("sendpass").value = "";
  Object.assign(tx, { running: true, paused: false, queue: [], waiting: false, shown: 0, k: m.sourceFrames });
  $("broadcast").hidden = false;
  $("bpause").textContent = "Pause";
  sizeCanvas();
  navigator.wakeLock?.request("screen").then((l) => (tx.lock = l)).catch(() => {});
  requestCodes();
  tx.timer = setInterval(tick, 1000 / tx.fps);
}

function requestCodes() {
  if (tx.waiting || tx.queue.length >= 2) return;
  tx.waiting = true;
  worker.postMessage({ type: "tx-next", count: tx.grid });
}

function onTxQrs(m) {
  tx.waiting = false;
  if (!tx.running) return;
  tx.queue.push(m.codes);
  requestCodes();
}

function tick() {
  if (!tx.running || tx.paused) return;
  const codes = tx.queue.shift();
  requestCodes();
  if (!codes) return; // encoder fell behind; keep showing the last set
  drawCodes(codes);
  tx.shown += codes.length;
  $("bstatus").textContent = `${tx.shown} codes shown · receiver needs ≈${tx.k} · ${tx.fps}/s`;
}

function sizeCanvas() {
  const c = $("bcanvas");
  const dpr = window.devicePixelRatio || 1;
  const side = Math.floor(Math.min(innerWidth, innerHeight - 70) * 0.96);
  c.style.width = c.style.height = `${side}px`;
  c.width = c.height = Math.floor(side * dpr);
}

function drawCodes(codes) {
  const c = $("bcanvas");
  const ctx = c.getContext("2d");
  const cols = codes.length > 1 ? 2 : 1;
  const w = codes[0].width;
  const cell = w + 8; // 4-module quiet zone each side
  const scale = Math.max(1, Math.floor(c.width / (cell * cols)));
  const off = Math.floor((c.width - scale * cell * cols) / 2);
  ctx.fillStyle = "#fff";
  ctx.fillRect(0, 0, c.width, c.height);
  ctx.fillStyle = "#000";
  codes.forEach((q, i) => {
    const ox = off + (i % cols) * cell * scale + 4 * scale;
    const oy = off + Math.floor(i / cols) * cell * scale + 4 * scale;
    for (let y = 0; y < q.width; y++) {
      for (let x = 0; x < q.width; x++) {
        if (q.modules[y * q.width + x]) ctx.fillRect(ox + x * scale, oy + y * scale, scale, scale);
      }
    }
  });
}

function stopSending() {
  if (pendingReload) return location.reload();
  tx.running = false;
  clearInterval(tx.timer);
  tx.lock?.release?.().catch(() => {});
  tx.lock = null;
  worker.postMessage({ type: "tx-stop" });
  $("broadcast").hidden = true;
}

$("bpause").addEventListener("click", () => {
  tx.paused = !tx.paused;
  $("bpause").textContent = tx.paused ? "Resume" : "Pause";
});
$("bstop").addEventListener("click", stopSending);
$("bcanvas").addEventListener("click", () => $("bpause").click());
addEventListener("resize", () => tx.running && sizeCanvas());

// ---------------------------------------------------------------- tabs

$("tab-receive").addEventListener("click", () => {
  scanMode = "receive";
  resetScanText();
  show("scan");
});
$("tab-send").addEventListener("click", openSendView);
$("tab-keys").addEventListener("click", openPairView);

// ---------------------------------------------------------------- reset

function resetAll() {
  if (pendingReload) return location.reload();
  file?.data.fill(0);
  file = null;
  codeTimes = [];
  worker.postMessage({ type: "reset" });
  $("bar").style.width = "0";
  $("pieces").textContent = "waiting for codes…";
  $("rate").textContent = "";
  $("unlock-err").hidden = true;
  show("scan");
  startCamera($("camera").value || undefined);
}

$("again").addEventListener("click", resetAll);
$("discard").addEventListener("click", resetAll);
$("start").addEventListener("click", () => startCamera());
$("stop").addEventListener("click", () => {
  stopCamera();
  $("idle").hidden = false;
  $("stop").hidden = true;
});
$("camera").addEventListener("change", (e) => startCamera(e.target.value));

startWorker();

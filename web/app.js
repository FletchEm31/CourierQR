// CourierQR receiver UI: camera -> worker (wasm) -> unlock -> save.

const $ = (id) => document.getElementById(id);
const views = { scan: $("scan"), unlock: $("unlock"), done: $("done") };

const MAX_DIM = 1280; // longest side handed to the decoder

let worker;
let workerReady = false;
let stream = null;
let busy = false; // a frame is in the worker; don't queue more (backpressure)
let running = false;
let file = null; // {name, data: Uint8Array}
let codeTimes = []; // timestamps of newly accepted pieces, for the rate readout

// ---------------------------------------------------------------- setup

function show(name) {
  for (const [k, el] of Object.entries(views)) el.hidden = k !== name;
}

function startWorker() {
  worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  worker.onmessage = ({ data: m }) => {
    if (m.type === "ready") workerReady = true;
    else if (m.type === "error") fatal(m.message);
    else if (m.type === "progress") onProgress(m);
    else if (m.type === "opened") onOpened(m);
  };
  worker.onerror = (e) => fatal(`Decoder crashed: ${e.message || "unknown error"}`);
}

function fatal(msg) {
  $("pieces").textContent = msg;
  $("pieces").classList.add("error");
}

if ("serviceWorker" in navigator && window.isSecureContext) {
  navigator.serviceWorker.register("sw.js").catch(() => {});
}

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
      .then((bitmap) => worker.postMessage({ type: "frame", bitmap, maxDim: MAX_DIM }, [bitmap]))
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

  if (m.complete) {
    stopCamera();
    $("unlock-sub").textContent = `${m.received} pieces received. The file is still encrypted; unlock it on this device.`;
    show("unlock");
    $("pass").focus();
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

// ---------------------------------------------------------------- reset

function resetAll() {
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

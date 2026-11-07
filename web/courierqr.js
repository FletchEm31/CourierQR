// Glue for courierqr_wasm.wasm — no wasm-bindgen, no dependencies.
// Works in the page, in a Web Worker, and in Node (for tests).
//
// The wasm module owns one receiver. Bytes cross into wasm memory through
// cq_alloc/cq_free; results come back as (pointer, length) pairs that we
// copy out immediately (wasm memory can grow and detach old views).

export const Status = Object.freeze({
  PROGRESS: 0,
  DUPLICATE: 1,
  OTHER_TRANSFER: 2,
  COMPLETE: 3,
  ALREADY_COMPLETE: 4,
  NOTHING: 5,
  NOT_COURIERQR: -1,
  UNSUPPORTED_VERSION: -2,
  TRUNCATED: -3,
  CHECKSUM: -4,
  BAD_PARAMETERS: -5,
});

export const OpenStatus = Object.freeze({
  OK: 0,
  NOT_COMPLETE: -1,
  DECRYPT: -2,
  BAD_KEY: -3,
  MALFORMED: -4,
});

const utf8 = new TextEncoder();
const fromUtf8 = new TextDecoder();

export class CourierQR {
  #x; // wasm exports

  /** @param {string|URL|Response|ArrayBuffer|Uint8Array} source */
  static async load(source) {
    let bytes = source;
    if (typeof source === "string" || source instanceof URL) bytes = await fetch(source);
    if (typeof Response !== "undefined" && bytes instanceof Response) {
      if (!bytes.ok) throw new Error(`failed to load wasm: HTTP ${bytes.status}`);
      bytes = await bytes.arrayBuffer();
    }
    let exports;
    const imports = {
      env: {
        // getrandom's "custom" backend lands here.
        host_random(ptr, len) {
          const buf = new Uint8Array(exports.memory.buffer, ptr, len);
          for (let i = 0; i < len; i += 65536) {
            globalThis.crypto.getRandomValues(buf.subarray(i, Math.min(len, i + 65536)));
          }
        },
      },
    };
    const { instance } = await WebAssembly.instantiate(bytes, imports);
    exports = instance.exports;
    return new CourierQR(exports);
  }

  constructor(exports) {
    this.#x = exports;
  }

  /** Copy `bytes` into wasm memory, run fn(ptr, len), then wipe and free. */
  #withBytes(bytes, fn) {
    const x = this.#x;
    const len = bytes.length;
    const ptr = x.cq_alloc(len);
    new Uint8Array(x.memory.buffer, ptr, len).set(bytes);
    try {
      return fn(ptr, len);
    } finally {
      new Uint8Array(x.memory.buffer, ptr, len).fill(0); // don't leave secrets around
      x.cq_free(ptr, len);
    }
  }

  #copyOut(ptr, len) {
    return len ? new Uint8Array(this.#x.memory.buffer, ptr, len).slice() : new Uint8Array(0);
  }

  /** Decode all QR codes in a grayscale frame (Uint8Array, w*h). Returns a Status. */
  scan(gray, width, height) {
    return this.#withBytes(gray, (p) => this.#x.cq_scan(p, width, height));
  }

  /** Feed one already-decoded QR payload. Returns a Status. */
  push(bytes) {
    return this.#withBytes(bytes, (p, n) => this.#x.cq_push(p, n));
  }

  progress() {
    const x = this.#x;
    return {
      received: x.cq_received(),
      needed: x.cq_needed(),
      complete: x.cq_is_complete() === 1,
      lastFound: x.cq_last_found(),
      lastNew: x.cq_last_new(),
      totalCodes: x.cq_total_codes(),
      totalRejected: x.cq_total_rejected(),
    };
  }

  /** @returns {{ok: boolean, code: number, error: string}} */
  openPassphrase(passphrase) {
    const code = this.#withBytes(utf8.encode(passphrase), (p, n) => this.#x.cq_open_passphrase(p, n));
    return this.#openResult(code);
  }

  /** Accepts the contents of an identity file (comments are ignored). */
  openIdentity(identityText) {
    const code = this.#withBytes(utf8.encode(identityText), (p, n) => this.#x.cq_open_identity(p, n));
    return this.#openResult(code);
  }

  #openResult(code) {
    const x = this.#x;
    const error = code === OpenStatus.OK ? "" : fromUtf8.decode(this.#copyOut(x.cq_error_ptr(), x.cq_error_len()));
    return { ok: code === OpenStatus.OK, code, error };
  }

  /** Copy the decrypted file out of wasm and wipe it there. */
  takeFile() {
    const x = this.#x;
    const name = fromUtf8.decode(this.#copyOut(x.cq_file_name_ptr(), x.cq_file_name_len()));
    const data = this.#copyOut(x.cq_file_data_ptr(), x.cq_file_data_len());
    x.cq_forget_file();
    return { name, data };
  }

  reset() {
    this.#x.cq_reset();
  }
}

/**
 * RGBA (ImageData.data) -> 8-bit luma. Integer BT.601 weights; reuses `out`
 * when it's big enough.
 */
export function rgbaToGray(rgba, out) {
  const n = rgba.length >> 2;
  const g = out && out.length >= n ? out.subarray(0, n) : new Uint8Array(n);
  for (let i = 0, j = 0; i < n; i++, j += 4) {
    g[i] = (rgba[j] * 77 + rgba[j + 1] * 150 + rgba[j + 2] * 29) >> 8;
  }
  return g;
}

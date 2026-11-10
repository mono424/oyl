/**
 * SHA-256 fed piece by piece, for checking a blob while it streams in
 * (sync.ts). `crypto.subtle.digest` needs the whole input at once, and holding
 * a 50 MB upload whole, twice over while it is joined, is more than a Worker
 * isolate should spend on one request. Plain arithmetic, so the tests run the
 * same code as the Worker.
 */

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

export class Sha256 {
  private readonly h = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]);
  private readonly w = new Uint32Array(64);
  private readonly buf = new Uint8Array(64);
  private buffered = 0;
  private length = 0;
  private done = false;

  update(data: Uint8Array): this {
    if (this.done) throw new Error('Sha256: update after digest');
    this.length += data.byteLength;
    let i = 0;
    if (this.buffered) {
      i = Math.min(64 - this.buffered, data.byteLength);
      this.buf.set(data.subarray(0, i), this.buffered);
      this.buffered += i;
      if (this.buffered < 64) return this;
      this.block(this.buf, 0);
      this.buffered = 0;
    }
    for (; i + 64 <= data.byteLength; i += 64) this.block(data, i);
    if (i < data.byteLength) {
      this.buf.set(data.subarray(i), 0);
      this.buffered = data.byteLength - i;
    }
    return this;
  }

  /** The digest as lowercase hex. Ends the hash. */
  hex(): string {
    if (this.done) throw new Error('Sha256: digest taken twice');
    const bits = this.length * 8;
    const tail = new Uint8Array(this.buffered < 56 ? 64 : 128);
    tail.set(this.buf.subarray(0, this.buffered));
    tail[this.buffered] = 0x80;
    const view = new DataView(tail.buffer);
    view.setUint32(tail.length - 8, Math.floor(bits / 2 ** 32));
    view.setUint32(tail.length - 4, bits >>> 0);
    for (let o = 0; o < tail.length; o += 64) this.block(tail, o);
    this.done = true;
    let out = '';
    for (const word of this.h) out += word.toString(16).padStart(8, '0');
    return out;
  }

  private block(d: Uint8Array, o: number) {
    const w = this.w;
    for (let t = 0; t < 16; t++) {
      const j = o + t * 4;
      w[t] = (d[j] << 24) | (d[j + 1] << 16) | (d[j + 2] << 8) | d[j + 3];
    }
    for (let t = 16; t < 64; t++) {
      const x = w[t - 15];
      const y = w[t - 2];
      const s0 = ((x >>> 7) | (x << 25)) ^ ((x >>> 18) | (x << 14)) ^ (x >>> 3);
      const s1 = ((y >>> 17) | (y << 15)) ^ ((y >>> 19) | (y << 13)) ^ (y >>> 10);
      w[t] = (w[t - 16] + s0 + w[t - 7] + s1) | 0;
    }
    const h = this.h;
    let a = h[0], b = h[1], c = h[2], e = h[4], f = h[5], g = h[6], k = h[7], dd = h[3];
    for (let t = 0; t < 64; t++) {
      const s1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
      const ch = (e & f) ^ (~e & g);
      const t1 = (k + s1 + ch + K[t] + w[t]) | 0;
      const s0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (s0 + maj) | 0;
      k = g;
      g = f;
      f = e;
      e = (dd + t1) | 0;
      dd = c;
      c = b;
      b = a;
      a = (t1 + t2) | 0;
    }
    h[0] += a;
    h[1] += b;
    h[2] += c;
    h[3] += dd;
    h[4] += e;
    h[5] += f;
    h[6] += g;
    h[7] += k;
  }
}

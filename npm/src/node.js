// Node.js entry point: WASM loaded from disk, file paths accepted everywhere an input is.
//
// Environment: FLASHRAW_WASM_SIMD=0 forces the scalar fallback binary (benchmarks, debugging).

import { readFile, open } from 'node:fs/promises';
import { availableParallelism, cpus } from 'node:os';
import { fileURLToPath } from 'node:url';
import { _setPlatform, IoError } from './core.js';

const STAGING = 8 << 20; // chunk size when streaming files into WASM memory

function ioError(e, path) {
  return new IoError(`${e.code ?? 'EIO'}: cannot read ${path}: ${e.message}`, { cause: e });
}

function toPath(p) {
  return p instanceof URL ? fileURLToPath(p) : p;
}

_setPlatform({
  name: 'node',
  load(variant, options) {
    const file = variant === 'simd' ? 'flashraw_simd.wasm' : 'flashraw.wasm';
    const url = options.baseUrl
      ? new URL(file, options.baseUrl)
      : variant === 'simd'
        ? new URL('./flashraw_simd.wasm', import.meta.url)
        : new URL('./flashraw.wasm', import.meta.url);
    return readFile(url);
  },
  defaultSimd() {
    const v = process.env.FLASHRAW_WASM_SIMD;
    if (v === '0' || v === 'false') return false;
    return undefined;
  },
  // Files are read in 8 MiB chunks straight into WASM memory: one copy, bounded JS memory,
  // and the event loop stays responsive.
  async fileSource(p) {
    const path = toPath(p);
    let fh;
    try {
      fh = await open(path, 'r');
    } catch (e) {
      throw ioError(e, path);
    }
    let size;
    try {
      size = (await fh.stat()).size;
    } catch (e) {
      await fh.close();
      throw ioError(e, path);
    }
    async function* chunks() {
      const staging = Buffer.allocUnsafe(Math.max(1, Math.min(size, STAGING)));
      let pos = 0;
      try {
        while (pos < size) {
          const { bytesRead } = await fh.read(staging, 0, Math.min(staging.length, size - pos), pos);
          if (bytesRead === 0) break;
          yield staging.subarray(0, bytesRead);
          pos += bytesRead;
        }
      } catch (e) {
        throw ioError(e, path);
      } finally {
        await fh.close();
      }
    }
    return { size, chunks: chunks() };
  },
  async readHead(p, max) {
    const path = toPath(p);
    let fh;
    try {
      fh = await open(path, 'r');
      const { size } = await fh.stat();
      const head = Buffer.allocUnsafe(Math.min(size, max));
      const { bytesRead } = await fh.read(head, 0, head.length, 0);
      return { head: head.subarray(0, bytesRead), size };
    } catch (e) {
      throw ioError(e, path);
    } finally {
      await fh?.close();
    }
  },
  hardwareConcurrency: () => (availableParallelism ? availableParallelism() : cpus().length),
  workers: () => true,
});

export * from './core.js';

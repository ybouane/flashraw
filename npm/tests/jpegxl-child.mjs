// Child process for jpegxl.test.mjs: opens and decodes the given files in order and reports, per
// file, which binary decoded it and how many JPEG XL variants were loaded so far. The binary and
// mode are chosen by the environment (FLASHRAW_WASM_THREADS, FLASHRAW_WASM_SIMD,
// FLASHRAW_WASM_JPEGXL).
import { FlashRaw, init, getCapabilities, _internals } from 'flashraw';
import { fnv1a64U16 } from './helpers.mjs';

const info = await init();
const files = [];
for (const file of process.argv.slice(2)) {
  const r = { file };
  const raw = await FlashRaw.openFile(file);
  r.compression = raw.frameInfo.compression;
  r.openedIn = raw.binary;
  // Metadata and embedded previews never need the JPEG XL codec.
  r.format = raw.metadata.format;
  r.preview = (await raw.preview())?.data.length ?? 0;
  r.loadsBeforeDecode = _internals().jpegxl.loads;
  try {
    // Two concurrent decodes: the variant still loads once and the file moves once.
    const [f, again] = await Promise.all([raw.decode(), raw.decode()]);
    r.hash = fnv1a64U16(f.data);
    r.same = fnv1a64U16(again.data) === r.hash;
    r.size = [f.width, f.height, f.data.length];
    r.backend = { binary: f.backend.binary, jpegxl: f.backend.jpegxl, threads: f.backend.threads };
    const region = await raw.decodeRegion({ x: 16, y: 16, width: 64, height: 32 });
    r.region = [region.width, region.height];
  } catch (e) {
    r.error = { name: e.name, kind: e.kind, message: e.message };
  }
  r.decodedIn = raw.binary;
  r.loads = _internals().jpegxl.loads;
  raw.close();
  files.push(r);
}
const caps = await getCapabilities();
process.stdout.write(
  JSON.stringify({ info, jpegxl: caps.jpegxl, threads: caps.threads.count, internals: _internals().jpegxl, files }),
);

// Child process for simd.test.mjs: prints the binary variant and output hashes as JSON.
import { FlashRaw, init } from 'flashraw';
import { fnv1a64U16 } from './helpers.mjs';

const info = await init();
const hashes = {};
for (const file of process.argv.slice(2)) {
  const raw = await FlashRaw.openFile(file);
  const u16 = await raw.decode();
  const rgba = await raw.decode({ output: 'rgba8' });
  raw.close();
  // Hash RGBA bytes pairwise as u16 to reuse the same FNV helper.
  const rgba16 = new Uint16Array(rgba.data.buffer, 0, rgba.data.length >> 1);
  hashes[file] = { u16: fnv1a64U16(u16.data), rgba8: fnv1a64U16(rgba16) };
}
process.stdout.write(JSON.stringify({ variant: info.variant, level: info.simdLevel, hashes }));

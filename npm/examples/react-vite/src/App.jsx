// Minimal React integration: a hook that opens a RAW file in a worker, shows the embedded
// preview immediately, then the decoded image.
import { useEffect, useRef, useState } from 'react';
import { createWorkerPool } from 'flashraw/worker';

// One pool for the whole app; workers are spawned lazily on first use.
const pool = createWorkerPool({ size: 2 });

function useRawFile(file) {
  const [state, setState] = useState({ status: 'idle' });
  useEffect(() => {
    if (!file) return undefined;
    let cancelled = false;
    let raw;
    (async () => {
      setState({ status: 'opening' });
      raw = await pool.open(file);
      if (cancelled) return;
      const preview = await raw.preview();
      const previewUrl =
        preview?.mimeType === 'image/jpeg' ? URL.createObjectURL(new Blob([preview.data], { type: preview.mimeType })) : null;
      if (cancelled) return;
      setState({ status: 'decoding', metadata: raw.metadata, previewUrl });
      const frame = await raw.decode({ output: 'rgba8', demosaic: 'half-size' });
      if (!cancelled) setState({ status: 'done', metadata: raw.metadata, previewUrl, frame });
    })().catch((error) => !cancelled && setState({ status: 'error', error }));
    return () => {
      cancelled = true;
      raw?.close();
    };
  }, [file]);
  return state;
}

function FrameCanvas({ frame }) {
  const ref = useRef(null);
  useEffect(() => {
    const c = ref.current;
    c.width = frame.width;
    c.height = frame.height;
    c.getContext('2d').putImageData(new ImageData(frame.data, frame.width, frame.height), 0, 0);
  }, [frame]);
  return <canvas ref={ref} style={{ maxWidth: '100%' }} />;
}

export default function App() {
  const [file, setFile] = useState(null);
  const { status, metadata, previewUrl, frame, error } = useRawFile(file);
  return (
    <main style={{ fontFamily: 'system-ui, sans-serif', padding: 16 }}>
      <h1 style={{ fontSize: 18 }}>FlashRaw + React + Vite</h1>
      <input type="file" onChange={(e) => setFile(e.target.files?.[0] ?? null)} />
      <p>
        {status}
        {metadata && ` — ${metadata.camera.name}, ${metadata.width}×${metadata.height}, ISO ${metadata.exposure.iso ?? '?'}`}
        {frame && ` — decoded in ${frame.timings.totalMs.toFixed(0)} ms (${frame.backend.decode})`}
      </p>
      {error && <p style={{ color: 'crimson' }}>{`${error.kind}: ${error.message}`}</p>}
      {frame ? <FrameCanvas frame={frame} /> : previewUrl && <img src={previewUrl} alt="embedded preview" style={{ maxWidth: '100%' }} />}
    </main>
  );
}

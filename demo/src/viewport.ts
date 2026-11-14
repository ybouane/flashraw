// Zoom and pan for the image stage: wheel / pinch / drag / keyboard.
// State is in device pixels: `scale` = device pixels per image pixel, (x, y) = position of the
// image's top-left corner in the canvas.

export interface View {
  scale: number;
  x: number;
  y: number;
  /** canvas size in device pixels */
  cw: number;
  ch: number;
}

const MAX_SCALE = 32;

export class Viewport {
  view: View = { scale: 1, x: 0, y: 0, cw: 1, ch: 1 };
  private iw = 0;
  private ih = 0;
  private fitted = true;
  private pointers = new Map<number, { x: number; y: number }>();
  private pinch: { dist: number; scale: number; cx: number; cy: number } | null = null;
  private dragFrom: { x: number; y: number; vx: number; vy: number } | null = null;
  onChange: () => void = () => {};
  onInteract: () => void = () => {};

  constructor(private el: HTMLElement) {
    new ResizeObserver(() => this.resize()).observe(el);
    el.addEventListener('wheel', (e) => this.wheel(e), { passive: false });
    el.addEventListener('pointerdown', (e) => this.down(e));
    el.addEventListener('pointermove', (e) => this.move(e));
    el.addEventListener('pointerup', (e) => this.up(e));
    el.addEventListener('pointercancel', (e) => this.up(e));
    el.addEventListener('keydown', (e) => this.key(e));
    this.resize();
  }

  get dpr() {
    return window.devicePixelRatio || 1;
  }

  /**
   * Show an image of w × h. With `keep`, a zoomed view keeps its framing: the same part of the
   * picture stays in the same place at the same apparent size, also when the new image has
   * another resolution (embedded preview ↔ RAW).
   */
  setImage(w: number, h: number, keep = false) {
    const [ow, oh] = [this.iw, this.ih];
    this.iw = w;
    this.ih = h;
    if (!keep || !ow || !oh || this.fitted) return this.fit();
    const v = this.view;
    const cx = (v.cw / 2 - v.x) / v.scale / ow;
    const cy = (v.ch / 2 - v.y) / v.scale / oh;
    v.scale *= ow / w;
    v.x = v.cw / 2 - cx * w * v.scale;
    v.y = v.ch / 2 - cy * h * v.scale;
    this.fitted = Math.abs(v.scale - this.fitScale) < 1e-6;
    this.clamp();
    this.changed();
  }

  /** Forget the image: the next `setImage` fits (a new file). */
  clear() {
    this.iw = 0;
    this.ih = 0;
  }

  get fitScale() {
    if (!this.iw) return 1;
    return Math.min(this.view.cw / this.iw, this.view.ch / this.ih);
  }

  get isFitted() {
    return this.fitted;
  }

  fit() {
    this.fitted = true;
    const s = this.fitScale;
    this.view.scale = s;
    this.view.x = (this.view.cw - this.iw * s) / 2;
    this.view.y = (this.view.ch - this.ih * s) / 2;
    this.changed();
  }

  /** Zoom to `scale` keeping canvas point (px, py) (device px) fixed. */
  zoomTo(scale: number, px = this.view.cw / 2, py = this.view.ch / 2) {
    const min = Math.min(this.fitScale, 1) * 0.5;
    scale = Math.min(MAX_SCALE, Math.max(min, scale));
    const v = this.view;
    const ix = (px - v.x) / v.scale;
    const iy = (py - v.y) / v.scale;
    v.scale = scale;
    v.x = px - ix * scale;
    v.y = py - iy * scale;
    this.fitted = Math.abs(scale - this.fitScale) < 1e-6;
    this.clamp();
    this.changed();
  }

  zoomBy(f: number, px?: number, py?: number) {
    this.zoomTo(this.view.scale * f, px, py);
  }

  pan(dx: number, dy: number) {
    this.view.x += dx;
    this.view.y += dy;
    this.clamp();
    this.changed();
  }

  private clamp() {
    const v = this.view;
    const w = this.iw * v.scale;
    const h = this.ih * v.scale;
    v.x = w <= v.cw ? (v.cw - w) / 2 : Math.min(0, Math.max(v.cw - w, v.x));
    v.y = h <= v.ch ? (v.ch - h) / 2 : Math.min(0, Math.max(v.ch - h, v.y));
  }

  private resize() {
    const r = this.el.getBoundingClientRect();
    const cw = Math.max(1, Math.round(r.width * this.dpr));
    const ch = Math.max(1, Math.round(r.height * this.dpr));
    if (cw === this.view.cw && ch === this.view.ch) return;
    const v = this.view;
    const cx = (v.cw / 2 - v.x) / v.scale;
    const cy = (v.ch / 2 - v.y) / v.scale;
    v.cw = cw;
    v.ch = ch;
    if (this.fitted) this.fit();
    else {
      v.x = cw / 2 - cx * v.scale;
      v.y = ch / 2 - cy * v.scale;
      this.clamp();
      this.changed();
    }
  }

  private local(e: { clientX: number; clientY: number }) {
    const r = this.el.getBoundingClientRect();
    return { x: (e.clientX - r.left) * this.dpr, y: (e.clientY - r.top) * this.dpr };
  }

  private wheel(e: WheelEvent) {
    if (!this.iw) return;
    e.preventDefault();
    const p = this.local(e);
    const unit = e.deltaMode === 1 ? 33 : e.deltaMode === 2 ? 400 : 1;
    const k = e.ctrlKey ? 0.01 : 0.0022; // trackpad pinch arrives as ctrl+wheel
    this.zoomBy(Math.exp(-e.deltaY * unit * k), p.x, p.y);
    this.onInteract();
  }

  private down(e: PointerEvent) {
    if (e.button !== 0 && e.pointerType === 'mouse') return;
    this.el.setPointerCapture(e.pointerId);
    const p = this.local(e);
    this.pointers.set(e.pointerId, p);
    if (this.pointers.size === 1) {
      this.dragFrom = { x: p.x, y: p.y, vx: this.view.x, vy: this.view.y };
      this.el.classList.add('dragging');
    } else if (this.pointers.size === 2) {
      const [a, b] = [...this.pointers.values()];
      this.pinch = { dist: Math.hypot(a.x - b.x, a.y - b.y), scale: this.view.scale, cx: (a.x + b.x) / 2, cy: (a.y + b.y) / 2 };
      this.dragFrom = null;
    }
    this.onInteract();
  }

  private move(e: PointerEvent) {
    if (!this.pointers.has(e.pointerId)) return;
    const p = this.local(e);
    this.pointers.set(e.pointerId, p);
    if (this.pinch && this.pointers.size >= 2) {
      const [a, b] = [...this.pointers.values()];
      const d = Math.hypot(a.x - b.x, a.y - b.y);
      const cx = (a.x + b.x) / 2;
      const cy = (a.y + b.y) / 2;
      // pan with the pinch centre, zoom around it
      this.view.x += cx - this.pinch.cx;
      this.view.y += cy - this.pinch.cy;
      this.pinch.cx = cx;
      this.pinch.cy = cy;
      this.zoomTo((this.pinch.scale * d) / Math.max(1, this.pinch.dist), cx, cy);
    } else if (this.dragFrom) {
      this.view.x = this.dragFrom.vx + (p.x - this.dragFrom.x);
      this.view.y = this.dragFrom.vy + (p.y - this.dragFrom.y);
      this.clamp();
      this.changed();
    }
  }

  private up(e: PointerEvent) {
    this.pointers.delete(e.pointerId);
    if (this.pointers.size < 2) this.pinch = null;
    if (this.pointers.size === 1) {
      const p = [...this.pointers.values()][0];
      this.dragFrom = { x: p.x, y: p.y, vx: this.view.x, vy: this.view.y };
    } else if (!this.pointers.size) {
      this.dragFrom = null;
      this.el.classList.remove('dragging');
    }
  }

  private key(e: KeyboardEvent) {
    if (!this.iw || e.altKey || e.metaKey || e.ctrlKey) return;
    const step = 80 * this.dpr;
    const handled = (() => {
      switch (e.key) {
        case '+':
        case '=':
          this.zoomBy(1.25);
          return true;
        case '-':
        case '_':
          this.zoomBy(0.8);
          return true;
        case '0':
          this.fit();
          return true;
        case '1':
          this.zoomTo(1);
          return true;
        case 'ArrowLeft':
          this.pan(step, 0);
          return true;
        case 'ArrowRight':
          this.pan(-step, 0);
          return true;
        case 'ArrowUp':
          this.pan(0, step);
          return true;
        case 'ArrowDown':
          this.pan(0, -step);
          return true;
      }
      return false;
    })();
    if (handled) {
      e.preventDefault();
      this.onInteract();
    }
  }

  private changed() {
    this.onChange();
  }
}

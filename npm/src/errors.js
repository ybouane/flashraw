// Structured errors. Every error thrown by flashraw is a `FlashRawError` with a stable
// `kind` string (mirroring the Rust `flashraw::Error::kind()`), and an `instanceof`-able
// subclass per kind.

export class FlashRawError extends Error {
  /**
   * @param {string} message
   * @param {string} [kind]
   * @param {{ cause?: unknown }} [options]
   */
  constructor(message, kind = 'Internal', options) {
    super(message, options);
    this.name = 'FlashRawError';
    /** @type {string} */
    this.kind = kind;
  }
  toJSON() {
    return { name: this.name, kind: this.kind, message: this.message };
  }
}

function define(kind) {
  const name = `${kind}Error`;
  // Named class expression so stack traces and `err.name` read naturally.
  const cls = {
    [name]: class extends FlashRawError {
      constructor(message, options) {
        super(message, kind, options);
        this.name = name;
      }
    },
  }[name];
  return cls;
}

export const UnsupportedFormatError = define('UnsupportedFormat');
export const UnsupportedCompressionError = define('UnsupportedCompression');
export const CorruptFileError = define('CorruptFile');
export const InvalidMetadataError = define('InvalidMetadata');
export const CameraUnsupportedError = define('CameraUnsupported');
export const GpuUnavailableError = define('GpuUnavailable');
export const OutOfMemoryError = define('OutOfMemory');
export const LimitExceededError = define('LimitExceeded');
export const NotAvailableError = define('NotAvailable');
export const InvalidArgumentError = define('InvalidArgument');
export const IoError = define('Io');
export const ClosedError = define('Closed');
export const InternalError = define('Internal');

const BY_KIND = {
  UnsupportedFormat: UnsupportedFormatError,
  UnsupportedCompression: UnsupportedCompressionError,
  CorruptFile: CorruptFileError,
  InvalidMetadata: InvalidMetadataError,
  CameraUnsupported: CameraUnsupportedError,
  GpuUnavailable: GpuUnavailableError,
  OutOfMemory: OutOfMemoryError,
  LimitExceeded: LimitExceededError,
  NotAvailable: NotAvailableError,
  InvalidArgument: InvalidArgumentError,
  Io: IoError,
  Closed: ClosedError,
  Internal: InternalError,
};

export const ERROR_KINDS = Object.freeze(Object.keys(BY_KIND));

/** Build the error subclass for `kind` (unknown kinds become a plain `FlashRawError`). */
export function errorFromKind(kind, message, options) {
  const cls = BY_KIND[kind];
  return cls ? new cls(message, options) : new FlashRawError(message, kind, options);
}

/** Rebuild an error serialised with `toJSON()` (used across worker boundaries). */
export function errorFromJSON(o) {
  if (o && typeof o === 'object' && typeof o.kind === 'string') return errorFromKind(o.kind, String(o.message ?? ''));
  return new InternalError(String(o?.message ?? o));
}

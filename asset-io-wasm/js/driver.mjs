const U64_MAX = (1n << 64n) - 1n;

function checkSize(size) {
  if (typeof size !== "bigint" || size < 0n || size > U64_MAX) {
    throw new RangeError("size must be a bigint in the u64 range");
  }
}

/**
 * Drive Wasm with a host-native async readAt(offset: bigint, windowLen: number).
 * The source is immutable; readAt returns a Uint8Array. Gather partial reads in
 * readAt itself; a response shorter than minLen means unexpected EOF.
 */
export async function parseAsync(ParseTask, size, readAt, { format, signal } = {}) {
  checkSize(size);
  if (typeof readAt !== "function") {
    throw new TypeError("readAt must be a function");
  }
  if (format !== undefined && typeof format !== "string") {
    throw new TypeError("format must be a string or undefined");
  }
  signal?.throwIfAborted();
  const task = new ParseTask(size, format);
  try {
    for (;;) {
      signal?.throwIfAborted();
      const request = task.resume();
      if (request === undefined) {
        return JSON.parse(task.summaryJson());
      }
      const { offset, min_len: minLen, window_len: windowLen } = request;
      request.free();
      if (minLen <= 0 || minLen > windowLen || offset + BigInt(windowLen) > size) {
        throw new Error("invalid Wasm read descriptor");
      }
      let bytes;
      try {
        // Rust has returned; no Rust call or I/O callback is active during this await.
        bytes = await readAt(offset, windowLen);
        signal?.throwIfAborted();
        if (!(bytes instanceof Uint8Array)) {
          throw new TypeError("readAt must resolve to a Uint8Array");
        }
        if (bytes.length > windowLen) {
          throw new RangeError("readAt returned more than the requested window");
        }
      } catch (error) {
        task.fail(String(error));
        throw error;
      }
      task.supply(bytes);
    }
  } finally {
    task.free();
  }
}

/** Browser Blob/File adapter. Await Blob.arrayBuffer using the browser runtime. */
export function parseBlob(ParseTask, blob, options) {
  return parseAsync(ParseTask, BigInt(blob.size), async (offset, windowLen) => {
    if (offset > BigInt(Number.MAX_SAFE_INTEGER)) {
      throw new RangeError("Blob offset exceeds JavaScript's safe integer range");
    }
    const start = Number(offset);
    const end = start + windowLen;
    if (!Number.isSafeInteger(end)) {
      throw new RangeError("Blob range exceeds JavaScript's safe integer range");
    }
    return new Uint8Array(await blob.slice(start, end).arrayBuffer());
  }, options);
}

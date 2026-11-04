/** Async positioned reads using Node's filesystem, not WASI or Rust I/O. */
export async function readWindow(file, offset, windowLen) {
  // Node 18 positioned read APIs use Number offsets.
  if (offset < 0n || offset + BigInt(windowLen) > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new RangeError("file range exceeds JavaScript's safe integer range");
  }
  const bytes = new Uint8Array(windowLen);
  let len = 0;
  while (len < windowLen) {
    const { bytesRead } = await file.read(bytes, len, windowLen - len, Number(offset) + len);
    if (bytesRead === 0) break;
    len += bytesRead;
  }
  return bytes.subarray(0, len);
}

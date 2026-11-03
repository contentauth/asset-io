import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { open, readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { setImmediate as nextTurn } from "node:timers/promises";
import test from "node:test";

import bindings from "../pkg/asset_io_wasm.js";
import { parseAsync, parseBlob } from "./driver.mjs";
import { readWindow } from "./node-source.mjs";

const { ParseTask } = bindings;
const root = fileURLToPath(new URL("../../", import.meta.url));
const oracle = fileURLToPath(new URL("../target/debug/examples/inspect", import.meta.url));
const jpeg = Uint8Array.of(0xff, 0xd8, 0xff, 0xd9);

for (const [name, format] of [["Designer.jpeg", undefined], ["GreenCat.png", "png"]]) {
  test(`native JS async file reads match blocking Rust: ${name}`, async () => {
    const path = `${root}tests/fixtures/${name}`;
    const expected = JSON.parse(execFileSync(oracle, format ? [path, format] : [path], {
      encoding: "utf8",
    }));
    const file = await open(path, "r");
    try {
      const { size } = await file.stat({ bigint: true });
      let reads = 0;
      let eventLoopTurns = 0;
      let active = false;
      const actual = await parseAsync(ParseTask, size, async (offset, windowLen) => {
        assert.equal(active, false, "one outstanding read per parser");
        active = true;
        assert.equal(typeof offset, "bigint");
        assert.ok(offset >= 0n && offset + BigInt(windowLen) <= size);
        reads++;
        // A macrotask must run while the parser is suspended, not just a
        // Promise.resolve microtask masquerading as async I/O.
        await nextTurn();
        eventLoopTurns++;
        const bytes = await readWindow(file, offset, windowLen);
        active = false;
        return bytes;
      }, { format });
      assert.ok(reads > 1, "exercise multiple Wasm suspensions");
      assert.equal(eventLoopTurns, reads);
      assert.deepEqual(actual, expected);
      console.log(`PASS: ${name}: ${reads} async reads / event-loop turns; native summary matches`);
    } finally {
      await file.close();
    }
  });
}

test("Blob adapter uses async arrayBuffer and matches the file adapter", async () => {
  const data = await readFile(`${root}tests/fixtures/GreenCat.png`);
  const expected = await parseAsync(ParseTask, BigInt(data.length), async (offset, len) => {
    await nextTurn();
    return data.subarray(Number(offset), Number(offset) + len);
  });
  assert.deepEqual(await parseBlob(ParseTask, new Blob([data])), expected);
});

test("separate parsers interleave on JavaScript's event loop", async () => {
  const seen = [];
  async function drive(id) {
    return parseAsync(ParseTask, 4n, async (offset, len) => {
      seen.push(id);
      await nextTurn();
      return jpeg.slice(Number(offset), Number(offset) + len);
    });
  }
  const [a, b] = await Promise.all([drive("a"), drive("b")]);
  assert.deepEqual(a, b);
  assert.deepEqual(seen.slice(0, 2), ["a", "b"]);
  assert.ok(seen.length >= 4, "detection and parsing each suspend");
});

test("pending reads are stable, supplied bytes are owned, and misuse throws", () => {
  const task = new ParseTask(4n);
  try {
    assert.throws(() => task.summaryJson(), /not completed/);
    assert.throws(() => task.supply(jpeg), /no outstanding read/);
    assert.throws(() => task.fail("early"), /no outstanding read/);
    let req = task.resume();
    const descriptor = [req.offset, req.min_len, req.window_len];
    req.free();
    req = task.resume();
    assert.deepEqual([req.offset, req.min_len, req.window_len], descriptor);
    req.free();
    assert.throws(() => task.supply(new Uint8Array(5)), /exceeds requested window/);
    for (;;) {
      req = task.resume();
      if (req === undefined) break;
      const response = jpeg.slice(Number(req.offset), Number(req.offset) + req.window_len);
      req.free();
      task.supply(response);
      response.fill(0);
    }
    assert.equal(JSON.parse(task.summaryJson()).container, "jpeg");
    assert.equal(task.resume(), undefined);
    assert.throws(() => task.supply(jpeg), /no outstanding read/);
  } finally {
    task.free();
  }
});

test("host failures are terminal and preserve their message", () => {
  const task = new ParseTask(4n);
  try {
    task.resume().free();
    task.fail("native JS read rejected");
    assert.throws(() => task.resume(), /native JS read rejected/);
    assert.throws(() => task.resume(), /native JS read rejected/);
    assert.throws(() => task.summaryJson(), /not completed/);
  } finally {
    task.free();
  }
});

test("read rejection, EOF, malformed data, and oversized/wrong responses reject", async () => {
  const failure = new Error("filesystem failure");
  await assert.rejects(parseAsync(ParseTask, 4n, async () => { throw failure; }),
    (error) => error === failure);
  await assert.rejects(parseAsync(ParseTask, 4n, async () => new Uint8Array()),
    /failed to fill whole buffer/);
  await assert.rejects(parseAsync(ParseTask, 4n, async () => Uint8Array.of(1, 2, 3, 4)));
  await assert.rejects(parseAsync(ParseTask, 4n, async () => new Uint8Array(5)), /more than/);
  await assert.rejects(parseAsync(ParseTask, 4n, async () => [1, 2, 3, 4]), /Uint8Array/);
  await assert.rejects(parseAsync(ParseTask, 4n, async () => jpeg, { format: "unsupported" }));
});

test("invalid sizes are rejected before invoking Wasm or I/O", async () => {
  for (const size of [-1n, 1n << 64n, 4, NaN]) {
    await assert.rejects(parseAsync(ParseTask, size, () => {
      assert.fail("invalid size must not read");
    }), /size must be a bigint/);
  }
  await assert.rejects(parseAsync(ParseTask, 4n, undefined), /readAt must be/);
  await assert.rejects(parseAsync(ParseTask, 4n, async () => jpeg, { format: 42 }),
    /format must be/);
  await assert.rejects(readWindow(null, BigInt(Number.MAX_SAFE_INTEGER), 2), /safe integer/);
});

test("AbortSignal cancels before starting and after host I/O settles", async () => {
  const alreadyAborted = new AbortController();
  alreadyAborted.abort();
  await assert.rejects(parseAsync(ParseTask, 4n, () => {
    assert.fail("already aborted task must not read");
  }, { signal: alreadyAborted.signal }), { name: "AbortError" });

  let freed = 0;
  class TrackedTask extends ParseTask {
    free() {
      freed++;
      super.free();
    }
  }
  const controller = new AbortController();
  let settled = false;
  await assert.rejects(parseAsync(TrackedTask, 4n, async () => {
    controller.abort();
    await nextTurn();
    assert.equal(freed, 0, "wait for host completion before freeing");
    settled = true;
    return jpeg;
  }, { signal: controller.signal }), { name: "AbortError" });
  assert.equal(settled, true);
  assert.equal(freed, 1);
});

test("module uses no WASI filesystem or Rust async runtime", async () => {
  const wasm = await readFile(new URL("../pkg/asset_io_wasm_bg.wasm", import.meta.url));
  const module = await WebAssembly.compile(wasm);
  const imports = WebAssembly.Module.imports(module);
  assert.equal(imports.some(({ module }) => module.startsWith("wasi")), false);
});

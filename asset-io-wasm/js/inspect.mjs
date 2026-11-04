import { open } from "node:fs/promises";
import bindings from "../pkg/asset_io_wasm.js";
import { parseAsync } from "./driver.mjs";
import { readWindow } from "./node-source.mjs";

const [path, format] = process.argv.slice(2);
if (!path) throw new Error("usage: node asset-io-wasm/js/inspect.mjs FILE [FORMAT]");
const file = await open(path, "r");
try {
  const { size } = await file.stat({ bigint: true });
  let reads = 0;
  const summary = await parseAsync(bindings.ParseTask, size, async (offset, len) => {
    reads++;
    return readWindow(file, offset, len);
  }, { format });
  console.error(`${reads} JavaScript async read completions`);
  console.log(JSON.stringify(summary, null, 2));
} finally {
  await file.close();
}

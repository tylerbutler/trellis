import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { unstable_readConfig } from "wrangler";

test("Wrangler configuration enables Workers previews", () => {
  const config = unstable_readConfig({
    config: fileURLToPath(new URL("../wrangler.jsonc", import.meta.url)),
  });
  assert.notEqual(config.previews, undefined, "wrangler preview requires a previews block");
});

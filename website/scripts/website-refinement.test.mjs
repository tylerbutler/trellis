import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { runInNewContext } from "node:vm";

const homepage = readFileSync(new URL("../src/pages/index.astro", import.meta.url), "utf8");
const reference = readFileSync(
  new URL("../src/content/docs/docs/configuration-reference.mdx", import.meta.url),
  "utf8",
);
const script = homepage.match(/<script>([\s\S]+)<\/script>/)[1]
  .replaceAll("<HTMLButtonElement>", "");

function copy_state(writeText, with_selection = true) {
  let click;
  let reset;
  const selected = [];
  const prompt = {};
  const code = {
    querySelector: () => prompt,
    focus: () => selected.push("focused"),
  };
  const status = { textContent: "", classList: { contains: () => true } };
  const button = {
    dataset: { copy: "brew install tylerbutler/tap/trellis" },
    textContent: "Copy",
    parentElement: { querySelector: () => code, nextElementSibling: status },
    addEventListener: (event, handler) => {
      assert.equal(event, "click");
      click = handler;
    },
  };
  runInNewContext(script, {
    document: {
      querySelectorAll: () => [button],
      createRange: () => ({
        selectNodeContents: (node) => assert.equal(node, code),
        setStartAfter: (node) => assert.equal(node, prompt),
      }),
    },
    window: {
      getSelection: () => with_selection ? {
        removeAllRanges: () => selected.push("cleared"),
        addRange: () => selected.push("selected"),
      } : null,
    },
    navigator: { clipboard: { writeText } },
    setTimeout: (handler, delay) => {
      assert.equal(delay, 1600);
      reset = handler;
    },
  });
  return { button, code, status, selected, click: () => click(), reset: () => reset() };
}

test("copy confirms the exact command and resets success feedback", async () => {
  const copied = [];
  const state = copy_state(async (text) => copied.push(text));
  await state.click();
  assert.deepEqual(copied, ["brew install tylerbutler/tap/trellis"]);
  assert.equal(state.button.textContent, "Copied");
  assert.equal(state.button.dataset.copied, "true");
  state.reset();
  assert.equal(state.button.textContent, "Copy");
  assert.equal(state.button.dataset.copied, undefined);
});

test("clipboard failure selects and focuses the command without its prompt", async () => {
  const state = copy_state(async () => { throw new Error("permission denied"); });
  await state.click();
  assert.equal(state.button.textContent, "Copy failed");
  assert.deepEqual(state.selected, ["cleared", "selected", "focused"]);
  assert.equal(state.code.tabIndex, -1);
  assert.match(state.status.textContent, /selected.*copy it manually/i);
});

test("clipboard failure explains manual recovery when selection is unavailable", async () => {
  const state = copy_state(async () => { throw new Error("unavailable"); }, false);
  await state.click();
  assert.deepEqual(state.selected, []);
  assert.match(state.status.textContent, /select the command.*copy it manually/i);
});

test("homepage adoption and installation links lead to the existing guides", () => {
  const adoption = homepage.match(/<ol class="adopt-paths">([\s\S]+?)<\/ol>/)[1];
  for (const guide of ["task-running", "changelog", "publishing", "ci"]) {
    assert.ok(adoption.includes(`href="/docs/${guide}/"`), `missing adoption guide: ${guide}`);
  }
  assert.ok(!homepage.includes('class="lifecycle"'));
  for (const section of ["powershell-installer", "pinning-a-version-in-ci"]) {
    assert.ok(homepage.includes(`href="/docs/installation/#${section}"`));
  }
});

test("every configuration key has a unique direct link in a grouped table", () => {
  const key_section = reference.split("\n## Release preparation\n")[0];
  const keys = [...key_section.matchAll(/^\| (.+?) \| (?:no|with the table|with the other two) \|/gm)];
  assert.equal(keys.length, 32, "retain the complete key inventory");
  const ids = [...reference.matchAll(/<a id="([^"]+)" href="#\1">/g)].map(([, id]) => id);
  assert.equal(ids.length, keys.length);
  assert.equal(new Set(ids).size, ids.length);
  for (const [, key] of keys) assert.match(key, /<a id=/);
  for (const group of ["Workspace and tasks", "Manifest adapter", "Package publishing", "Lifecycle settings", "Changelogs", "Doctor checks", "Release preparation settings"]) {
    assert.ok(reference.includes(`### ${group}\n`), `missing key group: ${group}`);
  }
});

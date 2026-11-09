import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { runInNewContext } from "node:vm";

const homepage = readFileSync(new URL("../src/pages/index.astro", import.meta.url), "utf8");
const reference = readFileSync(
  new URL("../src/content/docs/docs/configuration-reference.mdx", import.meta.url),
  "utf8",
);
const tokens = readFileSync(new URL("../src/styles/tokens.css", import.meta.url), "utf8");
const global_css = readFileSync(new URL("../src/styles/global.css", import.meta.url), "utf8");
const docs_css = readFileSync(new URL("../src/styles/starlight.css", import.meta.url), "utf8");
const design = readFileSync(new URL("../../DESIGN.md", import.meta.url), "utf8");
const sidecar = JSON.parse(readFileSync(
  new URL("../../.impeccable/design.json", import.meta.url),
  "utf8",
));
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

test("the statement band uses the section-heading type role", () => {
  const statement = homepage.match(/\.doctrine p\s*\{([^}]+)\}/)[1];
  const section = homepage.match(/\.section-head h2\s*\{([^}]+)\}/)[1];
  const heading = global_css.match(/h1,\s*h2,\s*h3,\s*h4\s*\{([^}]+)\}/)[1];
  const value = (css, property) => css.match(new RegExp(`${property}:\\s*([^;]+);`))[1];
  for (const property of ["font-size", "font-stretch"]) {
    assert.equal(value(statement, property), value(section, property), property);
  }
  for (const property of ["font-weight", "line-height", "letter-spacing"]) {
    assert.equal(value(statement, property), value(heading, property), property);
  }
});

test("docs inherit the shared font stacks instead of redefining them", () => {
  assert.match(docs_css, /--sl-font:\s*var\(--font-sans\);/);
  assert.match(docs_css, /--sl-font-mono:\s*var\(--font-mono\);/);
});

test("design typography and sidecar metadata match the shipped tokens", () => {
  const css_tokens = new Map([...tokens.matchAll(/^\s*--([\w-]+):\s*(.+);/gm)]
    .map(([, name, value]) => [name, value]));
  const type = design.match(/\ntypography:\n([\s\S]+?)\nrounded:/)[1];
  const roles = new Map([...type.matchAll(/  ([\w-]+):\n([\s\S]+?)(?=\n  [\w-]+:|$)/g)]
    .map(([, name, properties]) => [name, properties]));
  const sizes = {
    display: "text-display",
    headline: "text-h2",
    title: "text-h3",
    "docs-h1": "text-docs-h1",
    "docs-h2": "text-docs-h2",
    "docs-h3": "text-docs-h3",
    wordmark: "text-wordmark",
    body: "text-body",
    lead: "text-lead",
    small: "text-small",
    label: "text-fine",
    code: "text-code",
  };
  for (const [role, size] of Object.entries(sizes)) {
    const properties = roles.get(role);
    assert.ok(properties, `missing type role: ${role}`);
    assert.equal(properties.match(/fontSize: "([^"]+)"/)[1], css_tokens.get(size), role);
    const family = properties.match(/fontFamily: "([^"]+)"/)[1];
    const stack = css_tokens.get(["label", "code"].includes(role) ? "font-mono" : "font-sans");
    assert.equal(family, stack.replaceAll('"', ""), role);
    assert.ok(sidecar.extensions.typographyMeta[role], `missing sidecar type role: ${role}`);
  }
  assert.equal(sidecar.schemaVersion, 2);
  for (const [name, color] of Object.entries(sidecar.extensions.colorMeta)) {
    assert.equal(color.canonical, css_tokens.get(name), name);
  }
  assert.ok(!design.includes("Martian Mono"));
  assert.ok(!JSON.stringify(sidecar).includes("Martian Mono"));
  const overview = design.match(/## Overview[\s\S]+?\*\*Creative North Star:[^\n]+\n\n([\s\S]+?)\n\n\*\*Key Characteristics:/)[1];
  assert.equal(sidecar.narrative.overview, overview);
  const bullets = (section) => section.split("\n").filter(line => line.startsWith("- "))
    .map(line => line.slice(2).replaceAll("**", ""));
  assert.deepEqual(sidecar.narrative.keyCharacteristics,
    bullets(design.split("**Key Characteristics:**")[1].split("## Colors")[0]));
  assert.deepEqual(sidecar.narrative.dos,
    bullets(design.split("### Do:")[1].split("### Don't:")[0]));
  assert.deepEqual(sidecar.narrative.donts, bullets(design.split("### Don't:")[1]));
  for (const [, name, body] of design.matchAll(/\*\*(The [^\n]*? Rule)\.\*\* ([^\n]+)/g)) {
    assert.equal(sidecar.narrative.rules.find(rule => rule.name === name)?.body, body, name);
  }
});

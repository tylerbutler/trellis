import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { runInNewContext } from "node:vm";

const script = readFileSync(
  new URL("../src/components/ConfigurationReferenceRedirect.astro", import.meta.url),
  "utf8",
).match(/<script>([\s\S]+)<\/script>/)[1];
const guide = readFileSync(
  new URL("../src/content/docs/docs/configuration.mdx", import.meta.url),
  "utf8",
);
const reference = readFileSync(
  new URL("../src/content/docs/docs/configuration-reference.mdx", import.meta.url),
  "utf8",
);
const reference_links = [...guide.matchAll(/<a id="([^"]+)" data-reference href="([^"]+)"/g)];

function run_redirect(hash) {
  class anchor {
    constructor(href) {
      this.href = new URL(href, "https://trellis.tylerbutler.com").href;
    }
    hasAttribute(name) {
      return name === "data-reference";
    }
  }
  const links = new Map(reference_links.map(([, id, href]) => [id, new anchor(href)]));
  links.set("member-discovery", {});
  const destinations = [];
  let hash_change;
  const window = {
    location: { hash, replace: (url) => destinations.push(url) },
    addEventListener: (event, handler) => {
      assert.equal(event, "hashchange");
      hash_change = handler;
    },
  };
  runInNewContext(script, {
    window,
    document: { getElementById: (id) => links.get(id) ?? null },
    HTMLAnchorElement: anchor,
  });
  return { window, destinations, hash_change };
}

test("moved configuration anchors redirect to existing reference sections", () => {
  const headings = [...reference.matchAll(/^#{2,3} (.+)$/gm)]
    .map(([, text]) => text.toLowerCase().replaceAll(" ", "-"));
  assert.equal(reference_links.length, 8);
  for (const [, id, href] of reference_links) {
    const expected = new URL(href, "https://trellis.tylerbutler.com");
    assert.ok(headings.includes(expected.hash.slice(1)), `missing section: ${href}`);
    assert.deepEqual(run_redirect(`#${id}`).destinations, [expected.href]);
  }
});

test("guide anchors, empty hashes, and unknown hashes stay on the guide", () => {
  for (const hash of ["", "#member-discovery", "#unknown", "#%invalid"]) {
    assert.deepEqual(run_redirect(hash).destinations, []);
  }
});

test("hash changes also follow moved sections", () => {
  const state = run_redirect("#member-discovery");
  state.window.location.hash = "#tags";
  state.hash_change();
  assert.deepEqual(state.destinations, [
    "https://trellis.tylerbutler.com/docs/configuration-reference/#tags",
  ]);
});

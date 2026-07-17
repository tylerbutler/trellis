/**
 * Generates public/og.png (1200×630) for social sharing cards.
 *
 * Derive before declaring, applied to the image pipeline itself:
 * - Colors are parsed from src/styles/tokens.css (the same OKLCH tokens the
 *   site ships) and converted to hex with culori.
 * - Fonts are instanced at the exact DESIGN.md axis values (Archivo
 *   'wdth' 118 / wght 680, Martian Mono wght 560) from the same
 *   @fontsource-variable woff2 files the site serves, via HarfBuzz.
 * - The lattice glyph is the committed dark-scheme favicon mark.
 *
 * Run: pnpm og
 */

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { formatHex, parse } from "culori";
import satori from "satori";
import sharp from "sharp";
import subsetFont from "subset-font";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

// ---- colors: parsed from the shipped tokens ----

const tokensCss = readFileSync(join(root, "src/styles/tokens.css"), "utf8");

function token(name: string): string {
  const match = tokensCss.match(new RegExp(`--${name}:\\s*([^;]+);`));
  if (!match) throw new Error(`token --${name} not found in tokens.css`);
  const color = parse(match[1].trim());
  if (!color) throw new Error(`token --${name} is not a parsable color`);
  return formatHex(color);
}

const bg = token("bg");
const ink = token("ink");
const inkSoft = token("ink-soft");
const muted = token("muted");
const brass = token("brass");
const lineSoft = token("line-soft");

// ---- copy: mirrors the landing-page hero ----

const headlinePlain = "Tools for Gleam monorepos.";
const headlineBrass = "Derived from gleam.toml.";
const wordmark = "trellis";
const command = "trellis doctor";
const domain = "trellis.tylerbutler.com";

// ---- fonts: instanced from the site's own variable fonts ----

async function instance(
  fontsourcePath: string,
  text: string,
  axes: Record<string, number>,
): Promise<Buffer> {
  const woff2 = readFileSync(join(root, "node_modules", fontsourcePath));
  return subsetFont(woff2, text, {
    targetFormat: "truetype",
    variationAxes: axes,
  });
}

const archivoText = headlinePlain + headlineBrass + wordmark;
const monoText = `$ ${command}${domain}`;

const [archivoDisplay, archivoWordmark, martianMono] = await Promise.all([
  instance(
    "@fontsource-variable/archivo/files/archivo-latin-wdth-normal.woff2",
    archivoText,
    { wght: 680, wdth: 118 },
  ),
  instance(
    "@fontsource-variable/archivo/files/archivo-latin-wdth-normal.woff2",
    archivoText,
    { wght: 640, wdth: 118 },
  ),
  instance(
    "@fontsource-variable/martian-mono/files/martian-mono-latin-wght-normal.woff2",
    monoText,
    { wght: 560 },
  ),
]);

// ---- lattice glyph: the favicon mark, dark-scheme colors ----

const glyphSvg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <rect fill="#172833" width="32" height="32" rx="7"/>
  <path stroke="#57a8ce" d="M6 12.5 12.5 6M6 19.5 19.5 6M6 26 26 6M12.5 26 26 12.5M19.5 26 26 19.5" stroke-width="2" stroke-linecap="round"/>
  <path stroke="#f0c26a" d="M12.5 6 26 19.5M6 12.5 19.5 26" stroke-width="2" stroke-linecap="round"/>
</svg>`;
const glyphUri = `data:image/svg+xml;base64,${Buffer.from(glyphSvg).toString("base64")}`;

// ---- layout ----

type Node = {
  type: string;
  props: Record<string, unknown> & { children?: Node[] | string };
};

function h(
  type: string,
  style: Record<string, unknown>,
  children?: Node[] | string,
): Node {
  return { type, props: { style, children } };
}

const width = 1200;
const height = 630;
const pad = 72;

const card = h(
  "div",
  {
    width,
    height,
    display: "flex",
    flexDirection: "column",
    backgroundColor: bg,
  },
  [
    // brass band: the single accent voice, machined flat
    h("div", { height: 10, backgroundColor: brass }),
    h(
      "div",
      {
        flexGrow: 1,
        display: "flex",
        flexDirection: "column",
        padding: `54px ${pad}px 48px`,
      },
      [
        h("div", { display: "flex", alignItems: "center", gap: 20 }, [
          {
            type: "img",
            props: { src: glyphUri, width: 54, height: 54, style: {} },
          },
          h(
            "div",
            {
              fontFamily: "Archivo Wordmark",
              fontSize: 36,
              color: ink,
              letterSpacing: "-0.01em",
            },
            wordmark,
          ),
        ]),
        h(
          "div",
          {
            display: "flex",
            flexDirection: "column",
            marginTop: 44,
            maxWidth: 1010,
            fontFamily: "Archivo Display",
            fontSize: 79,
            lineHeight: 1.14,
            letterSpacing: "-0.022em",
          },
          [
            h("div", { color: ink }, headlinePlain),
            h("div", { color: brass }, headlineBrass),
          ],
        ),
        h("div", { flexGrow: 1 }),
        h("div", { height: 2, backgroundColor: lineSoft }),
        h(
          "div",
          {
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            marginTop: 30,
            fontFamily: "Martian Mono",
            fontSize: 23,
          },
          [
            h("div", { display: "flex", color: inkSoft }, [
              h("div", { color: brass, marginRight: 16 }, "$"),
              h("div", {}, command),
            ]),
            h("div", { color: muted }, domain),
          ],
        ),
      ],
    ),
  ],
);

// ---- render: satori (text → paths) then sharp (svg → png) ----

const svg = await satori(card, {
  width,
  height,
  fonts: [
    { name: "Archivo Display", data: archivoDisplay, weight: 680 },
    { name: "Archivo Wordmark", data: archivoWordmark, weight: 640 },
    { name: "Martian Mono", data: martianMono, weight: 560 },
  ],
});

const png = await sharp(Buffer.from(svg), { density: 144 })
  .resize(width, height)
  .png({ compressionLevel: 9 })
  .toBuffer();

const out = join(root, "public/og.png");
writeFileSync(out, png);
console.log(`wrote ${out} (${width}×${height}, ${(png.length / 1024).toFixed(1)} KiB)`);

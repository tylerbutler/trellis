---
name: Trellis
description: A Gleam workspace manager — landing page and documentation site
colors:
  bg: "oklch(0.16 0.02 230)"
  surface: "oklch(0.205 0.026 230)"
  surface-2: "oklch(0.25 0.03 230)"
  cobalt: "oklch(0.45 0.086 230)"
  cobalt-deep: "oklch(0.33 0.06 230)"
  cobalt-band: "oklch(0.28 0.045 230)"
  ink: "oklch(0.94 0.012 230)"
  ink-soft: "oklch(0.88 0.02 230)"
  muted: "oklch(0.76 0.022 230)"
  brass: "oklch(0.8 0.128 80)"
  brass-bright: "oklch(0.86 0.11 85)"
  ink-dark: "oklch(0.18 0.03 240)"
  line: "oklch(0.34 0.03 230)"
  line-soft: "oklch(0.27 0.028 230)"
typography:
  display:
    fontFamily: "Archivo Variable, Helvetica Neue, Arial, sans-serif"
    fontSize: "clamp(2.3rem, 1.1rem + 4.6vw, 4.4rem)"
    fontWeight: 680
    lineHeight: 1.12
    letterSpacing: "-0.022em"
    fontVariation: "'wdth' 118"
  headline:
    fontFamily: "Archivo Variable, Helvetica Neue, Arial, sans-serif"
    fontSize: "clamp(1.6rem, 1.05rem + 2.1vw, 2.5rem)"
    fontWeight: 620
    lineHeight: 1.12
    letterSpacing: "-0.015em"
    fontVariation: "'wdth' 114"
  title:
    fontFamily: "Archivo Variable, Helvetica Neue, Arial, sans-serif"
    fontSize: "1.3rem"
    fontWeight: 620
    lineHeight: 1.2
  docs-h1:
    fontFamily: "Archivo Variable, Helvetica Neue, Arial, sans-serif"
    fontSize: "clamp(2rem, 1.4rem + 2vw, 2.8rem)"
    fontWeight: 620
    lineHeight: 1.12
    fontVariation: "'wdth' 114"
  docs-h2:
    fontFamily: "Archivo Variable, Helvetica Neue, Arial, sans-serif"
    fontSize: "1.45rem"
    fontWeight: 620
    lineHeight: 1.12
  docs-h3:
    fontFamily: "Archivo Variable, Helvetica Neue, Arial, sans-serif"
    fontSize: "1.1rem"
    fontWeight: 620
    lineHeight: 1.2
  wordmark:
    fontFamily: "Archivo Variable, Helvetica Neue, Arial, sans-serif"
    fontSize: "1.2rem"
    fontWeight: 640
    fontVariation: "'wdth' 118"
  body:
    fontFamily: "Archivo Variable, Helvetica Neue, Arial, sans-serif"
    fontSize: "1.0625rem"
    fontWeight: 400
    lineHeight: 1.65
    letterSpacing: "0.011em"
  label:
    fontFamily: "Martian Mono Variable, ui-monospace, SF Mono, Menlo, monospace"
    fontSize: "0.8125rem"
    fontWeight: 560
  code:
    fontFamily: "Martian Mono Variable, ui-monospace, SF Mono, Menlo, monospace"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.75
rounded:
  sm: "4px"
  md: "8px"
spacing:
  2xs: "0.25rem"
  xs: "0.5rem"
  sm: "0.75rem"
  md: "1rem"
  lg: "1.5rem"
  xl: "2.5rem"
  2xl: "4rem"
  section: "clamp(5rem, 6rem + 4vw, 9rem)"
components:
  button-primary:
    backgroundColor: "{colors.brass}"
    textColor: "{colors.ink-dark}"
    rounded: "{rounded.sm}"
    padding: "0.6rem 1.4rem"
  button-primary-hover:
    backgroundColor: "{colors.brass-bright}"
    textColor: "{colors.ink-dark}"
  button-ghost:
    textColor: "{colors.ink}"
    rounded: "{rounded.sm}"
    padding: "0.6rem 1.4rem"
  button-ghost-hover:
    textColor: "{colors.brass-bright}"
---

# Design System: Trellis

## 1. Overview

**Creative North Star: "The Calibrated Instrument"**

Trellis is a tool you trust the way you trust a well-made instrument: it derives its answers instead of asking you to declare them, and it verifies everything it cannot derive. The site carries the same character — brass dials on oxidized steel. The page surface is a deep harbor-steel blue, structural panels step up through tonal cobalt layers, and a single warm brass voice marks what matters: the primary action, the active nav item, the `$` prompt, the `ok:` line in real terminal output. Typography pairs Archivo (stretched wide for display, plain for prose) with Martian Mono, which has a register reason: the product IS a CLI, and its real, unretouched output is the site's primary imagery.

Density is calm but committed. Sections hold one idea each; ledger tables and terminal figures do the persuading; motion is limited to state changes with one exponential ease. The system explicitly rejects the generic SaaS landing page (gradient heroes, floating product screenshots, pricing-page energy), the sterile default docs theme (out-of-the-box Starlight with no identity), and the Linear/Vercel dark-glow lane (near-black surfaces, glowing gradients, luminous blur). Its living references are gleam.run's hand-made friendliness and axo.dev's proof that a small CLI tool can have a real designed identity.

**Key Characteristics:**
- Committed color: steel cobalt carries the surface; brass is the single accent voice
- Real terminal output as imagery — never fabricated, never prettified
- Documentation styled with landing-page care; docs ARE the conversion surface
- Flat, machined, exact: depth from tonal steps and 1px edges, never shadows or glow

## 2. Colors: The Harbor Steel Palette

A committed steel-cobalt system anchored at hue 230, with one warm brass accent at hue 80 — the instrument-on-steel tension.

### Primary
- **Harbor Steel** (`{colors.bg}`): the page itself. A deep blue-tinted steel, not neutral black — the brand environment, present on every screen.
- **Instrument Cobalt** (`{colors.cobalt}`): the seed color. Panel headers, selection highlights, structural emphasis on the steel field.
- **Cobalt Band** (`{colors.cobalt-band}`) and **Cobalt Deep** (`{colors.cobalt-deep}`): full-width statement bands and deep structural fills; the committed 30–60% coverage lives here.

### Secondary
- **Signal Brass** (`{colors.brass}`): the single accent voice. Primary buttons, links, active nav, prompts, key table cells, the `ok:` line. **Brass Bright** (`{colors.brass-bright}`) is its hover state only.

### Neutral
- **Chart Ink** (`{colors.ink}`): body text. Verified 16.3:1 against Harbor Steel.
- **Ink Soft** (`{colors.ink-soft}`): lead paragraphs and secondary prose.
- **Fog Gray** (`{colors.muted}`): metadata, table notes, captions. Verified 9.1:1 on the background — never drops below AA.
- **Ink Dark** (`{colors.ink-dark}`): text on brass fills only. Verified 9.9:1 on Signal Brass.
- **Line / Line Soft** (`{colors.line}`, `{colors.line-soft}`): 1px borders and dividers. All structure is drawn with these hairlines.

### Named Rules
**The Committed Surface Rule.** Steel cobalt is not an accent — the surface IS the brand. Hedging back to a white page with a blue button produces exactly the sterile default docs theme this brand rejects.

**The No-Glow Rule.** No gradient glows, no neon edges, no luminous blur behind UI. Emphasis comes from brass, weight, and exact alignment — never from light effects.

**The Ink-on-Cobalt Rule.** Fog Gray fails contrast on cobalt fills (3.4:1). On any cobalt surface, text is Chart Ink or Ink Soft — never muted gray.

## 3. Typography

**Display Font:** Archivo Variable (with Helvetica Neue fallback), width axis stretched to 114–118% for display and headlines
**Body Font:** Archivo Variable at normal width
**Label/Mono Font:** Martian Mono Variable (with ui-monospace fallback)

**Character:** One grotesque with committed width/weight contrast, plus a wide technical mono. Archivo speaks; Martian Mono proves. The pairing contrasts on the proportion axis — expanded display against fixed-pitch evidence.

### Hierarchy
- **Display** (680, `clamp(2.3rem → 4.4rem)`, 1.12, 'wdth' 118): hero headline only. The brass sentence inside it is the page's one color-emphasis moment.
- **Headline** (620, `clamp(1.6rem → 2.5rem)`, 1.12, 'wdth' 114): section headings.
- **Title** (620, 1.3rem): phase and panel headings.
- **Body** (400, 1.0625rem, 1.65): prose, capped at 68ch measure (`--measure`).
- **Label** (560, 0.8125rem, Martian Mono): table headers, terminal title bars, fine print.
- **Code** (400, 0.875rem, 1.75, Martian Mono): code samples and terminal output; ligatures disabled globally.

### Named Rules
**The Proof-in-Mono Rule.** Terminal output shown on the site is real trellis output, set in the mono, unretouched. Never fabricate or prettify command output.

**The Two-Voices Rule.** Archivo carries meaning, Martian Mono carries evidence. Mono never sets prose; Archivo never sets output.

## 4. Elevation

Flat by default, tonal by exception. The system uses **zero box-shadows** — depth is conveyed by stepping surface lightness (Harbor Steel → Surface → Surface-2 → Cobalt Band) and by 1px hairline borders (`{colors.line}`, `{colors.line-soft}`). Panels read as machined plates laid on the steel field, not as floating cards. If an element looks like it's hovering, it's off-brand.

### Named Rules
**The Flat Field Rule.** Surfaces are flat at rest and flat on hover. State changes recolor (brass, background tint) — they never lift, scale, or cast.

## 5. Components

Machined and quiet: flat faces, exact 1px edges, small radii (4px controls, 8px panels), instant-feeling 160ms state changes on one exponential ease (`cubic-bezier(0.22, 1, 0.36, 1)`).

### Buttons
- **Shape:** small radius (`{rounded.sm}`), min-height 2.9rem, weight 620
- **Primary:** Signal Brass fill with Ink Dark text (`button-primary`); hover shifts to Brass Bright. No transform, no shadow.
- **Ghost:** transparent with `{colors.line}` border and Chart Ink text; hover recolors border and text toward brass.
- **Focus:** 2px Signal Brass outline, 3px offset — everywhere, keyboard-visible.

### Copy Command Row (signature)
A flex row: `$`-prompted mono command on a Surface fill, hairline border, with an attached Copy button (Surface-2 fill, 44px minimum target). On copy, the button reads "Copied" in brass for 1.6s. In the hero the command wraps (`pre-wrap`); in install lists it scrolls.

### Terminal Figure (signature)
The brand's hero imagery: a `<figure>` with a mono title bar (`$ trellis graph` — brass prompt, Surface-2 bar), real command output at 0.875rem/1.75, and an optional Fog Gray footnote. Output coloring is semantic and minimal: package names at weight 560, structural glyphs and versions in Fog Gray, success lines in brass.

### Ledger Tables
Full-width tables inside a hairline-bordered, 8px-radius scroll container. Mono Fog Gray column headers on Surface-2; brass first column for file/key names; Fog Gray for failure/description columns. Row dividers are Line Soft hairlines; no zebra striping.

### Code Frames
Expressive Code with the `vesper` theme, overridden to the system: Surface background, Surface-2 tab bar, brass active-tab indicator, shadow disabled (`shadowColor: transparent`), Martian Mono, word wrap on by default (`defaultProps: { wrap: true }`). TOML section headers land near-brass, strings in the cobalt family — the frames belong to the same instrument. Terminal frames follow the site's `$`-prompt convention: no macOS traffic-light dots (dots are display:none'd and zeroed in styleOverrides), titles left-aligned in mono with a brass `$` prefix; untitled terminal frames show a plain hairline bar.

### Navigation
Sticky header on a 94% Harbor Steel fill with a hairline bottom border; wordmark is the lattice glyph (brass + cobalt strokes) beside lowercase "trellis" in stretched Archivo. Links are Ink Soft at 520, brass on hover. Docs are Starlight (`@astrojs/starlight`), fully re-themed to this system via `src/styles/starlight.css` (dark-only: ThemeProvider/ThemeSelect overridden): sidebar active item gets Surface-2 fill + brass text; Pagefind search, mono "On this page" rail, heading anchor links, and a themed 404 come from the framework. The landing page keeps its own Base layout; both consume the same `tokens.css`.

### Derived Version Strings
No page carries a hand-maintained version number. `src/lib/version.ts` derives `TRELLIS_VERSION` from the root `Cargo.toml` at build time (a `?raw` import), and every install command interpolates it — the site obeys the tool's own derive-don't-declare doctrine. Statements about when a feature shipped ("Trellis 0.2.0 added…") are historical facts and stay literal.

## 6. Do's and Don'ts

### Do:
- **Do** keep steel cobalt carrying real surface area — bands, panels, terminal figures (The Committed Surface Rule).
- **Do** use real trellis output for every terminal figure, captured from an actual workspace (The Proof-in-Mono Rule).
- **Do** draw all structure with 1px hairlines and tonal steps; state changes recolor, never lift (The Flat Field Rule).
- **Do** hold WCAG AA everywhere: Chart Ink 16.3:1, Fog Gray 9.1:1, Ink Dark on brass 9.9:1 — verified numerically, re-verify when tokens move.
- **Do** give docs pages landing-page-level craft; the docs are the pitch.

### Don't:
- **Don't** build a **generic SaaS landing page**: no gradient heroes, no floating product screenshots, no pricing-page energy (PRODUCT.md anti-reference).
- **Don't** ship a **sterile default docs theme** — an out-of-the-box Starlight/Docusaurus look with no identity (PRODUCT.md anti-reference).
- **Don't** drift into **Linear/Vercel dark-glow**: glowing gradients, luminous blur, floating UI (The No-Glow Rule).
- **Don't** put Fog Gray text on cobalt fills — it fails contrast at 3.4:1 (The Ink-on-Cobalt Rule).
- **Don't** claim "simple" or "powerful" in the abstract; show the lattice before/after ledger and real config instead.
- **Don't** add eyebrow labels, reflex-numbered section markers, identical card grids, side-stripe borders, or box-shadows. The one numbered sequence (the command lifecycle) is numbered because it IS a sequence.

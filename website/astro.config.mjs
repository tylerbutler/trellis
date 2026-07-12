// @ts-check
import starlight from "@astrojs/starlight";
import { defineConfig } from "astro/config";

const github = "https://github.com/tylerbutler/trellis";
const site = "https://trellis.tylerbutler.com";

export default defineConfig({
  site,
  trailingSlash: "ignore",
  integrations: [
    starlight({
      title: "trellis",
      description:
        "Workspace tooling for multi-package Gleam repositories, derived from gleam.toml.",
      logo: { src: "./src/assets/logo.svg", alt: "" },
      favicon: "/favicon.svg",
      social: [{ icon: "github", label: "GitHub", href: github }],
      sidebar: [
        { label: "Overview", slug: "docs" },
        { label: "Installation", slug: "docs/installation" },
        { label: "Configuration", slug: "docs/configuration" },
        { label: "Task running", slug: "docs/task-running" },
        { label: "Changelog & versioning", slug: "docs/changelog" },
        { label: "Publishing", slug: "docs/publishing" },
        { label: "CI recipes", slug: "docs/ci" },
        {
          label: "Full README ↗",
          link: `${github}#readme`,
          attrs: { target: "_blank", rel: "noopener" },
        },
      ],
      customCss: [
        "@fontsource-variable/archivo/wdth.css",
        "@fontsource-variable/martian-mono",
        "./src/styles/starlight.css",
      ],
      components: {
        ThemeProvider: "./src/components/starlight/ThemeProvider.astro",
        ThemeSelect: "./src/components/starlight/ThemeSelect.astro",
      },
      head: [
        {
          tag: "link",
          attrs: { rel: "icon", href: "/favicon.ico", sizes: "32x32" },
        },
        {
          tag: "link",
          attrs: { rel: "apple-touch-icon", href: "/apple-touch-icon.png" },
        },
        {
          tag: "meta",
          attrs: { property: "og:image", content: `${site}/og.png` },
        },
        {
          tag: "meta",
          attrs: { name: "twitter:card", content: "summary_large_image" },
        },
      ],
      expressiveCode: {
        themes: ["vesper"],
        useThemedScrollbars: false,
        defaultProps: { wrap: true },
        styleOverrides: {
          borderRadius: "8px",
          borderColor: "oklch(0.34 0.03 230)",
          codeFontFamily:
            '"Martian Mono Variable", ui-monospace, "SF Mono", Menlo, monospace',
          codeFontSize: "0.875rem",
          codeLineHeight: "1.75",
          codeBackground: "oklch(0.205 0.026 230)",
          frames: {
            shadowColor: "transparent",
            editorBackground: "oklch(0.205 0.026 230)",
            editorActiveTabBackground: "oklch(0.205 0.026 230)",
            editorActiveTabForeground: "oklch(0.88 0.02 230)",
            editorActiveTabIndicatorTopColor: "oklch(0.8 0.128 80)",
            editorTabBarBackground: "oklch(0.25 0.03 230)",
            editorTabBarBorderBottomColor: "oklch(0.27 0.028 230)",
            terminalBackground: "oklch(0.205 0.026 230)",
            terminalTitlebarBackground: "oklch(0.25 0.03 230)",
            terminalTitlebarForeground: "oklch(0.88 0.02 230)",
            terminalTitlebarBorderBottomColor: "oklch(0.27 0.028 230)",
            terminalTitlebarDotsForeground: "transparent",
            terminalTitlebarDotsOpacity: "0",
            inlineButtonBackground: "oklch(0.25 0.03 230)",
            inlineButtonForeground: "oklch(0.88 0.02 230)",
          },
        },
      },
    }),
  ],
});

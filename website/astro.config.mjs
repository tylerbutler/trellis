// @ts-check
import { defineConfig } from "astro/config";
import expressiveCode from "astro-expressive-code";

export default defineConfig({
  site: "https://trellis.tylerbutler.com",
  trailingSlash: "ignore",
  integrations: [
    expressiveCode({
      themes: ["vesper"],
      useThemedScrollbars: false,
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
          inlineButtonBackground: "oklch(0.25 0.03 230)",
          inlineButtonForeground: "oklch(0.88 0.02 230)",
        },
      },
    }),
  ],
});

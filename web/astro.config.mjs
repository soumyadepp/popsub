import starlight from "@astrojs/starlight";
import { defineConfig } from "astro/config";
import { satteri } from "@astrojs/markdown-satteri";
import readable from "./src/plugins/readable.mjs";

export default defineConfig({
  site: "https://popsub.dev",
  markdown: {
    processor: satteri({ hastPlugins: [readable] }),
  },
  redirects: {
    "/getting-started/": "/quickstart/",
    "/guides/topics-and-replay/": "/concepts/topics-and-replay/",
    "/guides/qos-and-acknowledgements/": "/concepts/delivery-and-acks/",
    "/guides/durable-sessions/": "/concepts/durable-sessions/",
    "/guides/wildcards-and-groups/": "/concepts/wildcards-and-groups/",
    "/guides/users-and-permissions/": "/concepts/users-and-permissions/",
    "/reference/known-gaps/": "/help/status-and-comparisons/",
  },
  integrations: [
    starlight({
      title: "PopSub",
      description:
        "A publish/subscribe broker over WebSockets, written in Rust. One binary, a data directory, and a JSON protocol any language can speak.",
      logo: { src: "./src/assets/logo.svg", replacesTitle: false },
      favicon: "/favicon.svg",
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/soumyadepp/popsub",
        },
      ],
      editLink: {
        baseUrl: "https://github.com/soumyadepp/popsub/edit/main/web/",
      },
      customCss: [
        "@fontsource-variable/inter",
        "@fontsource-variable/inter-tight",
        "@fontsource-variable/geist-mono",
        "./src/styles/custom.css",
      ],
      expressiveCode: {
        themes: ["github-dark-default", "github-light-default"],
        defaultProps: {
          overridesByLang: {
            "bash,sh,shell,zsh": { frame: "none" },
          },
        },
        styleOverrides: {
          borderRadius: "12px",
          borderColor: "var(--pop-border)",
          codeFontFamily: "var(--sl-font-mono)",
          codeFontSize: "0.8125rem",
          codeLineHeight: "1.7",
          codePaddingBlock: "1rem",
          codePaddingInline: "1.15rem",
          uiFontFamily: "var(--sl-font)",
          uiFontSize: "0.75rem",
          codeBackground: "var(--pop-code-bg)",
          frames: {
            shadowColor: "transparent",
            frameBoxShadowCssValue: "none",
            editorBackground: "var(--pop-code-bg)",
            editorActiveTabBackground: "var(--pop-code-bg)",
            editorActiveTabIndicatorTopColor: "transparent",
            editorActiveTabIndicatorBottomColor: "var(--pop-ink)",
            editorActiveTabForeground: "var(--pop-ink)",
            editorTabBarBackground: "var(--pop-panel)",
            editorTabBarBorderBottomColor: "var(--pop-border)",
            terminalBackground: "var(--pop-code-bg)",
            terminalTitlebarBackground: "var(--pop-panel)",
            terminalTitlebarBorderBottomColor: "var(--pop-border)",
            terminalTitlebarForeground: "var(--pop-muted)",
            terminalTitlebarDotsForeground: "var(--pop-border-strong)",
            terminalTitlebarDotsOpacity: "1",
            inlineButtonBackground: "var(--pop-ink)",
            inlineButtonForeground: "var(--pop-ink)",
            inlineButtonBorder: "var(--pop-border-strong)",
            tooltipSuccessBackground: "var(--pop-ink)",
            tooltipSuccessForeground: "var(--pop-bg)",
          },
        },
      },
      components: { Hero: "./src/components/Hero.astro" },
      sidebar: [
        {
          label: "Start here",
          items: [
            { label: "What is PopSub?", slug: "introduction" },
            { label: "Quickstart", slug: "quickstart" },
            { label: "Core concepts", slug: "concepts" },
          ],
        },
        {
          label: "Examples",
          items: [
            { label: "Overview", slug: "examples" },
            { label: "Chat room with history", slug: "examples/chat-room" },
            { label: "Live sensor dashboard", slug: "examples/live-dashboard" },
            { label: "Worker that survives restarts", slug: "examples/restartable-worker" },
            { label: "Job queue with several workers", slug: "examples/job-queue" },
            { label: "Publish from your backend", slug: "examples/publish-from-backend" },
            { label: "A private inbox per user", slug: "examples/private-inboxes" },
            { label: "Replay history from a time", slug: "examples/replay-history" },
            { label: "Live updates in a browser", slug: "examples/browser" },
          ],
        },
        {
          label: "Concepts",
          items: [
            { label: "Topics and replay", slug: "concepts/topics-and-replay" },
            { label: "Delivery and acknowledgements", slug: "concepts/delivery-and-acks" },
            { label: "Durable sessions", slug: "concepts/durable-sessions" },
            { label: "Wildcards and groups", slug: "concepts/wildcards-and-groups" },
            { label: "Users and permissions", slug: "concepts/users-and-permissions" },
          ],
        },
        {
          label: "Run it",
          items: [
            { label: "Configuration", slug: "run/configuration" },
            { label: "Deploy to a server", slug: "run/deployment" },
            { label: "Performance", slug: "run/performance" },
          ],
        },
        {
          label: "Reference",
          items: [
            { label: "Client libraries", slug: "reference/client-libraries" },
            { label: "Errors and close codes", slug: "reference/errors-and-close-codes" },
            { label: "Protocol", slug: "reference/protocol" },
          ],
        },
        {
          label: "Help",
          items: [
            { label: "FAQ and troubleshooting", slug: "help/faq" },
            { label: "Status and comparisons", slug: "help/status-and-comparisons" },
          ],
        },
      ],
    }),
  ],
});

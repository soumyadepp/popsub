import starlight from "@astrojs/starlight";
import { defineConfig } from "astro/config";

export default defineConfig({
  site: "https://popsub.dev",
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
      customCss: ["./src/styles/custom.css"],
      components: { Hero: "./src/components/Hero.astro" },
      sidebar: [
        { label: "Get started", slug: "getting-started" },
        { label: "Guides", items: [{ autogenerate: { directory: "guides" } }] },
        { label: "Reference", items: [{ autogenerate: { directory: "reference" } }] },
      ],
    }),
  ],
});

/**
 * Keeps tables and short code readable on narrow screens, as a Sätteri hast plugin.
 *
 * Wraps every table in a `div.table-scroll`, so a wide table scrolls sideways instead of
 * squeezing its columns until code breaks across lines. Marks inline code of at most
 * `SHORT_CODE` characters with the class `nowrap`, so a value like `{ last: n }` stays on one
 * line, while long commands can still wrap on a phone.
 */

const SHORT_CODE = 32;

/**
 * The plugin, passed to `satteri({ hastPlugins })` in astro.config.mjs.
 */
export default {
  name: "popsub-readable",
  element: [
    {
      filter: ["table"],
      /**
       * Wraps a table in a scrolling container.
       *
       * Args:
       *     node: The table element.
       *     ctx: Sätteri's visitor context.
       */
      visit(node, ctx) {
        ctx.wrapNode(node, {
          type: "element",
          tagName: "div",
          properties: { className: ["table-scroll"] },
          children: [],
        });
      },
    },
    {
      filter: ["code"],
      /**
       * Marks short inline code so the stylesheet keeps it on one line.
       *
       * Args:
       *     node: The code element.
       *     ctx: Sätteri's visitor context.
       */
      visit(node, ctx) {
        const parent = ctx.parent(node);
        if (parent?.type === "element" && parent.tagName === "pre") return;
        if (ctx.textContent(node).length > SHORT_CODE) return;
        const classes = node.properties?.className ?? [];
        ctx.setProperty(node, "className", [...classes, "nowrap"]);
      },
    },
  ],
};

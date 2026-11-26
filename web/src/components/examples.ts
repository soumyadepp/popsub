/**
 * The four example languages: how each names its files, highlights its code and runs a
 * program, plus a lookup for the example sources in src/examples.
 */

export interface Language {
  label: string;
  extension: string;
  highlight: string;
  path: (file: string) => string;
  run: (file: string, args: string) => string;
}

const withArgs = (command: string, args: string) => (args ? `${command} ${args}` : command);

export const languages: Language[] = [
  {
    label: "TypeScript",
    extension: "mjs",
    highlight: "js",
    path: (file) => `${file}.mjs`,
    run: (file, args) => withArgs(`node ${file}.mjs`, args),
  },
  {
    label: "Python",
    extension: "py",
    highlight: "python",
    path: (file) => `${file}.py`,
    run: (file, args) => withArgs(`python ${file}.py`, args),
  },
  {
    label: "Go",
    extension: "go",
    highlight: "go",
    path: (file) => `${file}/main.go`,
    run: (file, args) => withArgs(`go run ./${file}`, args),
  },
  {
    label: "Rust",
    extension: "rs",
    highlight: "rust",
    path: (file) => `src/bin/${file}.rs`,
    run: (file, args) => (args ? `cargo run -q --bin ${file} -- ${args}` : `cargo run -q --bin ${file}`),
  },
];

const sources = import.meta.glob<string>("../examples/**/*.{mjs,py,go,rs}", {
  query: "?raw",
  import: "default",
  eager: true,
});

/**
 * Returns the source of one example program in one language.
 *
 * Args:
 *     example: The example's folder under src/examples, such as `chat`.
 *     file: The program's name without an extension, such as `chat`.
 *     language: The language to return it in.
 *
 * Returns:
 *     The file's contents with trailing whitespace removed.
 *
 * Raises:
 *     Error: When the file doesn't exist, so a missing translation fails the build.
 */
export function sourceFor(example: string, file: string, language: Language): string {
  const key = `../examples/${example}/${file}.${language.extension}`;
  const source = sources[key];
  if (source === undefined) {
    throw new Error(`missing example source ${key}`);
  }
  return source.trimEnd();
}

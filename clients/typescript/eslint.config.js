/**
 * ESLint flat configuration: typescript-eslint's strict and stylistic
 * type-checked presets for the TypeScript sources and tests.
 */

import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: ["dist/", "dist-test/", "node_modules/"],
  },
  tseslint.configs.strictTypeChecked,
  tseslint.configs.stylisticTypeChecked,
  {
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    rules: {
      "no-console": "error",
      "@typescript-eslint/no-explicit-any": "error",
      "@typescript-eslint/explicit-module-boundary-types": "error",
      "@typescript-eslint/consistent-type-imports": "error",
      "@typescript-eslint/no-floating-promises": [
        "error",
        {
          allowForKnownSafeCalls: [
            { from: "package", package: "node:test", name: ["test", "before", "after"] },
          ],
        },
      ],
    },
  },
  {
    files: ["src/driver.ts", "test/**/*.ts", "examples/**/*.ts"],
    rules: {
      "no-console": "off",
    },
  },
  {
    files: ["**/*.js"],
    extends: [tseslint.configs.disableTypeChecked],
  },
);

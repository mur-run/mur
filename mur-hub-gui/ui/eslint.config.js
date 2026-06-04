// Minimal flat config focused on React's Rules of Hooks. A violation here
// (a hook after an early return, or a conditionally-called hook) is what
// crashed the Hub with React error #310 — `rules-of-hooks` catches that class
// at build time. Kept deliberately narrow to avoid flooding unrelated lint.
import tsParser from "@typescript-eslint/parser";
import reactHooks from "eslint-plugin-react-hooks";

export default [
  { ignores: ["dist/**", "node_modules/**"] },
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      parser: tsParser,
      parserOptions: {
        ecmaFeatures: { jsx: true },
        sourceType: "module",
      },
    },
    plugins: { "react-hooks": reactHooks },
    rules: {
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
    },
  },
];

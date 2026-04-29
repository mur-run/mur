import type { Config } from "tailwindcss";

export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  // Tailwind 4 reads tokens from CSS @theme directive in index.css —
  // theme switching at runtime injects CSS variables from theme.json.
} satisfies Config;

import { readFileSync } from "node:fs";
import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import { firstLoadBudget } from "./build/first-load-budget.mjs";
import { recoverableParts } from "./build/recoverable-parts.mjs";
import { startupAudit } from "./build/startup-audit.mjs";
import { startupBudget } from "./build/startup-budget.mjs";

const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")) as { version: string };
// Tessera's public home: `repository` in the workspace Cargo.toml, the one
// place it is set (the Rust code reads it from there too).
const cargo = readFileSync(new URL("../../../Cargo.toml", import.meta.url), "utf8");
const repo = /^repository\s*=\s*"([^"]+)"/m.exec(cargo)?.[1];
if (!repo) throw new Error("Cargo.toml has no `repository`");

// The mock server (dev/mock-server.mjs) or a real `tessera serve` listens on 7878.
const backend = process.env.TESSERA_BACKEND ?? "http://127.0.0.1:7878";

/** Emitted HTML + static closure (including inline JS) is the native launch gate.
 * A paired /web/ browser also awaits lib/e2e and its closure before chat appears.
 * 2b74376 measured 681767 bytes for that path; 670 KiB gives 4313 bytes (0.63%)
 * headroom, not a moving limit. Onboarding/FileWindow are reported separately. */
const launchFixture = JSON.parse(readFileSync(new URL("./tests/fixtures/first-load-budget.json", import.meta.url), "utf8")) as { budgets: { staticKiB: number; pairedKiB: number } };
const FIRST_LOAD_BUDGET_KIB = launchFixture.budgets.staticKiB;
const PAIRED_BROWSER_BUDGET_KIB = launchFixture.budgets.pairedKiB;


const audited = startupBudget(startupAudit(), firstLoadBudget(FIRST_LOAD_BUDGET_KIB, PAIRED_BROWSER_BUDGET_KIB));

export default defineConfig({
  base: "./",
  plugins: [audited.audit, solid(), recoverableParts(), audited.budget],
  define: { __APP_VERSION__: JSON.stringify(pkg.version), __TESSERA_REPO__: JSON.stringify(repo.replace(/\/+$/, "")) },
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: process.env.TAURI_DEV_HOST || false,
    // The apps refuse fetches anywhere but their own channel (no `data:`
    // URLs either); the dev server does the same so tests catch it.
    headers: { "Content-Security-Policy": "connect-src 'self' http: https: ws: wss:" },
    proxy: {
      "/api": { target: backend, ws: true, changeOrigin: true },
      // Whiteboard pages (`board_link`) and their bundled libraries.
      "/board/": { target: backend, changeOrigin: true },
      "/board-lib/": { target: backend, changeOrigin: true },
      // Previews of a note's HTML (`preview_page`).
      "/preview/": { target: backend, changeOrigin: true },
    },
  },
  build: {
    outDir: "dist",
    target: "es2022",
    sourcemap: false,
    cssMinify: true,
    // Keep runtime helpers apart from deferred code and the audited retry bootstrap.
    rolldownOptions: { output: { manualChunks: (id: string) => id.endsWith('/src/lib/retryImport.ts') ? 'retryImport' : id === '\0vite/preload-helper.js' ? 'preloadHelper' : id === '\0rolldown/runtime.js' ? 'runtime' : undefined } },
    // One bundle for the whole app (~815 kB minified, ~260 kB gzipped); overlays
    // opened on demand (the mosaic switcher, "Show beside this chat") load apart.
    chunkSizeWarningLimit: 900,
  },
});

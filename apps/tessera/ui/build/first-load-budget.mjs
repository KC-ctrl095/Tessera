import { readFileSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { parse as parseHtml } from "parse5";
import { parse as parseJs } from "@babel/parser";

export const LAUNCH_MODULES = {
  paired: "src/lib/e2e.ts",
  onboarding: "src/components/Onboarding.tsx",
  setup: "src/components/Setup.tsx",
  fileWindowShell: "src/components/files/FileWindow.tsx",
};

function localFile(reference, owner, html = false) {
  const base = new URL(owner, "https://emitted.invalid/");
  const url = new URL(reference, base);
  if (url.origin !== base.origin || (!html && !/^(?:\.?\.?\/|\/)/.test(reference))) throw new Error(`Uncounted external JavaScript: ${reference} from ${owner}`);
  return decodeURIComponent(url.pathname.slice(1));
}

/** Read the final HTML, not Rollup's entry flags: preload/script assets count too. */
function htmlSeeds(directory) {
  const roots = new Set();
  const addRoot = reference => {
    const name = localFile(reference, "index.html", true);
    if (!/\.[cm]?js$/.test(name)) throw new Error(`Uncounted executable HTML asset: ${reference}`);
    roots.add(name);
  };
  const inline = [];
  const html = parseHtml(readFileSync(resolve(directory, "index.html"), "utf8"));
  const walk = node => {
    const attrs = Object.fromEntries((node.attrs ?? []).map(a => [a.name, a.value]));
    if (Object.entries(attrs).some(([key, value]) => /^on[a-z]+$/i.test(key) || /^javascript:/i.test(value.trim()))) throw new Error("Uncounted inline HTML handler: use a measured script.");
    if (node.tagName === "base") throw new Error("Uncounted HTML base URL: use the emitted relative paths.");
    if (node.tagName === "script") {
      const type = attrs.type?.trim().toLowerCase();
      const executable = !type || type === "module" || /^(?:application|text)\/(?:x-)?(?:ecmascript|javascript(?:1\.[0-5])?|jscript|livescript)$/.test(type);
      if (!executable && !/^(?:application\/(?:ld\+)?json|text\/plain)$/.test(type)) throw new Error(`Uncounted script type: ${type}`);
      if (!executable) return;
      if (attrs.src !== undefined) addRoot(attrs.src);
      else {
        const code = (node.childNodes ?? []).map(n => n.value ?? "").join("");
        inline.push({ fileName: `index.html#script-${inline.length}`, code, bytes: Buffer.byteLength(code) });
      }
    }
    if (node.tagName === "link" && (/\bmodulepreload\b/i.test(attrs.rel ?? "") || (/\bpreload\b/i.test(attrs.rel ?? "") && attrs.as === "script"))) {
      if (!attrs.href) throw new Error("Uncounted HTML script preload without a URL.");
      addRoot(attrs.href);
    }
    for (const child of node.childNodes ?? []) walk(child);
  };
  walk(html);
  if (!roots.size && !inline.length) throw new Error("No executable JavaScript in emitted index.html.");
  return { roots, inline };
}

/** Catch post-hook imports, JS assets and workers missing from Rollup's chunk map. */
function codeImports(code, owner, includeDynamic, allowRuntimeRetry = false) {
  const dependencies = new Set();
  dependencies.dynamicCounts = new Map();
  const ast = parseJs(code, { sourceType: "unambiguous", createImportExpressions: true });
  const add = source => {
    const literal = source?.type === "StringLiteral" ? source.value : source?.type === "TemplateLiteral" && !source.expressions.length ? source.quasis[0].value.cooked : null;
    if (literal === null) throw new Error(`Uncounted JavaScript dependency in ${owner}`);
    dependencies.add(localFile(literal, owner));
  };
  const walk = node => {
    if (!node || typeof node !== "object") return;
    if (/^(?:ImportDeclaration|ExportNamedDeclaration|ExportAllDeclaration)$/.test(node.type) && node.source) add(node.source);
    if (includeDynamic && node.type === 'ImportExpression') {
      if (!(allowRuntimeRetry && node.source?.type !== "StringLiteral")) {
        add(node.source);
        const literal = node.source.type === 'StringLiteral' ? node.source.value : node.source.quasis[0].value.cooked;
        const dep = localFile(literal, owner);
        dependencies.dynamicCounts.set(dep, (dependencies.dynamicCounts.get(dep) ?? 0) + 1);
      }
    }
    if (node.type === "CallExpression" && /^(?:importScripts|require)$/.test(node.callee?.name)) node.arguments.forEach(add);
    const constructor = node.callee?.type === "MemberExpression" ? node.callee.property?.name : node.callee?.name;
    if (node.type === "NewExpression" && /^(?:SharedWorker|Worker)$/.test(constructor)) {
      const arg = node.arguments[0];
      add(arg?.type === "NewExpression" && arg.callee?.name === "URL" ? arg.arguments[0] : arg);
    }
    for (const [key, value] of Object.entries(node)) {
      if (key === "loc" || key === "extra") continue;
      if (Array.isArray(value)) value.forEach(walk);
      else if (value && typeof value === "object") walk(value);
    }
  };
  walk(ast.program);
  return dependencies;
}

function measure(bundle, directory, html, extra = [], mode = "static") {
  const outputs = new Map(Object.values(bundle).map(out => [out.fileName, out]));
  const files = new Map(html.inline.map(({ fileName, bytes }) => [fileName, { fileName, bytes }]));
  const seen = new Set();
  const identities = out => [out.facadeModuleId, ...(out.moduleIds ?? [])].filter(Boolean).map(id => id.replaceAll("\\", "/").split("?")[0]);
  const deferred = (out, dep, count) => {
    if (!outputs.has(dep)) throw new Error(`Uncounted startup JavaScript: ${dep}`);
    const sites = out.tesseraStartupEdges?.[dep];
    return sites?.length >= count && sites.every(site => !site.modes.includes(mode));
  };
  const visit = name => {
    if (seen.has(name)) return;
    seen.add(name);
    const out = outputs.get(name);
    if (!out) throw new Error(`Uncounted JavaScript dependency: ${name}`);
    // CSS is not JavaScript; its existence is still required. Nothing else is silently skipped.
    const path = resolve(directory, name);
    const bytes = statSync(path).size;
    if (/\.css$/.test(name)) return;
    if (!/\.[cm]?js$/.test(name)) throw new Error(`Uncounted non-JavaScript dependency: ${name}`);
    files.set(name, { fileName: name, bytes });
    const code = readFileSync(path, "utf8");
    const deps = new Set([...(out.imports ?? []), ...codeImports(code, name, false)]);
    for (const dep of deps) visit(dep);
    const allowRuntimeRetry = identities(out).every(id => /\/src\/lib\/(?:chunkRecovery|retryImport)\.ts$/.test(id)) && identities(out).length > 0;
    const parsed = codeImports(code, name, true, allowRuntimeRetry);
    const all = new Set([...(out.dynamicImports ?? []), ...parsed]);
    for (const dep of all) if (!deps.has(dep) && !deferred(out, dep, Math.max(1, parsed.dynamicCounts.get(dep) ?? 0))) visit(dep);
  };
  for (const item of html.inline) for (const dep of codeImports(item.code, "index.html", true)) visit(dep);
  for (const name of [...html.roots, ...extra]) visit(name);
  const sorted = [...files.values()].sort((a, b) => a.fileName.localeCompare(b.fileName));
  return { files: sorted, bytes: sorted.reduce((total, file) => total + file.bytes, 0) };
}

export function emittedFirstLoad(bundle, directory) {
  return measure(bundle, directory, htmlSeeds(directory));
}

/** Explicit startup branches. E2E is awaited before a paired browser can show chat;
 * onboarding and FileWindow are separate, printed launch modes, not optional modals. */
export function emittedLaunchPaths(bundle, directory) {
  const html = htmlSeeds(directory);
  const chunks = Object.values(bundle).filter(out => out.type === "chunk");
  const chunkFor = source => {
    const found = chunks.filter(out => [out.facadeModuleId, ...(out.moduleIds ?? [])].some(id => id?.replaceAll("\\", "/").split("?")[0].endsWith(`/${source}`)));
    if (found.length !== 1) throw new Error(`Uncounted startup module ${source}: expected one emitted chunk, found ${found.length}`);
    return found[0].fileName;
  };
  const paths = { static: measure(bundle, directory, html) };
  for (const [mode, source] of Object.entries(LAUNCH_MODULES)) {
    const root = chunkFor(source);
    paths[mode] = measure(bundle, directory, html, [root], mode);
  }
  return paths;
}

export function firstLoadBudget(staticKiB, pairedKiB) {
  return {
    name: "tessera-first-load-budget",
    apply: "build",
    // Final emitted HTML/JS includes Vite's late import-analysis/preload helpers.
    writeBundle: {
      order: "post",
      sequential: true,
      handler(options, bundle) {
        const directory = options.dir ?? (options.file && dirname(options.file));
        if (!directory) this.error("Cannot measure first-load JavaScript without an output directory.");
        const paths = emittedLaunchPaths(bundle, directory);
        const violations = [];
        for (const [mode, first] of Object.entries(paths)) {
          const limit = mode === "static" ? staticKiB : mode === "paired" ? pairedKiB : null;
          if (limit !== null && !Number.isFinite(limit)) this.error(`Missing ${mode} JavaScript budget.`);
          const label = mode === "fileWindowShell" ? "FileWindow shell only" : mode;
          const line = `${label} first-load JavaScript: ${(first.bytes / 1024).toFixed(1)} KiB in ${first.files.length} files (${first.bytes} emitted bytes; ${limit === null ? "reported, not gated" : `budget ${limit} KiB`})`;
          console.log(line);
          if (limit !== null && first.bytes > limit * 1024) violations.push(line);
        }
        if (violations.length) this.error(`${violations.join("; ")}. Load optional screens on first use instead.`);
      },
    },
  };
}

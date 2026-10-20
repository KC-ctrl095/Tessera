// Cargo.toml is the public project address. Standalone installers and copyable
// documentation commands keep generated copies because they cannot import it.
// After changing repository there, run `node scripts/release-links.mjs --write`.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const cargo = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8");
const repo = /^repository\s*=\s*"([^"]+)"/m.exec(cargo)?.[1]?.replace(/\/$/, "");
if (!repo || !/^https:\/\/[^/'"\s]+\/[^/'"\s]+\/[^/'"\s]+$/.test(repo)) {
  throw new Error("Set workspace.package.repository in Cargo.toml to the public HTTPS project page.");
}
const write = process.argv.includes("--write");
const rules = [
  ["scripts/install.sh", /^REPO_URL="\$\{TESSERA_REPO_URL:-[^}]+\}"/m, `REPO_URL="\${TESSERA_REPO_URL:-${repo}}"`],
  ["scripts/install.ps1", /^\$TesseraRepo = '[^']+'/m, `$TesseraRepo = '${repo}'`],
  ["scripts/install.sh", /curl -fsSL https?:\/\/[^\s]+\/releases\/latest\/download\/install\.sh/g, `curl -fsSL ${repo}/releases/latest/download/install.sh`],
  ["scripts/install.ps1", /irm https?:\/\/[^\s]+\/releases\/latest\/download\/install\.ps1/g, `irm ${repo}/releases/latest/download/install.ps1`],
  ["README.md", /curl -fsSL https?:\/\/[^\s]+\/releases\/latest\/download\/install\.sh/g, `curl -fsSL ${repo}/releases/latest/download/install.sh`],
  ["README.md", /irm https?:\/\/[^\s]+\/releases\/latest\/download\/install\.ps1/g, `irm ${repo}/releases/latest/download/install.ps1`],
  ["docs/WINDOWS.md", /irm https?:\/\/[^\s]+\/releases\/latest\/download\/install\.ps1/g, `irm ${repo}/releases/latest/download/install.ps1`],
  ["README.md", /git clone https?:\/\/[^\s]+/g, `git clone ${repo}`],
  ["CONTRIBUTING.md", /git clone https?:\/\/[^\s]+/g, `git clone ${repo}`],
];
let stale = 0;
for (const [file, pattern, replacement] of rules) {
  const full = path.join(root, file);
  const before = fs.readFileSync(full, "utf8");
  if (!before.match(pattern)) throw new Error(`Expected project link missing in ${file}`);
  const after = before.replace(pattern, () => replacement);
  if (before !== after) {
    stale++;
    if (write) fs.writeFileSync(full, after);
    else console.error(`Project link out of date in ${file}`);
  }
}
if (stale && !write) process.exitCode = 1;
console.log(`${rules.length} project link checks${write ? " updated" : " checked"}.`);

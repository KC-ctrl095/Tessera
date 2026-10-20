// Source-only checks: execute the production UI writers with a fake bridge.
// No browser, native app, server, user profile or network is opened.
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";

const require = createRequire(import.meta.url);
const ts = process.env.TYPESCRIPT_PATH ? require(process.env.TYPESCRIPT_PATH) : require("../../ui/node_modules/typescript");

function load(relative, imports) {
  const filename = new URL(relative, import.meta.url);
  const { outputText, diagnostics } = ts.transpileModule(fs.readFileSync(filename, "utf8"), {
    fileName: filename.pathname,
    reportDiagnostics: true,
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  });
  assert.equal(diagnostics.filter((d) => d.category === ts.DiagnosticCategory.Error).length, 0);
  const exports = {};
  vm.runInNewContext(outputText, {
    exports,
    require: (name) => {
      assert.ok(Object.hasOwn(imports, name), `unexpected runtime import: ${name}`);
      return imports[name];
    },
    URL, URLSearchParams, console,
  }, { filename: filename.pathname });
  return exports;
}

const server = { id: "remote", name: "Saved", endpoints: ["https://remote.example"], headers: {}, token: "", deviceName: "Desktop" };

function app(backend) {
  const state = { conn: { state: "connected", serverId: "remote" }, config: { servers: [server], active: "remote", revision: 7 } };
  return load("../../ui/src/lib/app.ts", {
    "solid-js": { createSignal: (initial) => [() => initial, () => {}], createEffect: () => {}, createRoot: () => {} },
    "./backend": { backend },
    "./store": { state, setState: (key, value) => { state[key] = value; } },
    "./native": {}, "./alerts": {}, "./files": {}, "./ui": {}, "./devices": {}, "./workspace": {},
  });
}

for (const [action, args] of [
  ["saveServer", [{ ...server, name: "Renamed" }]],
  ["removeServer", ["remote"]],
  ["forgetServerEntry", ["remote"]],
  ["rememberDeviceName", ["Renamed device"]],
]) {
  test(`${action} preserves the revision and never retries a rejected stale snapshot`, async () => {
    let reads = 0;
    let saves = 0;
    const backend = {
      getConfig: async () => { reads++; return { servers: [server], active: "remote", revision: 7 }; },
      saveConfig: async (cfg) => {
        saves++;
        assert.equal(cfg.revision, 7);
        throw new Error("Saved servers changed");
      },
    };
    await assert.rejects(app(backend)[action](...args), /Saved servers changed/);
    assert.equal(saves, 1);
    assert.equal(reads, 1, "no fresh snapshot was silently substituted after rejection");
  });
}

test("a failed config read cannot turn into an empty whole-list save", async () => {
  let saves = 0;
  const backend = {
    getConfig: async () => { throw new Error("read failed"); },
    saveConfig: async () => { saves++; },
  };
  await assert.rejects(app(backend).removeServer("remote"), /read failed/);
  assert.equal(saves, 0);
});

function transport(invoke) {
  const { TauriTransport } = load("../../ui/src/lib/transport.ts", { "./util": {} });
  const transport = new TauriTransport();
  // All native effects terminate at the fake invoke boundary.
  transport.init = async () => invoke;
  return transport;
}

test("native pairing and link pairing do not issue a second whole-list save", async () => {
  const calls = [];
  const bridge = transport(async (cmd) => { calls.push(cmd); return server; });
  assert.equal((await bridge.pair({ endpoints: server.endpoints, headers: {}, code: "12345678", deviceName: "Desktop", name: "Saved" })).id, server.id);
  assert.equal((await bridge.pairLink("tessera://pair?fixture", "Desktop")).id, server.id);
  assert.deepEqual(calls, ["server_pair", "server_pair_link"]);
});

test("native selection uses the command's own locked save, not a whole-list snapshot", async () => {
  const calls = [];
  const bridge = transport(async (cmd, args) => { calls.push([cmd, args.id]); });
  await bridge.switchTo("remote", { keep: true });
  assert.deepEqual(calls, [["server_connect", "remote"]]);
});

test("the native save bridge preserves and propagates a rejected revision without retry", async () => {
  let calls = 0;
  const bridge = transport(async (cmd, { config }) => {
    calls++;
    assert.equal(cmd, "servers_save");
    assert.equal(config.revision, 7);
    throw "Saved servers changed";
  });
  await assert.rejects(bridge.saveConfig({ servers: [server], active: "remote", revision: 7 }), /Saved servers changed/);
  assert.equal(calls, 1);
});

// Offline unit regression: execute the actual reactive store, with no
// backend/browser/user data, and deliver whole snapshots out of order.
import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";
import { createRequire } from "node:module";
import test from "node:test";
import ts from "../node_modules/typescript/lib/typescript.js";

const require = createRequire(import.meta.url);
function store() {
  const source = fs.readFileSync(new URL("../src/lib/store.ts", import.meta.url), "utf8");
  const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
  const exports = {};
  let connection = 0;
  exports.connectForTest = () => connection++;
  // Same realm as Solid's Object.prototype checks, isolated module closure.
  vm.runInThisContext(`(function(exports, require) { ${js}\nexports.applySnapshotForTest = applySnapshot; })`)(exports, (id) => {
      if (id === "solid-js" || id === "solid-js/store") return require(id);
      if (id === "./completion") return { isCheckAck: () => false };
      if (id === "./connectionGeneration") return { connectionGeneration: () => connection };
      if (id === "./backend") return { backend: { request() { throw new Error("no network in a unit test"); } } };
      if (id === "./machines") return { machinesChanged() {} };
      if (id === "./ui") return { toast() {} };
      if (id === "./util") return { reducedMotion: () => true, uid: () => "fixture" };
      throw new Error(`unexpected dependency ${id}`);
  });
  return exports;
}
const info = (revision, atOnce = 7, archived = false) => ({
  id: "fixture", title: "Helper revision", kind: "chat", cwd: "/fixture",
  model: { provider: "fixture", model: "model" }, thinking: "auto", permission: "default",
  status: "running", created_at: 1, updated_at: 1, last_seq: 0, members: [],
  usage: {}, context: {}, private: false, pinned: false, preview: "", steps: atOnce,
  fast: false, subagents: true, archived, helper_limits: { at_once: atOnce },
  ...(revision === undefined ? {} : { revision }),
});

test("older updates cannot replace helper settings, counters or archive state", () => {
  const s = store();
  s.setState("archived", []);
  s.applyEvent({ type: "session_updated", session: info(3) });
  s.applyEvent({ type: "session_updated", session: info(2, 1, true) });
  assert.equal(s.state.sessions.fixture.info.revision, 3);
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 7);
  assert.equal(s.state.sessions.fixture.info.steps, 7);
  assert.equal(s.state.sessions.fixture.info.archived, false);
  assert.equal(s.state.archived.length, 0);
  s.applyEvent({ type: "session_updated", session: info(4, 9, true) });
  assert.equal(s.state.sessions.fixture.info.revision, 4);
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 9);
  assert.equal(s.state.archived[0].revision, 4);
});

test("RPC upserts reject old revisions and missing revision means legacy zero", () => {
  const s = store();
  assert.equal(s.upsertSession(info(undefined, 1)), true);
  assert.equal(s.upsertSession(info(undefined, 2)), true);
  assert.equal(s.upsertSession(info(3, 7)), true);
  assert.equal(s.upsertSession(info(undefined, 1)), false);
  assert.equal(s.upsertSession(info(2, 1)), false);
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 7);
  assert.equal(s.upsertSession(info(3, 8)), true);
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 8);
});

test("a late full RPC snapshot still loads history without replacing newer public info", () => {
  const s = store();
  s.upsertSession(info(3, 7));
  s.applySnapshotForTest({ info:info(2, 1), entries:[], approvals:[], queued:[], history_revision:0, streaming:null }, false);
  assert.equal(s.state.sessions.fixture.loaded, true);
  assert.equal(s.state.sessions.fixture.info.revision, 3);
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 7);
  assert.equal(s.state.sessions.fixture.info.steps, 7);
});

function bootstrapFixture(s, sessions) {
  s.api.hello = async () => ({});
  s.api.listSessions = async () => sessions;
  for (const name of ["listJobs", "listBots", "listBoards", "listNotes", "listProviders"]) s.api[name] = async () => [];
}

test("reconnect bootstrap resets revision 3 to legacy zero and legacy updates apply", async () => {
  const s = store();
  bootstrapFixture(s, [info(3, 7)]);
  await s.bootstrap();
  s.applyEvent({ type:"session_updated", session:info(2, 1) });
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 7, "same-connection older event is ignored");
  s.connectForTest();
  // An event before authoritative bootstrap cannot preserve the old watermark.
  s.applyEvent({ type:"session_updated", session:info(9, 9) });
  bootstrapFixture(s, [info(undefined, 1)]);
  await s.bootstrap();
  assert.equal(s.state.sessions.fixture.info.revision, 0);
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 1);
  s.applyEvent({ type:"session_updated", session:info(undefined, 2) });
  assert.equal(s.state.sessions.fixture.info.revision, 0);
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 2);
  s.applyEvent({ type:"session_updated", session:info(3, 7) });
  // A second bootstrap on this SAME connection must not reset revision again.
  await s.bootstrap();
  assert.equal(s.state.sessions.fixture.info.revision, 3);
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 7);
});

test("a bootstrap response from an earlier connection cannot reset the new baseline", async () => {
  const s = store();
  bootstrapFixture(s, [info(3, 7)]);
  let finishOld;
  s.api.listSessions = () => new Promise((resolve) => { finishOld = resolve; });
  const old = s.bootstrap();
  await Promise.resolve();
  assert.equal(typeof finishOld, "function");
  s.connectForTest();
  bootstrapFixture(s, [info(undefined, 1)]);
  await s.bootstrap();
  finishOld([info(3, 7)]);
  await old;
  assert.equal(s.state.sessions.fixture.info.revision, 0);
  s.applyEvent({ type:"session_updated", session:info(undefined, 2) });
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 2);
});

test("a full session response from an earlier connection keeps its generation guard", async () => {
  const s = store();
  s.upsertSession(info(3, 7));
  let finishOld;
  s.api.getSession = () => new Promise((resolve) => { finishOld = resolve; });
  const old = s.loadSession("fixture", false);
  s.connectForTest();
  bootstrapFixture(s, [info(undefined, 1)]);
  await s.bootstrap();
  finishOld({ info:info(3, 7), entries:[], approvals:[], queued:[], history_revision:0, streaming:null });
  await old;
  assert.equal(s.state.sessions.fixture.info.revision, 0);
  assert.equal(s.state.sessions.fixture.info.helper_limits.at_once, 1);
});

// Execute the mock's real grant issuer and HTTP handler with a fake clock.
// No sockets, browser state, or external services are used.
import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";
import test from "node:test";

function fixture() {
  const source = fs.readFileSync(new URL("../dev/mock-server.mjs", import.meta.url), "utf8");
  let now = 1000;
  let nonce = 0;
  const state = { boards: new Map(), boardTokens: new Map() };
  const context = vm.createContext({
    state, Date: { now: () => now }, hex: () => String(++nonce),
    need: (p, key) => p[key], notFound: (message) => new Error(message),
    badRequest: (message) => new Error(message),
    sendJson: (_res, status, body) => ({ status, body }),
    boardDocument: (html) => html, BOARD_CSP_HEADER: "fixture policy",
  });
  const issuer = source.slice(source.indexOf("  board_link(p) {"), source.indexOf("  board_fetch() {"));
  const link = vm.runInContext(`({${issuer}}).board_link`, context);
  const handler = source.slice(source.indexOf("  const bp = /^\\/board\\/"), source.indexOf("  const pp = /^\\/preview\\/"));
  assert.ok(handler.length > 100, "the real board page handler was found");
  const serve = vm.runInContext(`(function(req, res, p, url) { ${handler} })`, context);
  const page = (path) => {
    const url = new URL(path, "http://fixture.invalid");
    let status;
    const res = { writeHead: (code) => { status = code; }, end: (body) => ({ status, body }) };
    return serve({ method: "GET" }, res, url.pathname, url);
  };
  const board = { id: "wb_fixture", version: 1, content: { type: "html", html: "first revision" } };
  state.boards.set(board.id, board);
  return { board, state, link: () => link({ id: board.id }).path, page, now: (value) => { now = value; } };
}

test("mock board grants expire exactly at one day and are renewed, never extended by a read", () => {
  const f = fixture();
  const path = f.link();
  assert.equal(f.page(path).status, 200);
  f.now(1000 + 86_400_000 - 1);
  assert.equal(f.link(), path);
  assert.equal(f.page(path).status, 200);
  f.now(1000 + 86_400_000);
  assert.equal(f.page(path).status, 404);
  const fresh = f.link();
  assert.notEqual(fresh, path);
  assert.equal(f.page(path).status, 404);
  assert.equal(f.page(fresh).status, 200);
});

test("mock grants cannot read later revisions or deleted boards", () => {
  const f = fixture();
  const path = f.link();
  f.board.version++;
  f.board.content.html = "later revision";
  assert.equal(f.page(path).status, 404);
  const fresh = f.link();
  assert.equal(f.page(fresh).body, "later revision");
  assert.notEqual(fresh, path);
  f.state.boards.delete(f.board.id);
  assert.equal(f.page(fresh).status, 404);
});

test("browser frame URLs ask the authenticated connection for a fresh opaque path every time", async () => {
  const source = fs.readFileSync(new URL("../src/lib/boards.ts", import.meta.url), "utf8");
  const body = source.slice(source.indexOf("  const theme = resolvedTheme();"), source.indexOf("\n}\n", source.indexOf("export async function htmlFrameSrc")));
  let calls = 0;
  const context = vm.createContext({
    resolvedTheme: () => "light", isTauri: false, webAccess: false, URL,
    api: { boardLink: async (id) => ({ path: `/board/${id}/opaque${++calls}/` }) },
    backend: { sameOrigin: true }, location: { origin: "http://fixture.invalid" },
    withQuery: (url, params) => { const u = new URL(url); for (const [key, value] of Object.entries(params)) u.searchParams.set(key, value); return u.toString(); },
  });
  const src = vm.runInContext(`(async function(b) { ${body} })`, context);
  const first = await src({ id: "wb_fixture", version: 1 });
  const second = await src({ id: "wb_fixture", version: 2 });
  assert.equal(calls, 2);
  assert.match(first, /opaque1\/\?theme=light&v=1$/);
  assert.match(second, /opaque2\/\?theme=light&v=2$/);
});

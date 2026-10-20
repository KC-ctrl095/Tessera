import { parse } from '@babel/parser';
import { STARTUP_EDGES } from './startup-edges.mjs';
import { STARTUP_CALLS } from './startup-calls.mjs';

const clean = id => id.replaceAll('\\', '/').split('?')[0];
const isFunction = node => /^(?:FunctionDeclaration|FunctionExpression|ArrowFunctionExpression|ObjectMethod|ClassMethod|ClassPrivateMethod)$/.test(node?.type);
const children = node => Object.entries(node).filter(([key]) => !['loc', 'extra'].includes(key)).flatMap(([, value]) => Array.isArray(value) ? value : value && typeof value === 'object' ? [value] : []);
const calleeName = node => node?.type === 'Identifier' ? node.name : /^(?:Optional)?MemberExpression$/.test(node?.type) && !node.computed ? `${calleeName(node.object)}.${node.property.name}` : '';

export async function startupSource(code, importer, resolve) {
  const ast = parse(code, { sourceType: 'module', plugins: ['typescript', ...(clean(importer).endsWith('x') ? ['jsx'] : [])], createImportExpressions: true });
  const imports = [], topCalls = [], bindings = {}, topImports = new Set();
  const walk = (node, ancestors = []) => {
    if (!node || typeof node !== 'object') return;
    if (node.type === 'ImportExpression' && node.source.type === 'StringLiteral') imports.push({ node, ancestors });
    for (const child of children(node)) walk(child, [...ancestors, node]);
  };
  walk(ast.program);
  // Functions passed as callbacks are deferred. An immediately invoked body is
  // evaluated at module top level, including an IIFE wrapped in void/await.
  const top = node => {
    if (!node || typeof node !== 'object' || isFunction(node)) return;
    if (node.type === 'ImportExpression') topImports.add(node);
    if (node.type === 'CallExpression' || node.type === 'OptionalCallExpression') {
      const name = calleeName(node.callee);
      if (name) topCalls.push(name);
      if (isFunction(node.callee)) top(node.callee.body);
    }
    for (const child of children(node)) top(child);
  };
  top(ast.program);
  for (const node of ast.program.body) if (node.type === 'ImportDeclaration') {
    const target = clean((await resolve(node.source.value, importer))?.id ?? '');
    for (const spec of node.specifiers) bindings[spec.local.name] = { target, imported: spec.imported?.name ?? (spec.type === 'ImportNamespaceSpecifier' ? '*' : 'default') };
  }
  const counts = new Map(), edges = [];
  for (const { node, ancestors } of imports) {
    const specifier = node.source.value;
    const target = clean((await resolve(specifier, importer))?.id ?? '');
    const rule = Object.entries(STARTUP_EDGES).find(([source]) => target.endsWith('/' + source))?.[1];
    if (!rule) continue;
    const source = Object.keys(STARTUP_CALLS).find(source => clean(importer).endsWith('/' + source));
    const permitted = STARTUP_CALLS[source]?.filter(([, spec]) => spec === specifier).reduce((n, call) => n + (call[2] ?? 1), 0) ?? 0;
    const count = (counts.get(specifier) ?? 0) + 1;
    const enclosing = [...ancestors].reverse().find(isFunction);
    if (!permitted || count > permitted || !enclosing || topImports.has(node)) throw new Error(`Unaudited or top-level startup import call site: ${importer} -> ${specifier}`);
    const enclosingIndex = ancestors.lastIndexOf(enclosing);
    const declaration = ancestors.slice(0, enclosingIndex).reverse().find(parent => parent.type === 'VariableDeclarator' && parent.id.type === 'Identifier');
    const namedFunction = [...ancestors].reverse().find(parent => isFunction(parent) && (parent.id?.name || parent.key?.name));
    const loaders = [...new Set([declaration?.id.name, enclosing.id?.name, enclosing.key?.name, namedFunction?.id?.name, namedFunction?.key?.name].filter(Boolean))];
    if (!loaders.length) throw new Error(`Deferred import has no auditable loader: ${importer} -> ${specifier}`);
    counts.set(specifier, count);
    edges.push({ importer: clean(importer), target, specifier, loader: loaders[0], loaders, modes: rule.modes });
  }
  return { edges, topCalls, bindings };
}

export function validateStartupCalls(records) {
  for (const [owner, record] of records) for (const call of record.topCalls) {
    const [name, ...members] = call.split('.');
    const binding = record.bindings[name];
    const target = binding ? records.get(binding.target) : record;
    const loader = binding ? binding.imported === '*' ? members.shift() : binding.imported : name;
    if (target?.edges.some(edge => edge.loaders.includes(loader))) throw new Error(`Deferred loader/preload called at module top level: ${owner} -> ${call}`);
  }
}
export async function startupCallsites(code, importer, resolve) {
  const record = await startupSource(code, importer, resolve);
  validateStartupCalls(new Map([[clean(importer), record]]));
  return record.edges;
}

export function startupAudit() {
  const records = new Map();
  return {
    name: 'tessera-startup-callsite-audit', apply: 'build', enforce: 'pre',
    async transform(code, id) {
      if (!/\/src\/.*\.[jt]sx?$/.test(clean(id))) return;
      records.set(clean(id), await startupSource(code, id, (spec, owner) => this.resolve(spec, owner)));
    },
    generateBundle(_options, bundle) {
      validateStartupCalls(records);
      const chunks = Object.values(bundle).filter(out => out.type === 'chunk');
      const ids = out => new Set([out.facadeModuleId, ...(out.moduleIds ?? [])].filter(Boolean).map(clean));
      const proof = {};
      for (const owner of chunks) {
        const edges = [...ids(owner)].flatMap(id => records.get(id)?.edges ?? []);
        owner.tesseraStartupEdges = Object.fromEntries((owner.dynamicImports ?? []).flatMap(dep => {
          const target = bundle[dep];
          const matching = target ? edges.filter(edge => ids(target).has(edge.target)) : [];
          return matching.length ? [[dep, matching]] : [];
        }));
        proof[owner.fileName] = owner.tesseraStartupEdges;
      }
      this.emitFile({ type: 'asset', fileName: 'startup-edges.json', source: JSON.stringify(proof) });
    },
  };
}

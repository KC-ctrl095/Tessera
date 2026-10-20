// Rolldown drops custom chunk fields between hooks. Carry the audit's exact
// proof privately, bound to the final code as well as the chunk's owners.
import { createHash } from 'node:crypto';
import { parse } from '@babel/parser';

// Match the byte consumer's URL normalization (including query/fragment and
// percent decoding), so an eager alternate URL cannot retain an exemption.
function dependency(owner, specifier) {
  const url = new URL(specifier, `https://build.invalid/${owner}`);
  if (url.origin !== 'https://build.invalid') return null;
  try { return decodeURIComponent(url.pathname).replace(/^\//, ''); } catch { return null; }
}

function deferredCode(chunk) {
  const deferred = new Set(Object.entries(chunk.tesseraStartupEdges ?? {})
    .filter(([, sites]) => sites.length && sites.every(site => !site.modes.includes('static')))
    .map(([target]) => target));
  if (!deferred.size) return;
  const ast = parse(chunk.code, { sourceType: 'module', createImportExpressions: true });
  const functions = ['FunctionDeclaration', 'FunctionExpression', 'ArrowFunctionExpression', 'ObjectMethod', 'ClassMethod', 'ClassPrivateMethod'];
  const visit = (node, depth = 0, invoked = false) => {
    if (!node || typeof node !== 'object') return;
    // Direct IIFEs, constructors and .call/.apply invoke their bodies in the
    // caller's context, rather than establishing deferred function nesting.
    if (['CallExpression', 'OptionalCallExpression', 'NewExpression'].includes(node.type)) {
      let callee = node.callee;
      if (['MemberExpression', 'OptionalMemberExpression'].includes(callee?.type)) {
        const key = callee.computed ? callee.property?.value : callee.property?.name;
        if (['call', 'apply'].includes(key)) callee = callee.object;
      }
      if (['FunctionExpression', 'ArrowFunctionExpression'].includes(callee?.type)
        || node.type === 'NewExpression' && callee?.type === 'ClassExpression') {
        visit(callee, depth, true);
        node.arguments.forEach(arg => visit(arg, depth));
        return;
      }
    }
    if (node.type === 'ClassExpression' && invoked) {
      // Instantiation executes the constructor and field initializers now;
      // ordinary methods remain deferred until explicitly called.
      if (node.superClass) visit(node.superClass, depth);
      node.body.body.forEach(member => visit(member, depth, member.kind === 'constructor'));
      return;
    }
    if (functions.includes(node.type) && !invoked) depth++;
    if (node.type === 'ImportExpression') {
      const source = node.source;
      const value = source?.type === 'StringLiteral' ? source.value
        : source?.type === 'TemplateLiteral' && !source.expressions.length ? source.quasis[0].value.cooked : null;
      if (value && deferred.has(dependency(chunk.fileName, value)) && !depth) {
        throw new Error(`Eager startup import in final code: ${chunk.fileName} -> ${value}`);
      }
    }
    for (const [key, value] of Object.entries(node)) {
      if (key === 'loc' || key === 'extra') continue;
      if (Array.isArray(value)) value.forEach(child => visit(child, depth));
      else if (value && typeof value === 'object') visit(value, depth);
    }
  };
  visit(ast.program);
}

export function startupBudget(audit, budget) {
  let proofs;
  const identity = chunk => JSON.stringify([chunk.facadeModuleId ?? null, [...(chunk.moduleIds ?? [])].sort()]);
  const hash = chunk => createHash('sha256').update(chunk.code).digest('hex');
  const generate = typeof audit.generateBundle === 'function' ? audit.generateBundle : audit.generateBundle.handler;
  const write = budget.writeBundle.handler;
  return {
    audit: {
      ...audit,
      generateBundle: {
        order: 'post', sequential: true,
        async handler(options, bundle, ...args) {
          await generate.call(this, options, bundle, ...args);
          proofs = new Map(Object.values(bundle).filter(out => out.type === 'chunk').map(out => {
            deferredCode(out);
            return [out.fileName, { identity: identity(out), hash: hash(out), edges: structuredClone(out.tesseraStartupEdges) }];
          }));
        },
      },
    },
    budget: {
      ...budget,
      writeBundle: {
        ...budget.writeBundle,
        handler(options, bundle) {
          if (!proofs) throw new Error('Missing startup audit proof');
          for (const out of Object.values(bundle).filter(out => out.type === 'chunk')) {
            const proof = proofs.get(out.fileName);
            if (!proof || proof.identity !== identity(out)) throw new Error(`Changed startup audit owner: ${out.fileName}`);
            if (proof.hash !== hash(out)) throw new Error(`Changed startup audit code: ${out.fileName}`);
            out.tesseraStartupEdges = structuredClone(proof.edges);
          }
          return write.call(this, options, bundle);
        },
      },
    },
  };
}

// Named optional chunks share one repair path. Rollup also emits the bootstrap's
// helper URL directly, so even a URL-less failed helper import can be retried.
const sources = { RecallCard: '/components/chat/RecallCard.tsx', Rewind: '/components/chat/Rewind.tsx', voice: '/lib/voice/conversation.ts', TileOptions: '/components/chat/TileOptions.tsx', NotesListViews: '/components/notes/NotesListViews.tsx' };
export function recoverableParts() {
  let build = false, helper;
  return {
    name: 'tessera-recoverable-parts',
    configResolved(config) { build = config.command === 'build'; },
    buildStart() {
      if (build) helper = this.emitFile({ type: 'chunk', id: new URL('../src/lib/chunkRecovery.ts', import.meta.url).pathname });
    },
    transform(code, id) {
      if (!id.replaceAll('\\', '/').endsWith('/src/lib/retryImport.ts')) return;
      return code.replace('"__TESSERA_RECOVERY_URL__"', build ? `import.meta.ROLLUP_FILE_URL_${helper}` : `new URL('/src/lib/chunkRecovery.ts', location.href).href`);
    },
    generateBundle(_options, bundle) {
      const chunks = Object.values(bundle).filter(out => out.type === 'chunk');
      const parts = { entry: chunks.find(out => out.isEntry && /\/(?:index.html|src\/main.tsx)$/.test(out.facadeModuleId ?? ''))?.fileName, helper: this.getFileName(helper) };
      for (const [name, source] of Object.entries(sources)) {
        const entry = chunks.find(out => out.facadeModuleId?.replaceAll('\\', '/').endsWith(source));
        if (!entry) this.error(`Missing recoverable ${name} chunk`);
        const css = new Set(), seen = new Set();
        const visit = chunk => {
          if (!chunk || seen.has(chunk.fileName)) return;
          seen.add(chunk.fileName);
          for (const file of chunk.viteMetadata?.importedCss ?? []) css.add(file);
          for (const dep of chunk.imports ?? []) visit(bundle[dep]);
        };
        visit(entry);
        parts[name] = { file: entry.fileName, css: [...css].sort() };
      }
      this.emitFile({ type: 'asset', fileName: 'parts.json', source: JSON.stringify(parts) });
    },
    configureServer(server) {
      server.middlewares.use('/parts.json', (_req, res) => {
        res.setHeader('Content-Type', 'application/json');
        res.end(JSON.stringify({ entry: 'src/main.tsx', helper: 'src/lib/chunkRecovery.ts', ...Object.fromEntries(Object.entries(sources).map(([name, path]) => [name, { file: 'src' + path, css: [] }])) }));
      });
    },
  };
}

// Minimal already-loaded bootstrap. The retry URL is emitted by the bundler,
// never inferred from browser-specific import error text.
let failed = false;
const source = "__TESSERA_RECOVERY_URL__";
export async function retryImport<M>(load: () => Promise<M>): Promise<M> {
  if (!failed) {
    try { return await load(); }
    catch (error) { failed = true; throw error; }
  }
  const url = new URL(source, location.href);
  if (url.origin !== location.origin || !/\/(?:assets\/chunkRecovery-[\w-]+\.js|src\/lib\/chunkRecovery\.ts)$/.test(url.pathname)) throw new Error('Unsafe recovery path');
  url.searchParams.set('tessera_retry', String(Date.now()));
  return import(/* @vite-ignore */ url.href) as Promise<M>;
}

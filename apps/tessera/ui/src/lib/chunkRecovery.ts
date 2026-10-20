// Chunk repair is optional failure-path code. Never replace a stale embedded UI
// with random fresh modules: a real page reload is required for a new build.
export async function retryPart<M>(name: string, error: unknown, importer: () => Promise<M>): Promise<M> {
  const entry = document.querySelector<HTMLScriptElement>('script[type="module"][src]')?.src;
  if (!entry) return importer();
  const built = /\/assets\//.test(entry);
  const base = new URL(built ? '../' : '/', entry);
  const response = await fetch(new URL('parts.json', base), { cache: 'no-store' });
  if (!response.ok) throw new Error('Could not check the code and styles. Try again when the connection is back.');
  const parts = await response.json();
  if (built && new URL(parts.entry, base).href !== entry) throw Object.assign(new Error('Tessera was updated. Reload to get its files.'), { build: 'updated' });
  const part = parts[name] as { file: string; css: string[] } | undefined;
  if (!part) throw new Error('Could not find this part of Tessera. Reload to get its files.');
  const fresh = (file: string) => {
    const url = new URL(file, base);
    if (url.origin !== location.origin || !url.pathname.startsWith(base.pathname + (built ? 'assets/' : 'src/'))) throw new Error('Unsafe chunk path');
    url.searchParams.set('tessera_retry', String(Date.now()));
    return url.href;
  };
  const added: HTMLLinkElement[] = [];
  try {
    await Promise.all(part.css.map(file => {
      const original = new URL(file, base).href;
      const old = [...document.querySelectorAll<HTMLLinkElement>('link[rel="stylesheet"]')].filter(link => new URL(link.href).pathname === new URL(original).pathname);
      const failedCss = String((error as Error)?.message ?? error).includes(original);
      if (!failedCss && old.some(link => link.sheet)) return Promise.resolve();
      // Vite marks a failed preload as seen. Remove that failed link and await
      // an uncached CSS request before publishing the recovered JS exports.
      old.forEach(link => link.remove());
      return new Promise<void>((resolve, reject) => {
        const link = document.createElement('link');
        link.rel = 'stylesheet'; link.href = fresh(file);
        link.onload = () => resolve();
        link.onerror = () => reject(new Error(`Unable to load styles: ${link.href}`));
        added.push(link); document.head.append(link);
      });
    }));
    return await import(/* @vite-ignore */ fresh(part.file)) as M;
  } catch (error) {
    added.forEach(link => link.remove());
    throw error;
  }
}

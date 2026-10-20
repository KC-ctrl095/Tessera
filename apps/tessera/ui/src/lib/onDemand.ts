// Non-suspending optional parts: loading a mid-turn card never blanks its neighbors.
import { createSignal } from "solid-js";
import { retryImport } from "./retryImport";

export type BuildState = "updated" | "unchanged" | "unknown";
export interface PartFailure { message: string; build: BuildState }
const entry = (doc: Document) => doc.querySelector<HTMLScriptElement>('script[type="module"][src]')?.getAttribute('src');
const build = entry(document);
export async function updatedBuild(): Promise<BuildState> {
  try {
    const response = await fetch(location.href, { cache: 'no-store', headers: { Accept: 'text/html' }, signal: AbortSignal.timeout(2500) });
    if (!response.ok) return 'unknown';
    const fresh = entry(new DOMParser().parseFromString(await response.text(), 'text/html'));
    return !build || !fresh ? 'unknown' : build === fresh ? 'unchanged' : 'updated';
  } catch { return 'unknown'; } // Offline is not evidence that the build is unchanged.
}
const recovery = () => import('./chunkRecovery');
export interface OnDemand<M> {
  get: () => M | null;
  error: () => PartFailure | null;
  load: () => Promise<M | null>;
  retry: () => Promise<M | null>;
}
export function onDemand<M>(importer: () => Promise<M>, name?: string): OnDemand<M> {
  const [mod, setMod] = createSignal<M | null>(null);
  const [error, setError] = createSignal<PartFailure | null>(null);
  let loading: Promise<M | null> | null = null;
  let failure: unknown;
  let updated = false;
  const probe = async (known?: BuildState) => {
    const result = known ?? await updatedBuild();
    updated ||= result === 'updated';
    return updated ? 'updated' : result;
  };
  const start = (next: () => Promise<M>) => {
    if (loading) return loading;
    setError(null);
    return (loading = Promise.resolve().then(next).then(m => {
      setMod(() => m); return m;
    }, async e => {
      failure ??= e;
      console.warn("couldn't load a part of the app", e);
      setError({ message: String(e?.message ?? e), build: await probe(e?.build) });
      loading = null;
      return null;
    }));
  };
  const load = () => start(importer);
  const retry = () => start(async () => {
    if (await probe() === 'updated') throw Object.assign(new Error('Tessera was updated. Reload to get its files.'), { build: 'updated' });
    return name ? (await retryImport(recovery)).retryPart<M>(name, failure, importer) : importer();
  });
  return { get: mod, error, load, retry };
}

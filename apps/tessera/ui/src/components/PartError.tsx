import { For, Show, createSignal } from "solid-js";
import { errorMessage } from "../lib/ui";
import { discardPrivateDraft, draftBackup, openPrivateDraft, persistDraftsForReload, privateReloadDrafts, reloadWarning } from "../lib/workspace";
import { copyText } from "../lib/util";
import { saveText } from "../lib/files";
import type { PartFailure } from "../lib/onDemand";

/** A stale embedded deployment cannot recover by mixing old/new module graphs. */
export function PartError(props: { error: PartFailure | null; retry: () => unknown }) {
  const [saveError, setSaveError] = createSignal("");
  const [pickedPrivate, setPickedPrivate] = createSignal<string[]>([]);
  const reload = () => {
    try { persistDraftsForReload(); location.reload(); }
    catch (error) { setSaveError(errorMessage(error)); }
  };
  const backup = () => draftBackup(!!saveError(), pickedPrivate());
  return <div class="form-error part-error" role="alert"><div>
    <p>{props.error?.build === 'updated' ? "Tessera was updated." : `Couldn't load this part of Tessera: ${props.error?.message}`}</p>
    <Show when={props.error?.build !== 'updated'}><button type="button" class="btn part-retry" onClick={() => void props.retry()}>Retry</button>{" "}</Show>
    <button type="button" class="btn part-reload" onClick={reload}>{reloadWarning() || saveError() ? 'Reload safely' : 'Reload (drafts kept)'}</button>
    <Show when={reloadWarning() || saveError()}><p>{reloadWarning() || saveError()} Reload is blocked until your drafts can be kept.</p>
      <For each={privateReloadDrafts()}>{draft => <div class="private-reload-draft" data-draft={draft.identity}>
        <p>Private draft in {draft.title}{!draft.canOpen ? ' (another server)' : ''}</p>
        <button type="button" class="btn part-open-private" disabled={!draft.canOpen} title={!draft.canOpen ? 'Switch to its server first.' : undefined} onClick={() => openPrivateDraft(draft.identity)}>Open</button>{" "}
        <button type="button" class="btn part-discard-private" onClick={() => discardPrivateDraft(draft.identity)}>Discard</button>
        <label><input type="checkbox" checked={pickedPrivate().includes(draft.identity)} onChange={e => setPickedPrivate(list => e.currentTarget.checked ? [...list, draft.identity] : list.filter(id => id !== draft.identity))} /> Include this private draft in the backup</label>
      </div>}</For>
      <Show when={pickedPrivate().length}><p>This backup includes the selected private drafts. Save it somewhere private.</p></Show>
      <button type="button" class="btn part-copy" disabled={reloadWarning().startsWith('Still preparing')} onClick={() => void copyText(backup()).then(ok => !ok && setSaveError('Copy failed. Save a draft backup instead.'))}>Copy draft backup</button>{" "}
      <button type="button" class="btn part-save" disabled={reloadWarning().startsWith('Still preparing')} onClick={() => void saveText('tessera-draft-backup.json', 'application/json', backup())}>Save draft backup</button>
    </Show>
  </div></div>;
}

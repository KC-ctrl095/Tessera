import { check, device, finish, mock, run, signIn } from './cdp.mjs';
run(async () => {
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  const p = await device(); await p.viewport(1440,900); await signIn(p,'Stale RPCs');
  const r = await p.eval(`Promise.all([__mod('lib/store.ts'),__mod('lib/workspace.ts'),__mod('lib/mosaicSync.ts')]).then(async ([s,w,m]) => {
    await s.bootstrap(); await new Promise(r=>setTimeout(r,150));
    const original = {...s.api};
    const base = await s.api.getSession('ses_7d3a91e5c4b2');
    const pending = {};
    s.api.getSession = id => new Promise(resolve => pending[id]=resolve);
    const load = s.loadSession('late-load',false);
    s.peekSession('late-peek');
    s.api.listProviders = () => new Promise(resolve => pending.providers=resolve);
    const providers = s.refreshProviders();
    s.api.listSessions = () => new Promise(resolve => pending.archived=resolve);
    // New servers page the archive (list_session_page); old ones list it at once.
    s.api.listSessionPage = () => new Promise(resolve => pending.archived = list => resolve({sessions:list,next_cursor:null}));
    const archived = s.refreshArchived();
    s.api.saveMosaics = () => new Promise(resolve => pending.layout=resolve);
    w.openSession('ses_7d3a91e5c4b2',{newPane:true});
    const saving = m.syncLayoutNow(5000);
    await new Promise(r=>setTimeout(r,50));
    const hadSave = !!pending.layout;
    Object.assign(s.api, original);
    s.resetServerState(); w.loadWorkspace('synthetic-other-server');
    await s.bootstrap(); await new Promise(r=>setTimeout(r,150));
    const before = JSON.stringify(w.getSyncState());
    pending['late-load']({...base,info:{...base.info,id:'late-load',title:'Wrong server'}});
    pending['late-peek']({...base,info:{...base.info,id:'late-peek',title:'Wrong server'}});
    pending.providers([{id:'wrong-provider',models:[]}]);
    pending.archived([{...base.info,id:'wrong-archive'}]);
    pending.layout?.({version:999,mosaics:[{id:'wrong-layout',name:'Wrong server',layout:'dwindle',tiles:[]}]});
    await Promise.all([load,providers,archived,saving]);
    await new Promise(r=>setTimeout(r,30));
    return {hadSave,load:!s.state.sessions['late-load'],peek:!s.state.sessions['late-peek'],providers:!s.state.providers.some(p=>p.id==='wrong-provider'),archived:!s.state.archived?.some(p=>p.id==='wrong-archive'),layout:JSON.stringify(w.getSyncState())===before};
  })`);
  for (const key of ['hadSave','load','peek','providers','archived','layout']) check(r[key],`stale server RPC: ${key}`,r);
  // An old get_mosaics must not import into the next server either.
  const boot = await p.eval(`Promise.all([__mod('lib/store.ts'),__mod('lib/workspace.ts')]).then(async ([s,w]) => {
    const original = s.api.getMosaics; let resolve;
    s.api.getMosaics = () => new Promise(r=>resolve=r);
    await s.bootstrap();
    s.api.getMosaics = original;
    s.resetServerState(); w.loadWorkspace('synthetic-third-server'); await s.bootstrap();
    await new Promise(r=>setTimeout(r,100)); const before=JSON.stringify(w.exportMosaics());
    resolve({version:999,mosaics:[{id:'stale-get',name:'Old server',layout:'dwindle',tiles:[{id:'old-tile',session_id:'late-load'}]}]});
    await new Promise(r=>setTimeout(r,50));
    return JSON.stringify(w.exportMosaics())===before;
  })`);
  check(boot,'stale initial layout response is dropped before importing tiles');
  await p.close(); finish();
});

// Cheap metadata/legacy compatibility. Heavy real-server launch measurements
// are opt-in in launch-real.mjs and scripts/launch-perf.sh.
import {check, device, finish, run, signIn, mock} from './cdp.mjs';
run(async()=>{
  await mock('/__mock/reset');
  const p=await device(); await signIn(p,'Session catalog');
  const result=await p.eval(`(async()=>{
    const s=await __mod('lib/store.ts');
    const legacy=await s.api.listSessions(false);
    const job=legacy.find(i=>i.kind==='job');
    const first=await s.api.listSessionPage(false,null,job?[job.id]:[]);
    const all=[]; let next=first.next_cursor;
    all.push(...first.sessions);
    while(next) {const page=await s.api.listSessionPage(false,next);all.push(...page.sessions);next=page.next_cursor;}
    const summaries=new Map(all.map(i=>[i.id,i]));
    return {
      negotiated:!!s.state.server.session_list_pages,
      noTasks:all.every(i=>!i.task),
      includesJob:!job||first.sessions.some(i=>i.id===job.id),
      catalogComplete:legacy.filter(i=>i.kind==='chat'||i.kind==='channel').every(i=>summaries.has(i.id)),
      fieldsPreserved:legacy.filter(i=>summaries.has(i.id)).every(i=>{const a=summaries.get(i.id);return a.title===i.title&&a.pinned===i.pinned&&a.archived===i.archived&&a.bot_id===i.bot_id&&a.status===i.status&&a.last_seq===i.last_seq&&JSON.stringify(a.usage)===JSON.stringify(i.usage);}),
      legacyHasJob:!!job,
    };
  })()`);
  check(result.negotiated,'new server advertises metadata paging',result);
  check(result.noTasks,'list summaries omit task ledgers');
  check(result.includesJob,'explicit open job run is included');
  check(result.catalogComplete,'all older chats, bot chats and channels remain reachable');
  check(result.fieldsPreserved,'titles, pins, unread sequence, usage, status and bot identity survive projection');
  check(result.legacyHasJob,'legacy list retains job runs');
  check(!p.errors.length,'no browser errors',p.errors);
  await p.close(); finish();
});

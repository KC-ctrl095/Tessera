// A paired page restores Notes before any user action. An empty library must
// not load its deferred row/filter chunk; arriving and initial notes do load it.
import { check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
run(async () => {
  for (const initial of [false, true]) {
    await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
    const probe = await wsClient('Notes startup probe');
    if (initial) await probe.rpc('save_note', { title: 'Initial startup note', content: 'Present on cold launch' });
    const p = await device(); await p.viewport(1280, 860);
    await signIn(p, 'Persisted Notes tab');
    await p.eval(`localStorage.setItem('tessera.sidebar.tab', JSON.stringify('notes'))`);
    const requests = [];
    p.on('Network.requestWillBeSent', e => requests.push(e.request.url));
    await p.send('Network.enable'); await p.send('Network.setCacheDisabled', { cacheDisabled: true });
    await p.reload();
    await p.waitFor(`document.querySelector('.app') && document.querySelector('.sidebar .notes-list')`);
    check(await p.eval(`localStorage.getItem('tessera.token') !== null && JSON.parse(localStorage.getItem('tessera.sidebar.tab')) === 'notes'`), `${initial}: cold paired page restores persisted Notes tab`);
    if (!initial) {
      await p.waitFor(`document.querySelector('.sidebar .notes-list').textContent.includes('No notes yet')`);
      await sleep(300);
      check(!requests.some(url => /\/NotesListViews\.tsx/.test(url)), 'empty persisted Notes tab never requests deferred views');
      check(!await p.eval(`!!document.querySelector('.sidebar .nl-filter')`), 'empty state is rendered cheaply without row/filter controls');
      await probe.rpc('save_note', { title: 'Arriving startup note', content: 'Arrives after startup' });
    }
    await p.waitFor(`document.querySelector('.sidebar .nrow .srow-main') && document.querySelector('.sidebar .nl-filter')`);
    check(requests.some(url => /\/NotesListViews\.tsx/.test(url)), `${initial}: notes present trigger views module request`);
    check(await p.eval(`document.querySelector('.sidebar .nrow').textContent.includes(${JSON.stringify(initial ? 'Initial startup note' : 'Arriving startup note')})`), `${initial}: note rows actually render`);
    check(!p.errors.length, `${initial}: no Notes startup errors`, p.errors);
    probe.close(); await p.close();
  }
  finish();
});

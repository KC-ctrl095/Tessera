#!/usr/bin/env python3
"""Synthetic launch store. Run only against a fresh throwaway `tessera serve` store.
No production files are read. --full matches the 489,230 entry / 2.9 GB shape.
Without --full, session metadata is identical but only the five large chats have history.
"""
import argparse
import json
import pathlib
import sqlite3
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('home', type=pathlib.Path)
p.add_argument('--full', action='store_true')
a = p.parse_args()
home = a.home.resolve()
assert (home / 'LAUNCH-PERF-THROWAWAY').is_file(), 'Create the throwaway marker first'
c = sqlite3.connect(home / 'tessera.db')
assert c.execute('select count(*) from sessions').fetchone()[0] == 0, 'Refusing a nonempty store'
js = lambda v: json.dumps(v, separators=(',', ':'), ensure_ascii=False)
now = 1792886400000
model = {'provider': 'synthetic', 'model': 'synthetic'}
usage = dict(input=12000, output=3000, cache_read=9000, cache_write=1000)
ledger = dict(goal='Verify the synthetic project', constraints=['Keep all example files synthetic'],
              plan=[dict(text='Check the synthetic build', status='done')],
              decisions=['Keep the measured change small'], open_questions=[], artifacts=['/synthetic/report'],
              notes=['Synthetic evidence. ' * 20], done_criteria=['The example tests pass'], criteria=[])
counts = [('chat', 412), ('job', 3384), ('subagent', 2971), ('channel', 7)]
# A heavy long-term user: chats, scheduled-job runs, helpers, channels.
large = [17844, 12648, 11480, 7711, 6706]
entry_counts = large + [0] * (sum(n for _, n in counts) - 5)
if a.full:
    left = 489230 - sum(large)
    for i in range(5, len(entry_counts)):
        entry_counts[i] = left // (len(entry_counts) - 5) + int(i - 5 < left % (len(entry_counts) - 5))
body_total = info_total = total = 0
start = time.monotonic()
with c:
    idx = 0
    for kind, count in counts:
        for i in range(count):
            sid = f'perf-{kind}-{i:04d}'
            n = entry_counts[idx]
            ts = now - idx * 60000
            task = {**ledger, 'notes': ['Synthetic evidence. ' * (500 if kind == 'chat' and i < 5 else 20)]}
            info = dict(id=sid, revision=1, title=f'Synthetic {kind} {i:04d}', kind=kind,
                        cwd='/synthetic', model=model, thinking='auto', permission='ask', status='idle',
                        created_at=ts-86400000, updated_at=ts, last_seq=n, usage=usage,
                        context=dict(tokens=24000, window=128000, compactions=3),
                        pinned=kind == 'chat' and i in (0, 411), archived=False,
                        preview=f'Synthetic {kind} preview {i}. ' + 'Measured example content. ' * 3,
                        task=task, private=False, steps=n//3, fast=False, members=[])
            if kind == 'subagent':
                info.update(parent_id=f'perf-chat-{i % 412:04d}', parent_call=f'call-{i}', helper_owner='tessera')
            raw = js(info)
            info_total += len(raw)
            c.execute('insert into sessions(id,info,internal,updated_at,archived,parent_id) values(?,?,\'\',?,0,?)',
                      (sid, raw, ts, info.get('parent_id')))
            c.execute('insert into session_state(id,internal,revision) values(?,?,1)', (sid, '{"titled":true}'))
            if kind == 'job':
                c.execute('insert into job_runs values(?,?,?,?,?)', (sid, f'perf-jobdef-{i%12}', ts, ts+1000, 'ok'))
            # Tool results dominate long histories' bytes. Keep real structured turns,
            # not one huge notice. The newest answer stays short and visibly identifiable.
            target = (65_000_000 if idx == 0 else 45_000_000 if idx < 5 else 6200 * n)
            size = max(80, int(target / max(n, 1) * 3) - 1100)
            rows = []
            for seq in range(1, n + 1):
                if seq == n:
                    body = dict(kind='assistant', model=model, usage=usage, stop='stop',
                                content=[dict(type='text', text=f'Newest synthetic message {sid}.')])
                elif seq % 3 == 1:
                    body = dict(kind='user', content=[dict(type='text', text=f'Check synthetic file {seq}.')])
                elif seq % 3 == 2:
                    body = dict(kind='assistant', model=model, usage=usage, stop='tool_use',
                                content=[dict(type='tool_call', id=f'call-{seq}', name='read', arguments={'path': f'/synthetic/{seq}.txt'})])
                else:
                    body = dict(kind='tool_result', call_id=f'call-{seq-1}', tool='read', is_error=False,
                                content=[dict(type='text', text=('Synthetic tool output line.\n' * (size//28 + 1))[:size])], details={'duration_ms': 20})
                raw_body = js(body)
                body_total += len(raw_body)
                rows.append((sid, seq, ts+seq, body['kind'], raw_body, f'Synthetic file {seq}'))
                if len(rows) == 500:
                    c.executemany('insert into entries(session_id,seq,ts,kind,body,text) values(?,?,?,?,?,?)', rows)
                    rows.clear()
            c.executemany('insert into entries(session_id,seq,ts,kind,body,text) values(?,?,?,?,?,?)', rows)
            total += n
            idx += 1
    for i in range(12):
        job = dict(id=f'perf-jobdef-{i}', name=f'Synthetic schedule {i}', schedule='@every 1d',
                   prompt='Check a synthetic example', cwd='/synthetic', enabled=False, permission='ask',
                   last_session=f'perf-job-{3372+i:04d}', last_status='ok', runs=282)
        c.execute('insert into jobs values(?,?)', (job['id'], js(job)))
    # No inherited credentials, external model requests, scheduler or Claude CLI.
    c.execute("insert or replace into meta values('fts_upto', (select coalesce(max(id),0) from entries))")
    c.execute("insert into entries_fts(entries_fts) values('rebuild')")
    c.execute("insert or replace into meta values('setup_completed','1')")
    c.execute("insert or replace into meta values('usage_backfilled','1')")
c.execute('pragma wal_checkpoint(truncate)')
c.close()
summary = dict(sessions=idx, entries=total, infoBytes=info_total, bodyBytes=body_total,
               full=a.full, seconds=round(time.monotonic()-start, 2))
(home/'shape.json').write_text(json.dumps(summary, indent=2)+'\n')
print(json.dumps(summary))

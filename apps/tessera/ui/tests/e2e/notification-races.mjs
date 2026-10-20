// Response ordering in the notification center without clocks, network timing or real messages.
// Runs within notification-inbox.mjs; not a suite of its own.
import { check } from "./cdp.mjs";

export async function notificationRaces(p) {
  const results = await p.eval(`(async () => {
    const [inbox, { backend }] = await Promise.all([__mod('lib/notificationInbox.ts'), __mod('lib/backend.ts')]);
    const original = backend.call;
    const results = [];
    let pending = [];
    const item = { id: -21001, title: 'Synthetic ordering fixture', body: '', from: '', urgent: false, created_at: Date.now(), due_at: Date.now(), sent: true, seen: false };
    const seed = () => { pending = []; inbox.setInboxItems([{ ...item }]); };
    const take = (method) => {
      const i = pending.findIndex((p) => p.method === method);
      if (i < 0) throw Error('Expected in-flight ' + method);
      return pending.splice(i, 1)[0];
    };
    backend.call = function(request) {
      if (['seen_notifications', 'list_notification_inbox', 'clear_notifications'].includes(request.method)) {
        return new Promise((resolve, reject) => pending.push({ method: request.method, resolve, reject }));
      }
      return original.call(this, request);
    };
    try {
      seed();
      const read = inbox.readNotifications([item.id]);
      const readReply = take('seen_notifications');
      const refresh = inbox.refreshInbox();
      const staleReply = take('list_notification_inbox');
      readReply.resolve(null); await read;
      results.push(['a read is shown once the server confirms it', inbox.inboxItems()[0].seen]);
      staleReply.resolve([{ ...item }]); await refresh;
      results.push(['a list sent before the read landed cannot undo it', inbox.inboxItems()[0].seen]);

      seed();
      const failedRead = inbox.readNotifications([item.id]).then(() => false, () => true);
      take('seen_notifications').reject(new Error('synthetic refusal'));
      results.push(['a refused read stays unread', await failedRead && !inbox.inboxItems()[0].seen]);

      seed();
      const clear = inbox.clearNotifications([item.id]);
      const clearReply = take('clear_notifications');
      const during = inbox.refreshInbox();
      const duringReply = take('list_notification_inbox');
      clearReply.resolve(null); await clear;
      duringReply.resolve([{ ...item }]); await during;
      results.push(['a cleared item does not come back with an older list', inbox.inboxItems().length === 0]);

      // A delayed mutation must not be the last revision writer. New
      // activity in an intervening snapshot survives the final refetch,
      // even when that older snapshot arrives after the mutation reply.
      for (const method of ['readNotifications', 'clearNotifications']) {
        seed();
        const newer = { ...item, id: item.id - 1, title: 'New activity during mutation' };
        const mutation = inbox[method]([item.id]);
        const reply = take(method === 'readNotifications' ? 'seen_notifications' : 'clear_notifications');
        const intervening = inbox.refreshInbox();
        const interveningReply = take('list_notification_inbox');
        reply.resolve(null); await mutation;
        const authoritative = take('list_notification_inbox');
        interveningReply.resolve([{ ...item }, newer]); await intervening;
        const final = method === 'readNotifications' ? [{ ...item, seen: true }, newer] : [newer];
        authoritative.resolve(final);
        await new Promise(resolve => queueMicrotask(() => queueMicrotask(resolve)));
        results.push([method + ' refetches after its reply and retains new activity', inbox.inboxItems().some(n => n.id === newer.id && !n.seen)]);
        results.push([method + ' final snapshot preserves the confirmed mutation', method === 'readNotifications' ? inbox.inboxItems().find(n => n.id === item.id)?.seen : !inbox.inboxItems().some(n => n.id === item.id)]);
      }
      seed();
      const oldList = inbox.refreshInbox();
      const oldListReply = take('list_notification_inbox');
      const newerList = inbox.refreshInbox();
      take('list_notification_inbox').resolve([{ ...item, seen: true }]); await newerList;
      oldListReply.reject(new Error('synthetic old failure')); await oldList;
      results.push(['an old list failing cannot erase a newer one', inbox.inboxStatus() === 'ready' && inbox.inboxItems()[0].seen]);

      seed();
      const oldRead = inbox.readNotifications([item.id]);
      const oldReadReply = take('seen_notifications');
      const oldRefresh = inbox.refreshInbox();
      const oldRefreshReply = take('list_notification_inbox');
      inbox.resetInbox();
      inbox.setInboxItems([{ ...item }]); // the same id on another server
      oldReadReply.resolve(null); await oldRead;
      results.push(["one server's read cannot mark another's item read", !inbox.inboxItems()[0].seen]);
      inbox.resetInbox();
      oldRefreshReply.resolve([{ ...item }]); await oldRefresh;
      results.push(["one server's list cannot come back after a reset", inbox.inboxItems().length === 0 && !inbox.inboxOpen()]);
      return results;
    } finally {
      backend.call = original;
      inbox.resetInbox();
      await inbox.refreshInbox();
    }
  })()`);
  for (const [label, passed] of results) check(passed, label);
}

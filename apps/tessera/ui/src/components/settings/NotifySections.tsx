// Settings → Notifications: agents' messages (`notify`) — which devices get
// them, quiet hours, what is scheduled, and a test from the server that
// proves they arrive with the app closed.
import { For, Show, createResource, createSignal, onCleanup } from "solid-js";
import type { NotificationDelivery } from "../../proto/NotificationDelivery";
import type { DeviceInfo } from "../../proto/DeviceInfo";
import type { UserNotification } from "../../proto/UserNotification";
import { browserNotificationsOn, setBrowserNotifications } from "../../lib/alerts";
import { isCheckAck } from "../../lib/completion";
import { ROLE, clientOf } from "../../lib/devices";
import { api, call } from "../../lib/store";
import { toast, toastError } from "../../lib/ui";
import { fmtDateTime, isTauri, isTauriMobile, relTime } from "../../lib/util";
import { GuideLink } from "../Guide";
import { Icon } from "../icons";
import { Toggle, openMenu } from "../primitives";
import { Card, Row, ThisDevice } from "./kit";

const setNotify = (id: string, notify: boolean) => call<DeviceInfo[]>({ method: "set_device_notify", params: { id, notify } });

/**
 * Where agents' messages go: this device first, then the others. Stored on
 * the server, so any device can change it for all of them.
 */
export function AgentMessagesCard() {
  const [devices, { mutate }] = createResource(() => api.listDevices().catch(() => [] as DeviceInfo[]));
  const set = async (d: DeviceInfo, on: boolean) => {
    const before = devices();
    mutate(before?.map((x) => (x.id === d.id ? { ...x, notify: on } : x)));
    try {
      mutate(await setNotify(d.id, on));
    } catch (e) {
      mutate(before);
      toastError(e, "Could not save");
    }
  };
  // A login that is only a machine never shows notifications.
  const ordered = () => [...(devices() ?? [])].filter((d) => clientOf(d) !== "machine").sort((a, b) => Number(b.current) - Number(a.current));
  /** Its name, and what it is when another one has the same name (a computer's app and terminal). */
  const label = (d: DeviceInfo) => ((devices() ?? []).some((x) => x.id !== d.id && x.name === d.name) ? `${d.name} · ${ROLE[clientOf(d)].label}` : d.name);
  const closedHelp = isTauriMobile
    ? "Ask an agent to “let me know when…” or “remind me at…”. With Tessera closed they arrive within about 15 minutes (Android checks in the background); while it's open, or with “Stay connected: always”, at once."
    : "Ask an agent to “let me know when…” or “remind me at…”. Each device below shows them as a notification.";
  return (
    <Card>
      <Row
        title="Messages from agents"
        help={
          <>
            {closedHelp} <GuideLink name="notifications">How it works</GuideLink>
          </>
        }
      />
      <ul class="notify-devices" aria-label="Devices that get agents' messages">
        <For each={ordered()} fallback={<li class="setting-help">No devices yet.</li>}>
          {(d) => (
            <li>
              <Icon name={ROLE[clientOf(d)].icon} size={16} />
              <span class="grow">
                <span class="name">
                  {label(d)}
                  <Show when={d.current}> · this device</Show>
                </span>
                <span class="sub">{d.last_seen ? `Active ${relTime(d.last_seen)}` : "Never connected"}</span>
              </span>
              <Toggle checked={d.notify} onChange={(v) => void set(d, v)} label={`Agents' messages on ${d.name}`} />
            </li>
          )}
        </For>
      </ul>
    </Card>
  );
}

/** Quiet hours (the apps): everything still arrives, without sound or pop-up. */
export function QuietHoursCard(props: {
  quiet: boolean;
  from: string;
  to: string;
  onChange: (p: { quiet?: boolean; quietFrom?: string; quietTo?: string }) => void;
}) {
  return (
    <Card>
      <Row
        title="Quiet hours"
        help={
          <>
            <ThisDevice /> Notifications still arrive, but without sound, vibration or pop-up.
          </>
        }
      >
        <Toggle checked={props.quiet} onChange={(v) => props.onChange({ quiet: v })} label="Quiet hours" />
      </Row>
      <Show when={props.quiet}>
        <div class="setting-row">
          <label class="quiet-times">
            From
            <input
              type="time"
              value={props.from}
              aria-label="Quiet from"
              onChange={(e) => e.currentTarget.value && props.onChange({ quietFrom: e.currentTarget.value })}
            />
            to
            <input
              type="time"
              value={props.to}
              aria-label="Quiet until"
              onChange={(e) => e.currentTarget.value && props.onChange({ quietTo: e.currentTarget.value })}
            />
          </label>
        </div>
      </Show>
    </Card>
  );
}

/** Reminders agents scheduled, with a way to cancel them. */
export function ScheduledCard() {
  const [list, { refetch, mutate }] = createResource(() =>
    call<UserNotification[]>({ method: "list_notifications" }).catch(() => [] as UserNotification[]),
  );
  const scheduled = () => (list() ?? []).filter((n) => !n.sent && !isCheckAck(n.title) && !isCheckAck(n.body));
  const cancel = async (n: UserNotification) => {
    mutate((l) => l?.filter((x) => x.id !== n.id));
    try {
      await call({ method: "cancel_notification", params: { id: n.id } });
    } catch (e) {
      toastError(e, "Could not cancel");
      void refetch();
    }
  };
  return (
    <Show when={scheduled().length}>
      <Card>
        <Row title="Scheduled" help="Reminders and messages agents set for later." />
        <ul class="notify-scheduled" aria-label="Scheduled notifications">
          <For each={scheduled()}>
            {(n) => (
              <li>
                <Icon name="clock" size={16} />
                <span class="grow">
                  <span class="name">{n.title}</span>
                  <span class="sub">
                    {fmtDateTime(n.due_at)}
                    <Show when={n.from}> · {n.from}</Show>
                  </span>
                </span>
                <button type="button" class="btn btn-sm" onClick={() => void cancel(n)}>
                  Cancel
                </button>
              </li>
            )}
          </For>
        </ul>
      </Card>
    </Show>
  );
}

/**
 * One "Send a test": a message from the server, now or in a minute (close Tessera to see it
 * arrive in the background). It travels the whole way a message from an agent does. In the
 * apps, `here` adds a notification from this device itself, shown even with Tessera in front
 * (a message from the server is a toast then), to check this device can show them.
 */
export function SendTest(props: { here?: () => void }) {
  const [busy, setBusy] = createSignal(false);
  const [testId, setTestId] = createSignal<number>();
  const [reports, { refetch }] = createResource(testId, (id) => call<NotificationDelivery[]>({ method: "notification_deliveries", params: { id } }).catch(() => []));
  const timer = setInterval(() => { if (testId()) void refetch(); }, 5000);
  onCleanup(() => clearInterval(timer));
  const send = async (delay: number) => {
    setBusy(true);
    try {
      const n = await call<UserNotification>({ method: "test_notification", params: { delay_secs: delay } });
      setTestId(n.id);
      if (delay)
        toast({
          level: "info",
          title: "Scheduled",
          message: isTauriMobile ? "Close Tessera now: the message arrives in about a minute." : "The message arrives in about a minute.",
        });
    } catch (e) {
      toastError(e, "Could not send a test");
    } finally {
      setBusy(false);
    }
  };
  return (
    <div class="notify-tests">
      <button
        type="button"
        class="btn"
        aria-haspopup="menu"
        disabled={busy()}
        onClick={(e) =>
          openMenu(
            e.currentTarget,
            [
              ...(props.here ? [{ label: "On this device", icon: isTauriMobile ? ("phone" as const) : ("monitor" as const), description: "Right away, even with Tessera in front", onSelect: props.here }] : []),
              { label: "Now", icon: "bell", description: "From your server, the way agents' messages come", onSelect: () => void send(0) },
              { label: "In a minute", icon: "clock", description: "Close Tessera to see it arrive in the background", onSelect: () => void send(60) },
            ],
            "bottom-start",
            { sheet: false },
          )
        }
      >
        <Icon name="bell" size={15} /> Send a test alert
        <Icon name="chevronDown" size={13} />
      </button>
      <Show when={testId()}>
        <div class="setting-help" role="status">
          <p>Test sent through the real alert path, even to devices turned off here. Phone permissions and Do Not Disturb still apply. A closed Android app checks within about 15 minutes, sometimes later in deep sleep. Delivered means the device accepted the alert, not that you read it.</p>
          <For each={reports()}>{(d) => <p>{d.name} · {d.delivered ? "Delivered to this device" : "Waiting for a device report"}</p>}</For>
        </div>
      </Show>
    </div>
  );
}

/** A browser shows agents' messages as system notifications while its tab is hidden. */
export function BrowserNotificationsCard() {
  const supported = !isTauri && "Notification" in window;
  const [on, setOn] = createSignal(browserNotificationsOn());
  const blocked = () => supported && Notification.permission === "denied";
  return (
    <Show when={supported}>
      <Card>
        <Row
          title="Show in this browser"
          help={blocked() ? "This browser blocks notifications from this page; allow them in its site settings." : "While this tab is in the background. Otherwise they appear in the app itself."}
        >
          <Toggle
            checked={on()}
            onChange={(v) =>
              void setBrowserNotifications(v).then((r) => {
                setOn(r);
                if (v && !r) toast({ level: "warning", message: "The browser didn't allow notifications." });
              })
            }
            label="Show in this browser"
          />
        </Row>
      </Card>
    </Show>
  );
}

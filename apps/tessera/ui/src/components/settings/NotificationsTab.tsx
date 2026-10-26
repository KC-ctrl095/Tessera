// Settings → Notifications: what this device tells you about when Tessera isn't in front.
import { For, Show, createResource, createSignal } from "solid-js";
import { toastError } from "../../lib/ui";
import { isTauri, isTauriMobile } from "../../lib/util";
import { Icon } from "../icons";
import {
  batteryOptimizationRequest,
  notificationsGet,
  notificationsOpenSettings,
  notificationsSet,
  notificationsTest,
  type NotificationSettings,
  type NotificationsState,
} from "../../lib/native";
import { Toggle } from "../primitives";
import { NotificationPolicyCard } from "./NotificationPolicyCard";
import { AgentMessagesCard, BrowserNotificationsCard, ScheduledCard, SendTest } from "./NotifySections";
import { Card, LoadingBlock, Row, ThisDevice } from "./kit";

export function NotificationsTab() {
  // A browser: agents' messages only (the rest needs the app).
  if (!isTauri)
    return (
      <div class="stack">
        <BrowserNotificationsCard />
        <NotificationPolicyCard />
        <AgentMessagesCard />
        <ScheduledCard />
        <SendTest />
      </div>
    );
  const [st, { mutate }] = createResource(() => notificationsGet());
  const [busy, setBusy] = createSignal(false);
  const settingsOf = (x: NotificationsState): NotificationSettings => ({
    enabled: x.enabled,
    approvals: x.approvals,
    finished: x.finished,
    subagents: x.subagents,
    jobs: x.jobs,
    errors: x.errors,
    previews: x.previews,
    keepAlive: x.keepAlive,
    quiet: x.quiet,
    quietFrom: x.quietFrom,
    quietTo: x.quietTo,
  });
  const save = async (patch: Partial<NotificationSettings>) => {
    const cur = st();
    if (!cur) return;
    mutate({ ...cur, ...patch });
    try {
      // The shell asks the system for permission when notifications are turned on
      // (never before), and offers the battery exemption when keep-alive becomes "always".
      const next = await notificationsSet({ ...settingsOf(cur), ...patch });
      if (next) mutate(next);
    } catch (e) {
      mutate(cur);
      toastError(e, "Could not save");
    }
  };
  const run = async (f: () => Promise<NotificationsState | null>, what: string) => {
    setBusy(true);
    try {
      const next = await f();
      if (next) mutate(next);
    } catch (e) {
      toastError(e, what);
    } finally {
      setBusy(false);
    }
  };
  const KEEP: { value: NotificationSettings["keepAlive"]; label: string; help: string }[] = [
    { value: "while_busy", label: "While a task is running (recommended)", help: "Stays connected during a task and 2 minutes after, so you still hear when it's done." },
    { value: "always", label: "Always stay connected (uses more battery)", help: "Hear about everything at once, at any time, including agents' messages." },
    { value: "off", label: "Off", help: "No live background connection. A closed app still checks for alerts about every 15 minutes." },
  ];
  return (
    <Show when={st()} fallback={<LoadingBlock error={st.error} />}>
      {(n) => (
        <div class="stack">
          <Show when={n().permission === "denied"}>
            <div class="form-error warn">
              <Icon name="alert" size={14} />
              <span>
                Notifications are turned off for Tessera in the system settings.{" "}
                <Show when={isTauriMobile}>
                  <button type="button" class="link-btn" onClick={() => void notificationsOpenSettings().catch((e) => toastError(e))}>
                    Open notification settings
                  </button>
                </Show>
              </span>
            </div>
          </Show>
          <Card>
            <Row
              title="Notifications"
              col
              help={
                <>
                  <ThisDevice /> Approvals, things you asked to hear about, and replies that finish while you're away. No progress or helper alerts.
                </>
              }
            >
              <select
                class="input"
                style={{ width: "auto", "min-height": "44px", "max-width": "100%" }}
                aria-label="Notifications"
                value={!n().enabled ? "off" : n().finished ? "on" : "needed"}
                onChange={(e) => void save({ enabled: e.currentTarget.value !== "off", finished: e.currentTarget.value === "on", approvals: true, errors: true, jobs: true, subagents: false })}
              >
                <option value="on">On</option>
                <option value="needed">Only when I'm needed</option>
                <option value="off">Off</option>
              </select>
            </Row>
          </Card>
          <NotificationPolicyCard />
          <Show when={n().enabled}>
            <AgentMessagesCard />
            <Card>
              <Row
                title="Show message previews"
                help={
                  <>
                    <ThisDevice /> Message content stays hidden unless you turn this on. Private chats always stay hidden.
                  </>
                }
              >
                <Toggle checked={n().previews} onChange={(v) => void save({ previews: v })} label="Show message previews" />
              </Row>
            </Card>
            <ScheduledCard />
            <Show when={n().keepAliveAvailable}>
              <Card>
                <div class="setting-row stacked">
                  <div class="setting-text">
                    <div class="setting-title">Stay connected in the background</div>
                    <div class="setting-help">Android stops apps in the background, and then notifications can't arrive.</div>
                  </div>
                  <div class="keep-choices" role="radiogroup" aria-label="Stay connected in the background">
                    <For each={KEEP}>
                      {(o) => (
                        <button
                          type="button"
                          role="radio"
                          class="choice-card"
                          classList={{ on: n().keepAlive === o.value }}
                          aria-checked={n().keepAlive === o.value}
                          onClick={() => void save({ keepAlive: o.value })}
                        >
                          <span class="choice-label">{o.label}</span>
                          <span class="choice-desc">{o.help}</span>
                        </button>
                      )}
                    </For>
                  </div>
                </div>
                <Show when={n().keepAlive === "always" && n().batteryOptimized}>
                  <Row title="Battery optimisation" help="Android may still stop Tessera after a while. Let it run without battery restrictions.">
                    <button type="button" class="btn" disabled={busy()} onClick={() => void run(batteryOptimizationRequest, "Could not open battery settings")}>
                      Allow in the background
                    </button>
                  </Row>
                </Show>
              </Card>
            </Show>
          </Show>
          <SendTest here={() => void run(notificationsTest, "Could not send a test notification")} />
        </div>
      )}
    </Show>
  );
}

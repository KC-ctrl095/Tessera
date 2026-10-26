// One server policy, shared by every receiving device.
import { For, Show, createResource } from "solid-js";
import type { NotificationPolicy } from "../../proto/NotificationPolicy";
import { call } from "../../lib/store";
import { toastError } from "../../lib/ui";
import { Toggle } from "../primitives";
import { Card, Row } from "./kit";

const clock = (minute: number) => `${String(Math.floor(minute / 60)).padStart(2, "0")}:${String(minute % 60).padStart(2, "0")}`;
const minute = (clock: string) => { const [h, m] = clock.split(":").map(Number); return h * 60 + m; };

export function NotificationPolicyCard() {
  const [policy, { mutate }] = createResource(() => call<NotificationPolicy>({ method: "get_notification_policy" }).catch(() => null));
  let saving = false;
  const save = async (patch: Partial<NotificationPolicy>) => {
    const cur = policy();
    if (!cur || saving) return;
    saving = true;
    try { mutate(await call<NotificationPolicy>({ method: "set_notification_policy", params: { policy: { ...cur, ...patch } } })); }
    catch (e) { toastError(e, "Could not save notification choices"); }
    finally { saving = false; }
  };
  const restore = async (key: string) => {
    try { mutate(await call<NotificationPolicy>({ method: "set_notification_source", params: { key, level: null } })); }
    catch (e) { toastError(e, "Could not restore this source"); }
  };
  return <Show when={policy()}>{(p) => <Card>
    <Row title="Page, Digest, Silent" help="Page calls for your attention. Digest gathers updates without sound. Silent stays in the inbox. Bots use Digest unless they mark security news, an outage or a result you asked for urgent." />
    <Row title="Digest" col help="One summary when there is something new. Times use the server's time zone.">
      <select class="input" aria-label="Digest rhythm" style={{ "min-height": "44px" }} value={p().hourly ? "hourly" : "daily"} onChange={(e) => void save({ hourly: e.currentTarget.value === "hourly" })}>
        <option value="daily">Daily</option><option value="hourly">Hourly</option>
      </select>
      <Show when={!p().hourly}>
        <label>At <input class="input" aria-label="Digest times" placeholder="18:00" value={(p().daily_at.length ? p().daily_at : [1080]).map(clock).join(", ")} onChange={(e) => {
          const values = e.currentTarget.value.split(",").map((s) => s.trim());
          if (values.length <= 4 && values.every((s) => /^(?:[01]\d|2[0-3]):[0-5]\d$/.test(s))) void save({ daily_at: values.map(minute) });
          else toastError("Use times such as 09:00, 18:00. Up to four a day.");
        }} /></label>
      </Show>
    </Row>
    <Row title="Quiet hours" help="Off unless you choose it. Only urgent pages break through. Other pages wait in the digest.">
      <Toggle label="Quiet hours" checked={p().quiet} onChange={(quiet) => void save({ quiet })} />
    </Row>
    <Show when={p().quiet}><Row title="From and until" col>
      <input class="input" type="time" aria-label="Quiet from" value={clock(p().quiet_from)} onChange={(e) => void save({ quiet_from: minute(e.currentTarget.value) })} />
      <input class="input" type="time" aria-label="Quiet until" value={clock(p().quiet_to)} onChange={(e) => void save({ quiet_to: minute(e.currentTarget.value) })} />
    </Row></Show>
    <Show when={p().sources.length}><Row title="Sources you turned down" help="Your choice applies on every device. Restore brings back the usual defaults." />
      <For each={p().sources}>{(s) => <Row title={s.name} help={s.level === "silent" ? "Muted" : "At most Digest"}>
        <button type="button" class="btn" onClick={() => void restore(s.key)}>Restore</button>
      </Row>}</For>
    </Show>
  </Card>}</Show>;
}

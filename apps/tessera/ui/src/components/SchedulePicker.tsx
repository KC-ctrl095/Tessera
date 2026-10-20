// "How often" for a scheduled task, in plain words first: every day at a time, weekdays, once a week,
// every hour, every few minutes. The cron expression the server reads sits behind "Custom schedule".
import { Match, Show, Switch, createSignal } from "solid-js";
import { describeSchedule, validSchedule } from "./Jobs";

type Mode = "day" | "weekdays" | "week" | "hour" | "every" | "custom";
type Unit = "m" | "h" | "d";
interface Plan {
  mode: Mode;
  time: string;
  dow: string;
  n: number;
  unit: Unit;
}

const MODES: { id: Mode; label: string }[] = [
  { id: "day", label: "Every day" },
  { id: "weekdays", label: "Weekdays (Monday to Friday)" },
  { id: "week", label: "Once a week" },
  { id: "hour", label: "Every hour" },
  { id: "every", label: "Every few minutes or hours" },
  { id: "custom", label: "Custom schedule…" },
];
const DAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const UNITS: { id: Unit; label: string }[] = [
  { id: "m", label: "minutes" },
  { id: "h", label: "hours" },
  { id: "d", label: "days" },
];

const pad = (n: number | string) => String(n).padStart(2, "0");

/** The plan a stored schedule stands for (anything unusual is "custom"). */
export function parsePlan(schedule: string): Plan {
  const v = schedule.trim();
  const plan: Plan = { mode: "custom", time: "09:00", dow: "1", n: 30, unit: "m" };
  const every = /^@every\s*(\d+)\s*(m|min|mins|minutes?|h|hr|hours?|d|days?)?$/i.exec(v);
  if (every && Number(every[1]) >= 1) return { ...plan, mode: "every", n: Number(every[1]), unit: ((every[2] ?? "m")[0].toLowerCase() as Unit) };
  if (v === "0 * * * *" || v === "@hourly") return { ...plan, mode: "hour" };
  const at = /^(\d{1,2}) (\d{1,2}) \* \* (\*|1-5|[0-7])$/.exec(v);
  if (at && Number(at[1]) < 60 && Number(at[2]) < 24) {
    const time = `${pad(at[2])}:${pad(at[1])}`;
    if (at[3] === "*") return { ...plan, mode: "day", time };
    if (at[3] === "1-5") return { ...plan, mode: "weekdays", time };
    return { ...plan, mode: "week", time, dow: String(Number(at[3]) % 7) };
  }
  return plan;
}

/** The expression for a plan ("custom" is typed by the person, never built). */
export function buildSchedule(p: Plan): string {
  const [h, m] = p.time.split(":").map((x) => Number(x) || 0);
  switch (p.mode) {
    case "day":
      return `${m} ${h} * * *`;
    case "weekdays":
      return `${m} ${h} * * 1-5`;
    case "week":
      return `${m} ${h} * * ${p.dow}`;
    case "hour":
      return "0 * * * *";
    default:
      return `@every ${Math.max(1, Math.round(p.n) || 1)}${p.unit}`;
  }
}

export function SchedulePicker(props: { value: string; onChange: (schedule: string) => void }) {
  const [plan, setPlan] = createSignal<Plan>(parsePlan(props.value));
  const change = (patch: Partial<Plan>) => {
    const next = { ...plan(), ...patch };
    setPlan(next);
    // Switching to Custom keeps the expression in place for editing.
    if (next.mode !== "custom") props.onChange(buildSchedule(next));
  };
  return (
    <div class="sched">
      <select class="input" aria-label="How often" value={plan().mode} onChange={(e) => change({ mode: e.currentTarget.value as Mode })}>
        {MODES.map((m) => (
          <option value={m.id}>{m.label}</option>
        ))}
      </select>
      <Switch>
        <Match when={plan().mode === "day" || plan().mode === "weekdays" || plan().mode === "week"}>
          <div class="sched-row">
            <Show when={plan().mode === "week"}>
              <select class="input" aria-label="Day of the week" value={plan().dow} onChange={(e) => change({ dow: e.currentTarget.value })}>
                {DAYS.map((d, i) => (
                  <option value={String(i)}>{`On ${d}s`}</option>
                ))}
              </select>
            </Show>
            <label class="sched-at">
              <span>at</span>
              <input class="input" type="time" aria-label="Time" value={plan().time} onInput={(e) => e.currentTarget.value && change({ time: e.currentTarget.value })} />
            </label>
          </div>
        </Match>
        <Match when={plan().mode === "every"}>
          <div class="sched-row">
            <label class="sched-at">
              <span>every</span>
              <input
                class="input sched-n"
                type="number"
                min="1"
                inputmode="numeric"
                aria-label="How many"
                value={plan().n}
                onInput={(e) => Number(e.currentTarget.value) >= 1 && change({ n: Number(e.currentTarget.value) })}
              />
            </label>
            <select class="input" aria-label="Unit" value={plan().unit} onChange={(e) => change({ unit: e.currentTarget.value as Unit })}>
              {UNITS.map((u) => (
                <option value={u.id}>{u.label}</option>
              ))}
            </select>
          </div>
        </Match>
        <Match when={plan().mode === "custom"}>
          <input
            class="input mono"
            classList={{ invalid: !validSchedule(props.value) }}
            aria-label="Schedule expression"
            value={props.value}
            onInput={(e) => props.onChange(e.currentTarget.value)}
            spellcheck={false}
          />
        </Match>
      </Switch>
      <span class="field-help">
        <b>{describeSchedule(props.value)}</b>
        <Show when={plan().mode === "custom"}>
          {" "}
          — cron <code>min hour day month weekday</code> (e.g. <code>0 8 * * 1-5</code>) or an interval like <code>@every 30m</code>.
        </Show>
      </span>
    </div>
  );
}

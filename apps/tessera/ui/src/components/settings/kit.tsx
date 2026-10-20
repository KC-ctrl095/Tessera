// The building blocks every Settings section uses: a card, a row with a title
// and a line of help, a row with a switch, a fold-out "Advanced" block, and the
// placeholders shown while a section loads or the server isn't there. One copy,
// so every section looks and behaves the same.
import { Show, createSignal, type JSX } from "solid-js";
import { errorMessage } from "../../lib/ui";
import { Icon, Spinner, type IconName } from "../icons";
import { Toggle } from "../primitives";

export function Card(props: { children: JSX.Element; class?: string }) {
  return <div class={"card" + (props.class ? " " + props.class : "")}>{props.children}</div>;
}

/** A setting: its name and a line of help on the left, its control on the right (`col`: below). */
export function Row(props: { title: string; help?: JSX.Element; children?: JSX.Element; col?: boolean }) {
  return (
    <div class="setting-row" classList={{ col: !!props.col }}>
      <div class="setting-text">
        <div class="setting-title">{props.title}</div>
        <Show when={props.help}>
          <div class="setting-help">{props.help}</div>
        </Show>
      </div>
      {props.children}
    </div>
  );
}

/** Keep the switch mounted while saving so keyboard focus stays on the control. */
export function SwitchRow(props: {
  title: string;
  help?: JSX.Element;
  checked: boolean;
  disabled?: boolean;
  saving?: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <Row title={props.title} help={props.help}>
      <Toggle checked={props.checked} disabled={props.disabled && !props.saving} busy={props.saving} onChange={props.onChange} label={props.title} />
      <Show when={props.saving}>
        <Spinner size={16} />
      </Show>
    </Row>
  );
}

/** Settings few people need, folded away until asked for. */
export function Advanced(props: { children: JSX.Element; label?: string; open?: boolean }) {
  const [open, setOpen] = createSignal(!!props.open);
  return (
    <div class="advanced-block">
      <button type="button" class="disclosure" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <Icon name="chevronRight" size={14} class="disc-chev" />
        {props.label ?? "Advanced"}
      </button>
      <Show when={open()}>
        <div class="advanced">{props.children}</div>
      </Show>
    </div>
  );
}

/** Says a setting applies to this device only (the rest are kept on the server, for every device). */
export function ThisDevice() {
  return (
    <span class="this-device" data-tip="Kept on this device only; your other devices have their own">
      This device
    </span>
  );
}

/** A line that leads to a page of its own (Passwords & keys, Activity…): its name, where it stands, a chevron. */
export function PageRow(props: { icon: IconName; title: string; help?: JSX.Element; onOpen: () => void; id?: string }) {
  return (
    <button type="button" class="page-row" id={props.id} onClick={props.onOpen}>
      <span class="page-row-icon">
        <Icon name={props.icon} size={16} />
      </span>
      <span class="page-row-text">
        <span class="setting-title">{props.title}</span>
        <Show when={props.help}>
          <span class="setting-help">{props.help}</span>
        </Show>
      </span>
      <Icon name="chevronRight" size={15} class="page-row-chev" />
    </button>
  );
}

/** A spinner while something loads; what went wrong if it couldn't. */
export function LoadingBlock(props: { error?: unknown }) {
  return (
    <div class="empty-state">
      <Show when={props.error} fallback={<Spinner size={20} />}>
        <span class="empty-icon">
          <Icon name="alertCircle" size={20} />
        </span>
        <p>{errorMessage(props.error)}</p>
      </Show>
    </div>
  );
}

export function NotConnected() {
  return (
    <div class="empty-state">
      <span class="empty-icon">
        <Icon name="wifiOff" size={20} />
      </span>
      <p>Connect to your server to change these settings.</p>
    </div>
  );
}

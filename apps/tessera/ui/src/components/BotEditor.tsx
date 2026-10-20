// The bot editor and the bot profile share one modal slot (`botEditor` in
// lib/ui.ts). Both load on first use (bot/BotForm.tsx, bot/BotProfile.tsx):
// they pull in the avatar picker, job editor and presets, which most visits
// to the app never need.
import { Show, Suspense, createSignal, lazy, onCleanup } from "solid-js";
import { openBotEditor, setBotEditor } from "../lib/ui";

const BotForm = lazy(() => import("./bot/BotForm").then((m) => ({ default: m.BotForm })));
const BotProfile = lazy(() => import("./bot/BotProfile").then((m) => ({ default: m.BotProfile })));

/** The bot whose profile (not editor) the modal shows. */
const [profileOf, setProfileOf] = createSignal<string | null>(null);
/** A template to start a new bot from (see BOT_TEMPLATES). */
const [templateOf, setTemplateOf] = createSignal<string | null>(null);

/** A bot's profile: what it knows, its duties, its chats. */
export function openBotProfile(id: string) {
  setBotEditor(null);
  setProfileOf(id);
  setBotEditor({ id });
}

/** A new bot, prefilled from a template (`study`, `assistant`, …). */
export function openBotTemplate(templateId: string) {
  setBotEditor(null);
  setTemplateOf(templateId);
  openBotEditor(null);
}

export function BotEditor(props: { id: string | null }) {
  const profile = () => !!props.id && profileOf() === props.id;
  // Taken once: a template only seeds the form it opened.
  const template = templateOf();
  setTemplateOf(null);
  onCleanup(() => setProfileOf(null));
  return (
    <Suspense>
      <Show when={profile()} fallback={<BotForm id={props.id} template={template} />}>
        <BotProfile id={props.id!} onEdit={() => setProfileOf(null)} />
      </Show>
    </Suspense>
  );
}


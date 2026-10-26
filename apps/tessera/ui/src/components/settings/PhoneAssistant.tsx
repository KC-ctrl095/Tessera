import { For } from "solid-js";
import { assistantChoice, setAssistantChoice } from "../../lib/phoneAssistant";
import { state } from "../../lib/store";

/** Per-phone routing. Speech engines and the normal chat settings stay shared. */
export function PhoneAssistant() {
  return <section class="settings-section" id="set-phone-assistant">
    <h4 class="section-title">Phone assistant</h4>
    <div class="setting-card">
      <p class="field-help">In Android Settings → Apps → Default apps → Digital assistant app, choose Tessera. On Samsung, also choose Advanced features → Side button → Press and hold → Digital assistant.</p>
      <p class="field-help">Hold the side button or swipe from a bottom corner to talk and hear the answer. Unlock your device first. Double press can open Tessera, but starts voice only if your phone offers an assistant shortcut.</p>
      <label class="field"><span class="field-label">Assistant chat</span>
        <select class="input" style={{ "min-height": "44px" }} aria-label="Assistant chat" value={assistantChoice()} onChange={(e) => setAssistantChoice(e.currentTarget.value)}>
          <option value="recent">Fresh chat, continue for 5 minutes</option>
          <option value="continue">Always continue one Assistant chat</option>
          <For each={state.bots}>{(bot) => <option value={`bot:${bot.id}`}>Talk to {bot.name}</option>}</For>
        </select>
      </label>
      <p class="field-help">New chats use your default model, tools and memory on this server. A chosen bot uses its own settings. This choice stays on this phone. Voice must be on below.</p>
    </div>
  </section>;
}

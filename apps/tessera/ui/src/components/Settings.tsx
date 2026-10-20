import { For, Match, Show, Suspense, Switch, createEffect, createMemo, createSignal, lazy, on, onCleanup, onMount, untrack } from "solid-js";
// Loaded with this chunk, not with the app (first, to keep their place in the cascade).
import "../styles/settings.css";
import "../styles/setup.css";
import { state } from "../lib/store";
import { setSettingsAtList, setSettingsTab, settingsAtList, settingsTab, type SettingsTab } from "../lib/ui";
import { isTauri, touchUi } from "../lib/util";
import { Icon, type IconName } from "./icons";
import { isDesktopApp, notificationsGet } from "../lib/native";
import { Modal, useBackToClose } from "./primitives";
import { LoadingBlock, NotConnected } from "./settings/kit";

interface SectionDef {
  id: SettingsTab;
  label: string;
  icon: IconName;
  blurb: string;
  /** One short line under the name in the phone's list. */
  hint: string;
  /** More words the search finds it by. */
  keys?: string;
  /** A page inside a section (Passwords & keys in Privacy & safety…): its section. */
  parent?: SettingsTab;
  /** Not in Settings (its home is elsewhere): only the search shows it, and a tap goes there. */
  elsewhere?: boolean;
}

/**
 * Nine sections, each setting in exactly one of them; a few have a page of
 * their own one step deeper; Help & tour sits at the foot of the list.
 */
const SECTIONS: SectionDef[] = [
  { id: "appearance", label: "Appearance", icon: "palette", blurb: "How Tessera looks on this device.", hint: "Theme, text size, colour", keys: "dark light theme colour color accent font text size spacing density tiles space gap grout dim focus follows mouse files open window" },
  { id: "notifications", label: "Notifications", icon: "bell", blurb: "What Tessera tells you about when it isn't in front.", hint: "What you hear about", keys: "alerts quiet hours battery push sound previews messages from agents reminders test" },
  { id: "models", label: "Models", icon: "sparkles", blurb: "The AI models Tessera can use. Your keys stay on your server.", hint: "The AI models you use", keys: "provider service api key openai chatgpt anthropic claude code subscription account accounts local llama ollama default model roles titles summaries memory small simpler instructions" },
  { id: "usage", label: "Usage", icon: "gauge", blurb: "Your plans' limits, how much your models were used, and roughly what it cost.", hint: "Limits, tokens and cost", keys: "usage cost spend price money budget tokens limit limits subscription plan accounts" },
  { id: "safety", label: "Privacy & safety", icon: "shieldCheck", blurb: "What agents may do, and how your passwords and devices are kept safe. Only you can change these.", hint: "Freedom, passwords, sandbox", keys: "security sandbox permission freedom yolo ask read only default passwords keys secrets bots help hand off encryption redact whiteboards pictures folders terminal" },
  { id: "memory", label: "Memory & skills", icon: "bookmark", blurb: "What Tessera remembers about you and your work, and the procedures it has saved.", hint: "What Tessera remembers", keys: "profile notes learned forget skills recipes how to" },
  { id: "web", label: "Web & voice", icon: "globe", blurb: "How agents search the web, the browser behind web views, and dictating messages.", hint: "Web search, browser, voice", keys: "search searxng duckduckgo brave tavily browser web view internet voice speech microphone dictation whisper transcribe" },
  { id: "devices", label: "Devices", icon: "phone", blurb: "Your phones, computers and browsers, and which computers your agents may use.", hint: "Phones, computers, browsers", keys: "pair pairing phone machines node this computer desktop control rename name sign out laptop activity log what agents did" },
  { id: "connection", label: "Connection", icon: "server", blurb: "Which server this app talks to, and how your devices reach it at home and away.", hint: "Your server, at home and away", keys: "server remote access away tunnel cloudflare lan home network address web browsers" },
];
const PAGES: SectionDef[] = [
  { id: "secrets", parent: "safety", label: "Passwords & keys", icon: "key", blurb: "Logins, passwords and API keys your agents can use without ever seeing them.", hint: "Logins your agents can use", keys: "secrets credentials tokens api keys login machines titles saving typed" },
  { id: "commands", parent: "safety", label: "What commands can reach", icon: "terminal", blurb: "The sandbox around agents' commands, the folders they may change, and the terminal on this server.", hint: "Sandbox, folders, terminal", keys: "sandbox folders read write terminal shell console command line" },
  { id: "activity", parent: "devices", label: "Activity", icon: "listChecks", blurb: "What your agents did on your computers, newest first.", hint: "What agents did on computers", keys: "log logs history audit commands ran recent machine node" },
];
const HELP: SectionDef = { id: "help", label: "Help & tour", icon: "info", blurb: "The tour, setup, keyboard shortcuts, tips and guides.", hint: "Tour, setup, shortcuts, guides", keys: "tour hints tips setup again keyboard shortcuts guides version about" };
const ALL: SectionDef[] = [...SECTIONS, ...PAGES, HELP];
/** Things that were in Settings once and live in the sidebar now: the search still finds them (App.tsx takes a tap there). */
const ELSEWHERE: SectionDef[] = [
  { id: "bots", label: "Bots", icon: "bot", blurb: "", hint: "In the sidebar", keys: "assistants templates persona", elsewhere: true },
  { id: "jobs", label: "Scheduled tasks", icon: "clock", blurb: "", hint: "In the sidebar", keys: "jobs schedule recurring every day duties", elsewhere: true },
  { id: "archived", label: "Archived chats", icon: "archive", blurb: "", hint: "In the sidebar", keys: "archive restore", elsewhere: true },
];

/** Older names, and where each lives now (with the card to show, when it is one card of a section). */
const ALIASES: Partial<Record<SettingsTab, { to: SettingsTab; card?: string }>> = {
  general: { to: "appearance" },
  providers: { to: "models" },
  voice: { to: "web", card: "set-voice" },
  search: { to: "web" },
  skills: { to: "memory", card: "set-skills" },
  security: { to: "safety" },
  terminal: { to: "commands", card: "set-terminal" },
  machines: { to: "devices" },
  thiscomputer: { to: "devices" },
  servers: { to: "connection" },
};

// Each section loads the first time it is opened.
const AppearanceTab = lazy(() => import("./settings/AppearanceTab").then((m) => ({ default: m.AppearanceTab })));
const NotificationsTab = lazy(() => import("./settings/NotificationsTab").then((m) => ({ default: m.NotificationsTab })));
const ModelsTab = lazy(() => import("./settings/ModelsTab").then((m) => ({ default: m.ModelsTab })));
const UsageTab = lazy(() => import("./UsageTab").then((m) => ({ default: m.UsageTab })));
const SafetyTab = lazy(() => import("./settings/SafetyTab").then((m) => ({ default: m.SafetyTab })));
const SecretsTab = lazy(() => import("./Secrets").then((m) => ({ default: m.SecretsTab })));
const CommandsTab = lazy(() => import("./settings/CommandsTab").then((m) => ({ default: m.CommandsTab })));
const MemoryTab = lazy(() => import("./settings/MemoryTab").then((m) => ({ default: m.MemoryTab })));
const WebTab = lazy(() => import("./settings/WebTab").then((m) => ({ default: m.WebTab })));
const DevicesTab = lazy(() => import("./settings/DevicesTab").then((m) => ({ default: m.DevicesTab })));
const ActivityTab = lazy(() => import("./settings/ActivityTab").then((m) => ({ default: m.ActivityTab })));
const ServersTab = lazy(() => import("./settings/ConnectionTab").then((m) => ({ default: m.ServersTab })));
const HelpTab = lazy(() => import("./settings/HelpTab").then((m) => ({ default: m.HelpTab })));

/** The native notification settings exist (the Tessera app); `undefined` while checking. */
const [notifSupport, setNotifSupport] = createSignal<boolean | undefined>(undefined);
const tabShown = (t: SectionDef) => t.id !== "notifications" || notifSupport() === true || !isTauri;
/** Where a name (an older one too) lives now. */
const homeOf = (t: SettingsTab): SettingsTab => ALIASES[t]?.to ?? t;
const defOf = (t: SettingsTab) => ALL.find((s) => s.id === homeOf(t)) ?? SECTIONS[0];

/** Phones (a normal phone, the folded Fold): the list of sections, then one section at a time. */
const PHONE = "(max-width: 639px)";
/** Where the phone's list was scrolled, kept while the app runs. */
let listScroll = 0;
/** The section looked at last (the phone's list marks it), kept while the app runs. */
const [lastSeen, setLastSeen] = createSignal<SettingsTab | null>(null);

function useMedia(query: string) {
  const mq = matchMedia(query);
  const [on, setOn] = createSignal(mq.matches);
  const change = () => setOn(mq.matches);
  mq.addEventListener("change", change);
  onCleanup(() => mq.removeEventListener("change", change));
  return on;
}

/** Every word of the search is somewhere in the section's name, line, description or keywords. */
function sectionMatches(t: SectionDef, q: string): boolean {
  const words = q.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const hay = `${t.label} ${t.hint} ${t.blurb} ${t.keys ?? ""}`.toLowerCase();
  return words.every((w) => hay.includes(w));
}

/**
 * Settings. Wide screens (the unfolded Fold, computers): the sections down the
 * side, the open one beside them. Phones: like a phone's own settings, a list
 * of sections (icon, name, one line), and a tap opens one full screen with a
 * back arrow; the back gesture goes back a level (a page to its section, a
 * section to the list), then out.
 */
export function SettingsModal() {
  const tab = () => homeOf(settingsTab()!);
  const def = () => defOf(tab());
  /** The section in the side list (a page's own section). */
  const section = () => def().parent ?? def().id;
  const parentDef = () => (def().parent ? defOf(def().parent!) : null);
  if (notifSupport() === undefined)
    notificationsGet()
      .then((p) => setNotifSupport(!!p))
      .catch(() => setNotifSupport(false));
  const phone = useMedia(PHONE);
  const [view, setView] = createSignal<"list" | "section">(untrack(settingsAtList) ? "list" : "section");
  /** Which way the last move went (the phone slides pages in from that side). */
  const [moved, setMoved] = createSignal<"in" | "out" | null>(null);
  /** The open section came from the list (back returns there rather than out of Settings). */
  let fromList = false;
  const [query, setQuery] = createSignal("");
  const shown = createMemo(() => [...ALL, ...ELSEWHERE].filter((t) => tabShown(t) && sectionMatches(t, query())));
  /** Where each section was scrolled while Settings is open. */
  const scrolls = new Map<SettingsTab, number>();
  let nav!: HTMLElement;
  let body!: HTMLDivElement;

  const restoreScroll = (el: HTMLElement, top: number) => {
    // A section that loads its content first is short for a moment: try again for a little while.
    let tries = 0;
    const put = () => {
      el.scrollTop = top;
      if (Math.abs(el.scrollTop - top) > 2 && tries++ < 30) requestAnimationFrame(put);
    };
    requestAnimationFrame(put);
  };
  /**
   * An older name that stood for one card of a section: bring that card into view once it is
   * drawn, and keep it there for a moment while the cards above it load (Web & voice draws its
   * voice card before web search has loaded), until the user scrolls or touches the page.
   */
  const showCard = (id: string) => {
    let frames = 0;
    let stopped = false;
    const quit = () => {
      stopped = true;
      for (const ev of ["wheel", "touchstart", "pointerdown", "keydown"]) body.removeEventListener(ev, quit);
    };
    for (const ev of ["wheel", "touchstart", "pointerdown", "keydown"]) body.addEventListener(ev, quit, { passive: true });
    const look = () => {
      if (stopped) return;
      const el = document.getElementById(id);
      // Only this section's scroll moves (scrollIntoView would also move the phone's sliding panes).
      if (el && body.contains(el) && body.clientHeight) {
        const off = el.getBoundingClientRect().top - body.getBoundingClientRect().top - 8;
        if (Math.abs(off) > 2) body.scrollTop += off;
      }
      if (frames++ < 90) requestAnimationFrame(look);
      else quit();
    };
    requestAnimationFrame(look);
  };
  const openSection = (id: SettingsTab) => {
    if (view() === "list") {
      listScroll = nav.scrollTop;
      fromList = true;
      setMoved("in");
    }
    setView("section");
    setSettingsTab(id);
  };
  const goList = () => {
    setMoved("out");
    setView("list");
    fromList = false;
    restoreScroll(nav, listScroll);
  };
  const close = () => setSettingsTab(null);
  /** Up a level: a page to its section; a phone's section to the list it came from. */
  const upLevel = (): boolean => {
    if (def().parent) {
      setSettingsTab(def().parent!);
      return true;
    }
    if (phone() && view() === "section" && fromList) {
      goList();
      return true;
    }
    return false;
  };
  /** Back (gesture, Escape, the arrow): up a level, else Settings closes. */
  const back = () => {
    if (!upLevel()) close();
  };

  // Opened again at no particular section: the list. The request stays up until this change is
  // through, so the section openSettings() also sets (Appearance) doesn't open over the list.
  createEffect(() => {
    if (!settingsAtList()) return;
    untrack(() => {
      if (view() !== "list") goList();
    });
    queueMicrotask(() => setSettingsAtList(false));
  });
  // A section opened from elsewhere while the list shows (a link in a dialog above): show it.
  createEffect(
    on(
      tab,
      (t) => {
        if (view() === "list" && !untrack(settingsAtList)) {
          fromList = true;
          setView("section");
        }
        // An older name for one card scrolls to that card instead (below).
        if (!ALIASES[untrack(settingsTab)!]?.card) restoreScroll(body, scrolls.get(t) ?? 0);
      },
      { defer: true },
    ),
  );
  // An older name for one card (Voice, Skills, Terminal): that card, in its section.
  createEffect(
    on(settingsTab, (t) => {
      const card = t ? ALIASES[t]?.card : undefined;
      if (card) showCard(card);
    }),
  );

  createEffect(() => {
    if (!phone() || view() === "section") setLastSeen(section());
  });

  // The back gesture: one history entry for Settings, set up again after each step up, so the
  // last back leaves Settings (see useBackToClose).
  let releaseBack = () => {};
  const armBack = () => {
    releaseBack = useBackToClose(() => {
      if (upLevel()) armBack();
      else close();
    });
  };
  onMount(armBack);
  onCleanup(() => releaseBack());

  /** ↑/↓ move through the sections in the side list. */
  const onNavKey = (e: KeyboardEvent) => {
    if (phone() || (e.key !== "ArrowDown" && e.key !== "ArrowUp")) return;
    if ((e.target as HTMLElement).closest("input")) return;
    const list = query().trim() ? shown().filter((t) => !t.elsewhere) : [...SECTIONS.filter(tabShown), HELP];
    const i = list.findIndex((t) => t.id === section() || t.id === tab());
    const next = list[(i + (e.key === "ArrowDown" ? 1 : -1) + list.length) % list.length];
    if (!next) return;
    e.preventDefault();
    openSection(next.id);
    requestAnimationFrame(() => nav.querySelector<HTMLElement>(".settings-tab.active")?.focus());
  };
  const onSearchKey = (e: KeyboardEvent) => {
    if (e.key === "Enter") {
      const first = shown()[0];
      if (first) {
        e.preventDefault();
        openSection(first.id);
      }
    } else if (e.key === "Escape" && query()) {
      // Clears the search; a second Escape closes Settings.
      e.preventDefault();
      e.stopPropagation();
      setQuery("");
    }
  };

  /** A keyboard starts in the search; a touch screen gets no keyboard popping up, just the page. */
  const firstFocus = () => (!touchUi() ? ".settings-search input" : phone() && view() === "list" ? ".settings-nav-head h2" : ".settings-head h3");

  const tabButton = (t: SectionDef) => (
    <button
      type="button"
      role={phone() || t.elsewhere ? undefined : "tab"}
      class="settings-tab"
      classList={{ elsewhere: !!t.elsewhere, active: phone() && view() === "list" ? lastSeen() === t.id : section() === t.id || tab() === t.id, page: !!t.parent }}
      aria-selected={phone() || t.elsewhere ? undefined : section() === t.id || tab() === t.id}
      data-tab={t.id}
      onClick={() => openSection(t.id)}
    >
      <span class="settings-tab-icon">
        <Icon name={t.icon} size={phone() ? 18 : 16} />
      </span>
      <span class="settings-tab-text">
        <span class="settings-tab-label">{t.label}</span>
        <Show when={phone() || t.parent || t.elsewhere}>
          <span class="settings-tab-hint">{t.parent ? `In ${defOf(t.parent).label}` : t.hint}</span>
        </Show>
      </span>
      <Show when={t.id === "web" && state.server && !state.server.voice_engine && !state.server.voice}>
        <span class="tab-dot" role="img" aria-label="Voice typing isn't set up" data-tip="Voice typing isn't set up" />
      </Show>
      <Show when={phone()}>
        <Icon name="chevronRight" size={16} class="settings-tab-chev" />
      </Show>
    </button>
  );

  return (
    <Modal onClose={back} title="Settings" size="lg" class="settings" hideHeader ownBack initialFocus={firstFocus()}>
      <div class="settings-layout" data-view={phone() ? view() : undefined} data-moved={phone() ? (moved() ?? undefined) : undefined}>
        <aside class="settings-nav" aria-label="Settings sections" ref={nav}>
          <div class="settings-nav-head">
            <h2 tabIndex={-1}>Settings</h2>
            <Show when={phone()}>
              <button type="button" class="icon-btn settings-x" aria-label="Close settings" onClick={close}>
                <Icon name="x" />
              </button>
            </Show>
          </div>
          <label class="settings-search">
            <Icon name="search" size={phone() ? 17 : 14} />
            <input
              type="search"
              placeholder="Search settings"
              aria-label="Search settings"
              autocomplete="off"
              spellcheck={false}
              value={query()}
              onInput={(e) => setQuery(e.currentTarget.value)}
              on:keydown={onSearchKey}
            />
          </label>
          <div class="settings-tabs" role={phone() ? undefined : "tablist"} aria-orientation={phone() ? undefined : "vertical"} onKeyDown={onNavKey}>
            <Show
              when={!query().trim()}
              fallback={
                <Show when={shown().length} fallback={<p class="settings-none">Nothing in Settings matches “{query().trim()}”.</p>}>
                  <div class="settings-group-rows">
                    <For each={shown()}>{tabButton}</For>
                  </div>
                </Show>
              }
            >
              <div class="settings-group-rows">
                <For each={SECTIONS.filter(tabShown)}>{tabButton}</For>
              </div>
              <div class="settings-group-rows settings-foot">{tabButton(HELP)}</div>
            </Show>
          </div>
        </aside>
        <section class="settings-main" role={phone() ? undefined : "tabpanel"} aria-label={def().label}>
          <header class="settings-head">
            <Show when={parentDef() || phone()}>
              <button
                type="button"
                class="icon-btn settings-back"
                aria-label={parentDef() ? `Back to ${parentDef()!.label}` : "All settings"}
                data-tip={parentDef() ? parentDef()!.label : undefined}
                onClick={() => (parentDef() ? setSettingsTab(parentDef()!.id) : goList())}
              >
                <Icon name="chevronLeft" size={20} />
              </button>
            </Show>
            <div class="grow">
              <Show when={parentDef()}>
                <div class="settings-crumb">{parentDef()!.label}</div>
              </Show>
              <h3 tabIndex={-1}>{def().label}</h3>
              <p class="settings-blurb">{def().blurb}</p>
            </div>
            <button type="button" class="icon-btn" aria-label="Close settings" data-tip="Close" data-kbd="Esc" onClick={close}>
              <Icon name="x" />
            </button>
          </header>
          <div class="settings-body" ref={body} onScroll={() => scrolls.set(tab(), body.scrollTop)}>
            <Show when={state.ready || tab() === "appearance" || tab() === "connection" || tab() === "help" || tab() === "notifications"} fallback={<NotConnected />}>
              <Suspense fallback={<LoadingBlock />}>
                <Switch>
                  <Match when={tab() === "appearance"}>
                    <AppearanceTab />
                  </Match>
                  <Match when={tab() === "notifications"}>
                    <NotificationsTab />
                  </Match>
                  <Match when={tab() === "models"}>
                    <ModelsTab />
                  </Match>
                  <Match when={tab() === "usage"}>
                    <UsageTab />
                  </Match>
                  <Match when={tab() === "safety"}>
                    <SafetyTab />
                  </Match>
                  <Match when={tab() === "secrets"}>
                    <SecretsTab />
                  </Match>
                  <Match when={tab() === "commands"}>
                    <CommandsTab />
                  </Match>
                  <Match when={tab() === "memory"}>
                    <MemoryTab />
                  </Match>
                  <Match when={tab() === "web"}>
                    <WebTab />
                  </Match>
                  <Match when={tab() === "devices"}>
                    <DevicesTab focus={settingsTab() === "thiscomputer" && isDesktopApp ? "this" : undefined} />
                  </Match>
                  <Match when={tab() === "activity"}>
                    <ActivityTab />
                  </Match>
                  <Match when={tab() === "connection"}>
                    <ServersTab />
                  </Match>
                  <Match when={tab() === "help"}>
                    <HelpTab />
                  </Match>
                </Switch>
              </Suspense>
            </Show>
          </div>
        </section>
      </div>
    </Modal>
  );
}

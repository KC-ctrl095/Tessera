// Bot presentation helpers shared by the sidebar, tiles, transcript and editor
// (the editor's own presets are in bot/presets.ts).
import { For, Show } from "solid-js";
import type { Bot } from "../proto/Bot";
import type { SessionInfo } from "../proto/SessionInfo";
import type { IconName } from "./icons";
import { botById, isBusy, state } from "../lib/store";
import { onDemand } from "../lib/onDemand";

/**
 * The portraits (their designs and the code that lays the tesserae) come on
 * demand: only people with bots ever see one. Not the avatars index: that
 * also brings the picker, which only the bot editor needs.
 */
export const portraits = onDemand(() => import("../avatars/portrait"));

const EMOJI_RE = /\p{Extended_Pictographic}/u;
export const isEmoji = (s: string) => EMOJI_RE.test(s);

const COLOR_RE = /^#[0-9a-f]{3,8}$/i;
/** A bot's colour, only if it is a plain hex colour (it ends up in CSS). */
export const botColor = (c: string | undefined | null, fallback = "#8a8a90") => (c && COLOR_RE.test(c) ? c : fallback);

/**
 * A bot's avatar: a mosaic portrait (`m:<id>`), or an emoji / short text on a
 * tile field in the bot's colour. Old single-letter avatars resolve to a
 * stable portrait.
 */
export function BotAvatar(props: { bot: Bot | undefined; size?: number; class?: string; title?: string; ring?: boolean }) {
  void portraits.load();
  const size = () => `${props.size ?? 24}px`;
  return (
    <Show
      when={portraits.get()}
      fallback={
        // Its place, in the bot's colour, for the moment the portraits take to arrive.
        <span
          class={"mav mav-wait " + (props.class ?? "")}
          style={{ display: "inline-block", width: size(), height: size(), "border-radius": "20%", background: botColor(props.bot?.color), opacity: 0.35, "flex-shrink": 0 }}
          aria-hidden="true"
          title={props.title}
        />
      }
    >
      {(m) => {
        const Portrait = m().MosaicAvatar;
        return (
          <Portrait
            avatar={m().resolveAvatar(props.bot?.avatar, props.bot?.id ?? "")}
            size={props.size ?? 24}
            color={props.bot?.color}
            ring={props.ring ?? false}
            class={(props.bot ? "" : "unknown ") + (props.class ?? "")}
            title={props.title}
            label={props.title ?? ""}
          />
        );
      }}
    </Show>
  );
}

/** Overlapping avatars (channel members). */
export function AvatarStack(props: { ids: string[]; size?: number; max?: number }) {
  const max = () => props.max ?? 3;
  const shown = () => props.ids.slice(0, max());
  return (
    <span class="av-stack" style={{ "--av": `${props.size ?? 20}px` }} aria-hidden="true">
      <For each={shown()}>{(id) => <BotAvatar bot={botById(id)} size={props.size ?? 20} />}</For>
      <Show when={props.ids.length > max()}>
        <span class="av-more">+{props.ids.length - max()}</span>
      </Show>
    </span>
  );
}

/** Whether a bot is doing something right now. */
export function botBusy(id: string): boolean {
  for (const s of Object.values(state.sessions)) {
    const i = s.info;
    // A bot's memory review is the harness's work, not the bot's.
    if (!isBusy(i) || i.internal) continue;
    if (i.kind !== "channel" && i.bot_id === id) return true;
    if (i.kind === "channel" && i.members.includes(id)) {
      const author = s.streaming?.author;
      if (!author || author === id) return true;
    }
  }
  return false;
}

/** "@nova ↔ @forge" for a private bot DM. */
export function dmLabel(info: SessionInfo): string {
  const m = info.members.map((id) => botById(id)).filter((b): b is Bot => !!b);
  if (m.length >= 2) return `@${m[0].handle} ↔ @${m[1].handle}`;
  if (m.length === 1) return `Your chats → @${m[0].handle}`;
  return info.title || "Private conversation";
}

export function sessionLabel(info: SessionInfo): string {
  if (info.kind === "channel" && info.bot_id) return dmLabel(info);
  if (info.kind === "channel") return info.title || "Untitled channel";
  return info.title || (info.bot_id ? `Chat with ${botById(info.bot_id)?.name ?? "bot"}` : "Untitled chat");
}

/** Fields newer servers may add (optional until then). */
export type BotX = Bot & {
  /** Conversation starters shown on a new chat with the bot. */
  starters?: string[];
};
/** The bot's own conversation starters, if it has any. */
export const botStarters = (b: Bot | undefined): string[] => ((b as BotX | undefined)?.starters ?? []).map((s) => s.trim()).filter(Boolean);
/** The notes the bot knows (ids). */
export const botKnowledge = (b: Bot | undefined): string[] => (Array.isArray(b?.knowledge) ? b!.knowledge : []);

/** A few starter prompts that fit the bot: its own starters first, else by role. */
export function botSuggestions(bot: Bot): string[] {
  const own = botStarters(bot);
  if (own.length) return own.slice(0, 4);
  const r = `${bot.role} ${bot.instructions}`.toLowerCase();
  if (/assistant|plan|day|email|inbox|remind|calendar|schedule|errand/.test(r))
    return ["Plan my day", "What's on my plate this week?", "Draft a polite reply to…", "Remind me every Friday to…"];
  if (/dev|code|build|test|ship|engineer|bug|ci\b|deploy/.test(r))
    return ["Run the tests and fix what fails", "Review my latest changes", "Why is the build slow?", "Explain how this repo is organized"];
  if (/research|source|summar|dig|analy|report|news/.test(r))
    return ["Research the latest on…", "Compare the top options for…", "Summarize this article: …", "Find primary sources on…"];
  if (/writ|edit|copy|blog|draft/.test(r)) return ["Tighten this paragraph: …", "Draft a post about…", "Give me three headline options for…"];
  if (/stud|school|learn|exam|homework|class|course|tutor/.test(r)) return ["Quiz me on what we covered this week", "Explain this step by step: …", "What should I review today?"];
  return [`What can you do, ${bot.name}?`, "Help me think through…", "Summarize…"];
}

// ── templates: a good bot in one click ───────────────────────────────────

export interface BotTemplate {
  id: string;
  label: string;
  icon: IconName;
  blurb: string;
  bot: Partial<BotX>;
  /** A recurring duty offered with it (added after the bot is created). */
  duty?: { name: string; schedule: string; prompt: string; when: string };
}

export const BOT_TEMPLATES: BotTemplate[] = [
  {
    id: "study",
    label: "Study buddy",
    icon: "book",
    blurb: "Remembers what you learn, explains, quizzes you and keeps track of exams.",
    bot: {
      name: "Sage",
      avatar: "m:owl",
      color: "#6fb08e",
      role: "Study buddy: remembers what I learn, explains it and quizzes me",
      personality: "Patient, encouraging and clear. Explains step by step with small examples, checks understanding with one quick question, and celebrates progress.",
      instructions: [
        "Help me learn for school.",
        "- Remember what I study: after each session, save the key points to memory (subject, topic, what I found hard). Keep course notes in notes, one note per subject, and add to them.",
        "- Before answering, check memory, my notes and our past chats (recall) for what we already covered, and build on it.",
        "- Explain in plain language, then check with one short question.",
        "- Quiz me with spaced repetition: bring back what I found hard after 1, 3 and 7 days.",
        "- Keep track of exams and deadlines I mention, and remind me ahead of time.",
        "- For homework, guide me to the answer; don't just hand it over.",
      ].join("\n"),
      tools: "assistant",
      permission: "default",
      own_memory: true,
      share_user_profile: true,
      starters: ["Quiz me on what we covered this week", "Explain this step by step: …", "Make flashcards from my notes on …", "What should I review today?"],
    },
    duty: {
      name: "Daily review",
      schedule: "0 18 * * 1-5",
      when: "weekdays at 18:00",
      prompt: "Pick what I should review today from my notes and memory (spaced repetition), and quiz me with three short questions.",
    },
  },
  {
    id: "assistant",
    label: "Personal assistant",
    icon: "calendar",
    blurb: "Plans your day, drafts replies and reminds you of things.",
    bot: {
      name: "Nova",
      avatar: "m:winged-cap",
      color: "#9d8bd6",
      role: "Personal assistant: plans my day, drafts replies, reminds me",
      personality: "Warm, friendly and to the point. Gives the short answer first and details on request.",
      instructions: "Keep my week organised: plan my days around fixed appointments, draft polite replies when I ask, keep a list of open to-dos in notes, and remind me of deadlines I mention.",
      tools: "assistant",
      permission: "default",
      own_memory: true,
      share_user_profile: true,
      starters: ["Plan my day", "What's on my plate this week?", "Draft a polite reply to …", "Remind me every Friday to …"],
    },
    duty: { name: "Morning briefing", schedule: "0 8 * * 1-5", when: "weekdays at 8:00", prompt: "Brief me on today: plans, open to-dos and anything due soon." },
  },
  {
    id: "research",
    label: "Researcher",
    icon: "globe",
    blurb: "Digs up sources, compares options and writes short reports.",
    bot: {
      name: "Scout",
      avatar: "m:eagle",
      color: "#7d9be0",
      role: "Researcher: finds sources, compares options, writes short reports",
      personality: "Curious and careful. Cites sources, separates facts from opinions, and says how confident it is.",
      instructions: "Research what I ask with web search and the browser. Prefer primary sources, cite every claim, and keep findings in a note per topic so we can build on them.",
      tools: "research",
      permission: "default",
      own_memory: true,
      share_user_profile: false,
      starters: ["Research the latest on …", "Compare the top options for …", "Summarize this article: …", "Find primary sources on …"],
    },
  },
  {
    id: "coder",
    label: "Coding partner",
    icon: "terminal",
    blurb: "Builds, tests and reviews code in a project folder.",
    bot: {
      name: "Forge",
      avatar: "m:hammer-cap",
      color: "#e07a56",
      role: "Coding partner: builds, tests and reviews code",
      personality: "Terse and technical. Leads with the answer or the diff, states trade-offs plainly.",
      instructions: "Work in my project folder. Run the tests before and after changes, keep changes small and explain what changed and why.",
      tools: "coding",
      permission: "default",
      own_memory: true,
      share_user_profile: false,
      starters: ["Run the tests and fix what fails", "Review my latest changes", "Explain how this repo is organized", "Why is the build slow?"],
    },
  },
  {
    id: "writer",
    label: "Writer",
    icon: "pencil",
    blurb: "Drafts, edits and tightens your writing in your voice.",
    bot: {
      name: "Quill",
      avatar: "m:scroll-muse",
      color: "#c98aa6",
      role: "Writer: drafts, edits and tightens my writing",
      personality: "Thoughtful and direct. Suggests concrete edits, keeps my voice, and explains changes briefly.",
      instructions: "Help me write. Keep drafts in notes, suggest edits rather than rewriting everything, and match my tone.",
      tools: "assistant",
      permission: "default",
      own_memory: true,
      share_user_profile: true,
      starters: ["Tighten this paragraph: …", "Draft a post about …", "Give me three headline options for …"],
    },
  },
];

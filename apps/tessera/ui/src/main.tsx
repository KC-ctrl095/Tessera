/* @refresh reload */
import { lazy } from "solid-js";
import { render } from "solid-js/web";
// Fonts ship with the app (never fetched): see styles/fonts.css.
import "./styles/fonts.css";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/components.css";
import "./styles/layout.css";
import "./styles/chat.css";
import "./styles/files.css";
import "./styles/markdown.css";
// Settings, setup, the bot editor, web views, note tiles, a whiteboard's
// question, the cheatsheet and the coach marks bring their own styles with
// their chunks (settings.css, setup.css, bot-editor.css, webview.css,
// notes.css, ask.css, cheatsheet.css, tour.css).
import "./styles/bots.css";
import "./styles/boards.css";
import "./lib/prefs";
import "./lib/mosaicSync";
import { App } from "./App";
import { fileWindowToken } from "./lib/fileWindow";
import { isTauriMobile } from "./lib/util";

// The phone app is laid out for its screen: a pinch never zooms the whole app (it stayed zoomed
// with nothing to undo it, and sideways strokes then panned the zoomed page instead of turning
// tiles). A pinch in still opens the tiles overview; pictures zoom in their own viewer. Browsers
// keep page zoom.
if (isTauriMobile) {
  const meta = document.querySelector('meta[name="viewport"]');
  meta?.setAttribute("content", `${meta.getAttribute("content")}, maximum-scale=1, user-scalable=no`);
}

const root = document.getElementById("root")!;
// A window showing a file from a chat (lib/fileWindow.ts) is only that: the app is never started
// there (no connection, no tiles), so it can't change what the app keeps.
const fileWindow = fileWindowToken();
const FileWindow = lazy(() => import("./components/files/FileWindow").then(m => ({ default: m.FileWindow })));
if (fileWindow) render(() => <FileWindow token={fileWindow} />, root);
else render(() => <App />, root);

// Tessera's stroke icon set: 24-unit grid, drawn with non-scaling 1.5px
// strokes (see `.icon` in base.css) so every size renders the same weight.
import type { JSX } from "solid-js";
import { onCleanup, onMount, splitProps } from "solid-js";

const P = (d: string) => `<path d="${d}"/>`;
const C = (cx: number, cy: number, r: number, fill = false) =>
  `<circle cx="${cx}" cy="${cy}" r="${r}"${fill ? ' fill="currentColor" stroke="none"' : ""}/>`;
const R = (x: number, y: number, w: number, h: number, rx: number, fill = false) =>
  `<rect x="${x}" y="${y}" width="${w}" height="${h}" rx="${rx}"${fill ? ' fill="currentColor" stroke="none"' : ""}/>`;

const ICONS = {
  plus: P("M12 5.5v13M5.5 12h13"),
  minus: P("M6 12h12"),
  x: P("M7 7l10 10M17 7L7 17"),
  check: P("M5.5 12.5l4 4L18.5 7.5"),
  chevronDown: P("M7 10l5 5 5-5"),
  chevronUp: P("M7 14l5-5 5 5"),
  chevronRight: P("M10 7l5 5-5 5"),
  chevronLeft: P("M14 7l-5 5 5 5"),
  chevronsUpDown: P("M8 9.5l4-4 4 4M8 14.5l4 4 4-4"),
  menu: P("M4.5 7.5h15M4.5 12h15M4.5 16.5h9"),
  search: C(11, 11, 6) + P("M15.5 15.5l4 4"),
  sliders: P("M4.5 7.5h8M16.5 7.5h3M4.5 16.5h3M11.5 16.5h8") + C(14.5, 7.5, 2) + C(9.5, 16.5, 2),
  /** Settings (everywhere): a cog. */
  gear: C(12, 12, 3) + P("M12 3.5v2.2M12 18.3v2.2M3.5 12h2.2M18.3 12h2.2M6 6l1.6 1.6M16.4 16.4L18 18M6 18l1.6-1.6M16.4 7.6L18 6") + C(12, 12, 6.2),
  tune: P("M7 4.5v6M7 14.5v5M17 4.5v2M17 10.5v9") + C(7, 12.5, 2) + C(17, 8.5, 2),
  arrowUp: P("M12 18.5v-13M6.5 11L12 5.5 17.5 11"),
  arrowDown: P("M12 5.5v13M6.5 13l5.5 5.5 5.5-5.5"),
  arrowLeft: P("M18.5 12h-13M11 6.5L5.5 12l5.5 5.5"),
  arrowRight: P("M5.5 12h13M13 6.5l5.5 5.5-5.5 5.5"),
  cornerDownLeft: P("M18.5 5.5v6a3 3 0 0 1-3 3h-10M9 11l-3.5 3.5L9 18"),
  stop: R(7, 7, 10, 10, 2.5, true),
  paperclip: P("M19.5 11.5l-7.8 7.8a4.8 4.8 0 0 1-6.8-6.8l8.3-8.3a3.2 3.2 0 0 1 4.5 4.5l-8.1 8.1a1.6 1.6 0 0 1-2.3-2.3l7.4-7.4"),
  image: R(3.5, 4.5, 17, 15, 3) + C(9, 10, 1.6) + P("M20.5 15.5L16 11l-9.5 8.5"),
  camera: P("M4.5 8.5a2 2 0 0 1 2-2h1.6l1.1-1.7a1 1 0 0 1 .85-.5h3.9a1 1 0 0 1 .85.5l1.1 1.7h1.6a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2h-11a2 2 0 0 1-2-2z") + C(12, 12.6, 3.2),
  copy: R(8.5, 8.5, 11, 11, 2.5) + P("M15.5 8.5v-2a2 2 0 0 0-2-2h-7a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h2"),
  terminal: R(3, 4.5, 18, 15, 3) + P("M7.5 9.5l3 2.5-3 2.5M13 15h3.5"),
  file: P("M13.5 3.5H7.5a2.5 2.5 0 0 0-2.5 2.5v12a2.5 2.5 0 0 0 2.5 2.5h9a2.5 2.5 0 0 0 2.5-2.5V9zM13.5 3.5V9H19"),
  pencil: P("M4.5 19.5h3.8L19 8.8a2.2 2.2 0 0 0-3.1-3.1L5.2 16.4l-.7 3.1zM14.5 7.2l2.9 2.9"),
  compose: P("M11 4.5H7a2.5 2.5 0 0 0-2.5 2.5v10A2.5 2.5 0 0 0 7 19.5h10a2.5 2.5 0 0 0 2.5-2.5v-4") + P("M17.8 3.8a1.9 1.9 0 0 1 2.7 2.7L13 14l-3.5.9.9-3.5z"),
  folder: P("M3.5 7.5a2.5 2.5 0 0 1 2.5-2.5h3.3l2.2 2.2h6.5a2.5 2.5 0 0 1 2.5 2.5v7.3a2.5 2.5 0 0 1-2.5 2.5H6a2.5 2.5 0 0 1-2.5-2.5z"),
  globe: C(12, 12, 8.5) + P("M3.5 12h17M12 3.5c2.4 2.5 3.5 5.4 3.5 8.5s-1.1 6-3.5 8.5c-2.4-2.5-3.5-5.4-3.5-8.5s1.1-6 3.5-8.5z"),
  wrench: P("M15 4.4a4.6 4.6 0 0 0-4.4 6L4.6 16.4a2.1 2.1 0 0 0 3 3l6-6a4.6 4.6 0 0 0 6-4.4l-2.8 2.8-2.6-.6-.6-2.6z"),
  bulb: P("M9.5 17.5h5M10.5 20.5h3M12 3.5a6 6 0 0 0-3.6 10.8c.6.5 1.1 1.3 1.1 2.2h5c0-.9.5-1.7 1.1-2.2A6 6 0 0 0 12 3.5z"),
  brain: P("M12 5.5a3 3 0 0 0-5.6-1.2A3 3 0 0 0 4 8.5a3 3 0 0 0 .5 4.8A3.2 3.2 0 0 0 8 18.4a3 3 0 0 0 4 1.1zM12 5.5a3 3 0 0 1 5.6-1.2A3 3 0 0 1 20 8.5a3 3 0 0 1-.5 4.8 3.2 3.2 0 0 1-3.5 5.1 3 3 0 0 1-4 1.1zM12 5.5v14"),
  sparkles: P("M11 3.5l1.7 4.6 4.6 1.7-4.6 1.7L11 16.1l-1.7-4.6-4.6-1.7 4.6-1.7zM18 14.5l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8z"),
  alert: P("M10.3 4.9L3.4 17a2 2 0 0 0 1.7 3h13.8a2 2 0 0 0 1.7-3L13.7 4.9a2 2 0 0 0-3.4 0zM12 9.5v4M12 16.8v.2"),
  info: C(12, 12, 8.5) + P("M12 11v5M12 7.9v.2"),
  alertCircle: C(12, 12, 8.5) + P("M12 7.5v5M12 15.9v.2"),
  pin: P("M9 3.5h6l-.9 5.1 3.4 3.1v1.8h-11v-1.8l3.4-3.1zM12 13.5v7"),
  archive: R(3.5, 4.5, 17, 4, 1.5) + P("M5 8.5v9a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-9M10 12.5h4"),
  fork: C(6.5, 5.5, 2) + C(6.5, 18.5, 2) + C(17.5, 7.5, 2) + P("M6.5 7.5v9M17.5 9.5v.5a4 4 0 0 1-4 4h-3.5a3.5 3.5 0 0 0-3.5 2.5"),
  trash: P("M4.5 7h15M9.5 7V5.2c0-.9.7-1.7 1.7-1.7h1.6c1 0 1.7.8 1.7 1.7V7M6.5 7l.8 11.2a2 2 0 0 0 2 1.8h5.4a2 2 0 0 0 2-1.8L17.5 7"),
  more: C(6, 12, 1.25, true) + C(12, 12, 1.25, true) + C(18, 12, 1.25, true),
  maximize: P("M4.5 9V6a1.5 1.5 0 0 1 1.5-1.5h3M15 4.5h3A1.5 1.5 0 0 1 19.5 6v3M19.5 15v3a1.5 1.5 0 0 1-1.5 1.5h-3M9 19.5H6A1.5 1.5 0 0 1 4.5 18v-3"),
  minimize: P("M9 4.5V7.5A1.5 1.5 0 0 1 7.5 9h-3M19.5 9h-3A1.5 1.5 0 0 1 15 7.5v-3M15 19.5v-3a1.5 1.5 0 0 1 1.5-1.5h3M4.5 15h3A1.5 1.5 0 0 1 9 16.5v3"),
  columns: R(3.5, 4.5, 17, 15, 3) + P("M12 4.5v15"),
  rows: R(3.5, 4.5, 17, 15, 3) + P("M3.5 12h17"),
  sidebar: R(3.5, 4.5, 17, 15, 3) + P("M9.5 4.5v15"),
  clock: C(12, 12, 8.5) + P("M12 7.5V12l3 2"),
  server: R(3.5, 4, 17, 7, 2.5) + R(3.5, 13, 17, 7, 2.5) + P("M7 7.5h.01M7 16.5h.01"),
  key: C(8, 15.5, 4) + P("M11 12.5l8.5-8.5M16.5 7l2.5 2.5M14.5 9l2 2"),
  link: P("M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"),
  refresh: P("M19.5 11A7.5 7.5 0 0 0 6 6.8L4.5 8.5M4.5 4v4.5H9M4.5 13A7.5 7.5 0 0 0 18 17.2l1.5-1.7M19.5 20v-4.5H15"),
  sun: C(12, 12, 3.8) + P("M12 2.8v1.9M12 19.3v1.9M4.9 4.9l1.3 1.3M17.8 17.8l1.3 1.3M2.8 12h1.9M19.3 12h1.9M4.9 19.1l1.3-1.3M17.8 6.2l1.3-1.3"),
  moon: P("M19.5 14.5A7.5 7.5 0 1 1 9.5 4.5a6 6 0 0 0 10 10z"),
  monitor: R(3, 4, 18, 12.5, 2.5) + P("M8.5 20h7M12 16.5V20"),
  laptop: R(4.5, 5, 15, 10.5, 2) + P("M2.5 19h19"),
  home: P("M4.5 10.5L12 4.5l7.5 6M6.5 9v9.5a1 1 0 0 0 1 1H10v-5h4v5h2.5a1 1 0 0 0 1-1V9"),
  cloud: P("M7.5 18.5a4.5 4.5 0 0 1-.7-8.95 6 6 0 0 1 11.4 1.6 3.7 3.7 0 0 1-.7 7.35z"),
  user: C(12, 8.5, 3.5) + P("M5 20a7 7 0 0 1 14 0"),
  cpu: R(6.5, 6.5, 11, 11, 2.5) + P("M9.5 3.5v3M14.5 3.5v3M9.5 17.5v3M14.5 17.5v3M3.5 9.5h3M3.5 14.5h3M17.5 9.5h3M17.5 14.5h3") + R(9.5, 9.5, 5, 5, 1),
  book: P("M5 18.5V5.5a2 2 0 0 1 2-2h12v14H7a2 2 0 0 0-2 2 2 2 0 0 0 2 2h12M9 7.5h6"),
  phone: R(6.5, 3, 11, 18, 2.5) + P("M11 17.5h2"),
  palette: P("M12 3.5a8.5 8.5 0 1 0 0 17c1 0 1.7-.8 1.7-1.7 0-.5-.2-.9-.5-1.2-.3-.3-.5-.7-.5-1.2 0-.9.8-1.7 1.7-1.7h2a4.1 4.1 0 0 0 4.1-4.1c0-3.9-3.8-7.1-8.5-7.1z") + C(7.5, 11, 1.1, true) + C(10, 7.5, 1.1, true) + C(14.5, 7.5, 1.1, true),
  external: P("M14 4.5h5.5V10M19.5 4.5L11 13M17.5 13.5v4a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2v-9a2 2 0 0 1 2-2h4"),
  listChecks: P("M4 7l1.6 1.6L8.8 5.4M4 14l1.6 1.6 3.2-3.2M12 7h8M12 14h8M5 20h.01M12 20h8"),
  shield: P("M12 3.5l7 2.6v5.5c0 4.2-2.9 7.6-7 8.9-4.1-1.3-7-4.7-7-8.9V6.1z"),
  shieldCheck: P("M12 3.5l7 2.6v5.5c0 4.2-2.9 7.6-7 8.9-4.1-1.3-7-4.7-7-8.9V6.1zM9 12l2.2 2.2L15.2 10"),
  shieldAlert: P("M12 3.5l7 2.6v5.5c0 4.2-2.9 7.6-7 8.9-4.1-1.3-7-4.7-7-8.9V6.1zM12 8.5v4M12 15.7v.1"),
  /** Kept for older call sites; use `fast` (fast mode) or `shieldOff` (YOLO). */
  zap: P("M13 3.5L5 13.5h6.2l-1 7L18.5 10.5h-6.2z"),
  hand: P("M8.5 12.5V6.8a1.4 1.4 0 0 1 2.8 0V11M11.3 10.5V5.4a1.4 1.4 0 0 1 2.8 0v5.1M14.1 10.5V6.8a1.4 1.4 0 0 1 2.8 0v6.7a6.5 6.5 0 0 1-6.5 6.5h-.3a5.2 5.2 0 0 1-4.2-2.1l-2-2.8a1.5 1.5 0 0 1 2.3-1.9l2.3 2.1"),
  eye: P("M2.5 12s3.5-6.5 9.5-6.5 9.5 6.5 9.5 6.5-3.5 6.5-9.5 6.5S2.5 12 2.5 12z") + C(12, 12, 2.8),
  eyeOff: P("M9.9 5.7A9.6 9.6 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a17 17 0 0 1-2.6 3.4M6.6 6.6C3.9 8.3 2.5 12 2.5 12s3.5 6.5 9.5 6.5c1.8 0 3.3-.6 4.6-1.4M9.9 9.9a2.9 2.9 0 0 0 4.2 4.2M4 4l16 16"),
  lock: R(5, 10.5, 14, 10, 2.5) + P("M8 10.5V8a4 4 0 0 1 8 0v2.5M12 14.5v2"),
  lockOpen: R(5, 10.5, 14, 10, 2.5) + P("M8 10.5V8a4 4 0 0 1 7.6-1.7M12 14.5v2"),
  help: C(12, 12, 8.5) + P("M9.6 9.6a2.5 2.5 0 0 1 4.9.6c0 1.7-2.5 2.1-2.5 3.6M12 16.9v.2"),
  grid: R(4, 4, 7, 7, 2) + R(13, 4, 7, 7, 2) + R(4, 13, 7, 7, 2) + R(13, 13, 7, 7, 2),
  play: P("M8 5.5v13l10.5-6.5z"),
  pause: P("M8.5 5.5v13M15.5 5.5v13"),
  calendar: R(3.5, 5, 17, 15, 2.5) + P("M3.5 10h17M8 3v4M16 3v4"),
  bookmark: P("M6.5 4.5c0-.6.4-1 1-1h9c.6 0 1 .4 1 1v16l-5.5-4-5.5 4z"),
  chat: P("M20 11.5a8 8 0 0 1-11.7 7.1L4 20l1.4-4A8 8 0 1 1 20 11.5z"),
  wifiOff: P("M3 3l18 18M8.5 15.5a5 5 0 0 1 6.2-.6M5 12a10 10 0 0 1 3.4-2.1M15.6 10.3A10 10 0 0 1 19 12M2 8.5A15 15 0 0 1 6 6M10.8 5.1A15 15 0 0 1 22 8.5M12 19.5h.01"),
  logOut: P("M14.5 4.5H18a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2h-3.5M9.5 16.5L5 12l4.5-4.5M5 12h10.5"),
  circle: C(12, 12, 8),
  circleCheck: C(12, 12, 8) + P("M8.6 12.2l2.3 2.3 4.5-4.7"),
  circleHalf: C(12, 12, 8) + `<path d="M12 4a8 8 0 0 1 0 16z" fill="currentColor" stroke="none"/>`,
  circleX: C(12, 12, 8) + P("M9.6 9.6l4.8 4.8M14.4 9.6l-4.8 4.8"),
  circleMinus: C(12, 12, 8) + P("M8.6 12h6.8"),
  dot: C(12, 12, 3, true),
  hash: P("M5 9h14.5M4.5 15H19M10.5 4l-2 16M15.5 4l-2 16"),
  board: R(3.5, 4.5, 17, 12, 2.5) + P("M8 20.5l4-4 4 4M7.5 12.5c1.5-3 3-3 4.5-1s3 2 4.5-1"),
  cube: P("M12 3.5l7.5 4.2v8.6L12 20.5l-7.5-4.2V7.7zM4.5 7.7L12 12l7.5-4.3M12 12v8.5"),
  pen: P("M3.5 17.5c2.2-2.9 4.1-4.3 5.6-4.3 2.2 0 .6 4.3 2.7 4.3 1.4 0 2.4-2.6 3.8-2.6 1 0 1.2 1.3 2.2 1.3.7 0 1.4-.4 2.7-1.2M13.5 5.5l3 3M15.3 3.7a2.1 2.1 0 0 1 3 3L11 14l-3.4.8.8-3.4z"),
  zoomIn: C(11, 11, 6.5) + P("M16 16l4 4M8.5 11h5M11 8.5v5"),
  zoomOut: C(11, 11, 6.5) + P("M16 16l4 4M8.5 11h5"),
  appWindow: R(3, 4.5, 18, 15, 3) + P("M3 9h18M6.5 6.8h.01M9 6.8h.01"),
  inbox2: P("M4 13.5V17a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3.5M12 4v10M8 10l4 4 4-4"),
  compress: P("M4 14h6v6M20 10h-6V4M14 10l6.5-6.5M3.5 20.5L10 14"),
  grip: C(9, 6, 1.2, true) + C(15, 6, 1.2, true) + C(9, 12, 1.2, true) + C(15, 12, 1.2, true) + C(9, 18, 1.2, true) + C(15, 18, 1.2, true),
  gauge: P("M4.5 17a8.5 8.5 0 1 1 15 0M12 13l3.5-3.5") + C(12, 13, 1.2, true),
  download: P("M12 4v11M7 10.5l5 5 5-5M5 19.5h14"),
  inbox: P("M3.5 13.5h5l1.5 2.5h4l1.5-2.5h5M5.5 5.5h13l2 8v4.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2v-4.5z"),
  mic: R(9, 3.5, 6, 11, 3) + P("M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v2.5"),
  micOff: P("M15 9.4V6.5a3 3 0 0 0-5.8-1M9 9v2.5a3 3 0 0 0 4.9 2.3M5.5 11.5a6.5 6.5 0 0 0 10.6 5M18.5 11.5a6.4 6.4 0 0 1-.6 2.7M12 18v2.5M4 4l16 16"),
  waveform: P("M4 10.5v3M7.5 8v8M11 5v14M14.5 8.5v7M18 10v4M21 11.2v1.6"),
  variable: P("M8 4.5c-2 1.8-3 4.3-3 7.5s1 5.7 3 7.5M16 4.5c2 1.8 3 4.3 3 7.5s-1 5.7-3 7.5M9.5 9l5 6M14.5 9l-5 6"),
  scan: P("M4.5 8.5V6.5a2 2 0 0 1 2-2h2M15.5 4.5h2a2 2 0 0 1 2 2v2M19.5 15.5v2a2 2 0 0 1-2 2h-2M8.5 19.5h-2a2 2 0 0 1-2-2v-2M7.5 12h9"),
  qrCode: R(4, 4, 6, 6, 1.2) + R(14, 4, 6, 6, 1.2) + R(4, 14, 6, 6, 1.2) + P("M14 14h2.5v2.5M20 14v.01M14 20h.01M17 20h3v-3M17 17h.01"),
  command: P("M9 9V6.5A2.5 2.5 0 1 0 6.5 9H9zM9 9h6M9 9v6M15 9V6.5A2.5 2.5 0 1 1 17.5 9H15zM15 9v6M9 15v2.5A2.5 2.5 0 1 1 6.5 15H9zM9 15h6M15 15h2.5a2.5 2.5 0 1 1-2.5 2.5z"),
  // Tiling
  layoutDwindle: R(3.5, 4.5, 17, 15, 3) + P("M11 4.5v15M11 12h9.5M15.75 12v7.5"),
  layoutMaster: R(3.5, 4.5, 17, 15, 3) + P("M12.5 4.5v15M12.5 9.5h8M12.5 14.5h8"),
  layoutColumns: R(3.5, 4.5, 17, 15, 3) + P("M9.2 4.5v15M14.8 4.5v15"),
  layoutRows: R(3.5, 4.5, 17, 15, 3) + P("M3.5 9.5h17M3.5 14.5h17"),
  layoutGrid: R(3.5, 4.5, 17, 15, 3) + P("M12 4.5v15M3.5 12h17"),
  layoutMonocle: R(3.5, 4.5, 17, 15, 3) + P("M3.5 9h17M9 4.5V9M14.5 4.5V9"),
  splitRight: R(3.5, 4.5, 17, 15, 3) + P("M11 4.5v15M15.75 10v4M13.75 12h4"),
  splitDown: R(3.5, 4.5, 17, 15, 3) + P("M3.5 11h17M12 13.2v4M10 15.2h4"),
  keyboard: R(2.5, 6, 19, 12, 3) + P("M6.5 10h.01M10 10h.01M13.5 10h.01M17 10h.01M7.5 14h9"),
  bot: R(4.5, 8, 15, 11.5, 3.5) + P("M12 4.5V8M9.5 12.8v1.2M14.5 12.8v1.2M2.5 13v2.5M21.5 13v2.5") + C(12, 3.8, 1.2, true),
  users: C(9, 8.5, 3.2) + P("M3.5 19.5a5.5 5.5 0 0 1 11 0M15.5 5.6a3.2 3.2 0 0 1 0 5.8M17.5 14.3a5.5 5.5 0 0 1 3 5.2"),
  at: C(12, 12, 3.5) + P("M15.5 8.5v5a2.5 2.5 0 0 0 5 0V12a8.5 8.5 0 1 0-3.4 6.8"),
  swap: P("M7 7.5h12.5M16 4l3.5 3.5L16 11M17 16.5H4.5M8 13l-3.5 3.5L8 20"),
  workspaces: R(3.5, 3.5, 7, 7, 2) + R(13.5, 3.5, 7, 7, 2) + R(3.5, 13.5, 7, 7, 2) + R(13.5, 13.5, 7, 7, 2, true),
  /** Four tiles, the last one a "+": a new mosaic. */
  newMosaic: R(3.5, 3.5, 7, 7, 2) + R(13.5, 3.5, 7, 7, 2) + R(3.5, 13.5, 7, 7, 2) + P("M17 13.5v7M13.5 17h7"),
  smile: C(12, 12, 8.5) + P("M8.5 14.5a4.5 4.5 0 0 0 7 0M9 9.8v.2M15 9.8v.2"),
  send: P("M4.5 12L20 4.5 16 20l-4-6.5zM12 13.5L20 4.5"),
  bell: P("M6.5 16.5V11a5.5 5.5 0 0 1 11 0v5.5l1.5 2h-14zM10 20.5h4"),
  mosaic: P("M4 4.2l7.6-.4.3 6.4-7.7.3zM13.6 3.8l6.4.3-.2 9.6-6.1-.2zM4.1 12.5l5.6.2-.2 7.6-5.5-.2zM11.6 15.6l8.2.1.2 4.4-8.5-.1z"),
  flag: P("M5.5 20.5V4.5M5.5 5h11.5l-2.2 3.7L17 12.4H5.5"),
  cornerDownRight: P("M5.5 4.5v7a3 3 0 0 0 3 3h10M15 11l3.5 3.5L15 18"),
  veil: P("M3.5 9.8c2.6-1.6 5.5-2.3 8.5-2.3s5.9.7 8.5 2.3c0 3.9-2.1 6.7-4.9 6.7-1.8 0-2.4-1.6-3.6-1.6s-1.8 1.6-3.6 1.6c-2.8 0-4.9-2.8-4.9-6.7z") + C(8.6, 11.3, 1.1, true) + C(15.4, 11.3, 1.1, true),
  steer: C(12, 12, 8.5) + C(12, 12, 2) + P("M12 3.5V10M5 16.5l5.3-3.3M19 16.5l-5.3-3.3"),
  queue: P("M4.5 6.5h11M4.5 11.5h11M4.5 16.5h6") + C(17.5, 16.5, 3) + P("M17.5 15.2v1.5l1 .7"),
  /** Back to an earlier point: a dial turned counter-clockwise. */
  rewind: P("M4.8 13.5A7.5 7.5 0 1 0 6.7 6.7L4.5 8.9M4.5 4.5v4.4h4.4M12 8.5V12l2.4 1.6"),
  /** Fast mode: forward, twice. */
  /** Fast mode: the bolt (the only meaning of a bolt in Tessera). */
  fast: P("M13 3.5L5 13.5h6.2l-1 7L18.5 10.5h-6.2z"),
  /** YOLO (never asks): a shield struck through. */
  shieldOff: P("M8.2 4.9 12 3.5l7 2.6v5.5c0 1.5-.4 2.9-1.1 4.1M16.1 18.4A10 10 0 0 1 12 20.5c-4.1-1.3-7-4.7-7-8.9V6.1M3.5 3.5l17 17"),
  /** "+" for a new tile (chat, web page, note, image…). */
  addTile: R(3.5, 3.5, 17, 17, 2.5) + P("M12 8v8M8 12h8"),
  /** A note from the notes library: a page with lines. */
  note: P("M6.5 3.5h7.5l3.5 3.5v13.5h-11zM14 3.5V7h3.5M9 11.5h6M9 14.5h6M9 17.5h3.5"),
} as const;

export type IconName = keyof typeof ICONS;

export function Icon(props: { name: IconName; size?: number; class?: string; stroke?: number } & JSX.AriaAttributes) {
  const [local, rest] = splitProps(props, ["name", "size", "class", "stroke"]);
  return (
    <svg
      class={"icon" + (local.class ? " " + local.class : "")}
      viewBox="0 0 24 24"
      width={local.size ?? 18}
      height={local.size ?? 18}
      fill="none"
      stroke="currentColor"
      stroke-width={local.stroke ?? 1.5}
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden={rest["aria-label"] ? undefined : "true"}
      role={rest["aria-label"] ? "img" : undefined}
      {...rest}
      innerHTML={ICONS[local.name]}
    />
  );
}


/**
 * The Tessera mark: a "T" laid out of tiles (tesserae) in a tiling layout —
 * a luminous crossbar and stem, with frosted side tiles. Colours follow the
 * accent (brand teal by default) and the theme.
 */
/**
 * The mark: a T laid in five hand-cut tesserae, the one at the crossing in
 * gold (the accent material). Irregular on purpose — cut by hand, not drawn
 * on a grid.
 */
export function Logo(props: { size?: number; class?: string }) {
  return (
    <svg class={"logo" + (props.class ? " " + props.class : "")} viewBox="0 0 24 24" width={props.size ?? 24} height={props.size ?? 24} aria-hidden="true">
      <path class="lp l1" d="M2.1 3.25 8.45 2.95 8.7 8.85 2.05 9.1Z" />
      <path class="lg" d="M9.3 3 14.6 3.2 14.45 9 9.4 8.8Z" />
      <path class="lp l2" d="M15.3 3.15 21.9 3.35 21.95 8.8 15.2 9.05Z" />
      <path class="lp l3" d="M9.5 9.7 14.4 9.85 14.6 15 9.3 15.2Z" />
      <path class="lp l4" d="M9.35 15.9 14.6 15.7 14.4 21.1 9.6 20.9Z" />
    </svg>
  );
}

/** Tessera wordmark next to the mark. */
export function Brand(props: { size?: number; class?: string }) {
  return (
    <span class={"brand" + (props.class ? " " + props.class : "")}>
      <Logo size={props.size ?? 22} />
      <span class="brand-name">Tessera</span>
    </span>
  );
}

// One observer pauses every working indicator that is off screen (CSS
// animations keep ticking on some compositors even when nothing shows).
let tessIo: IntersectionObserver | null = null;
function watchTess(el: HTMLElement) {
  if (typeof IntersectionObserver === "undefined") return () => {};
  tessIo ??= new IntersectionObserver((es) => {
    for (const e of es) (e.target as HTMLElement).toggleAttribute("data-off", !e.isIntersecting);
  });
  tessIo.observe(el);
  return () => tessIo?.unobserve(el);
}

/**
 * "Tessellate": four tiny squares filling one by one in gold, the app's only
 * looping animation (opacity with steps(), paused when off screen).
 */
export function Tessellate(props: { size?: number; class?: string; label?: string }) {
  let el!: HTMLSpanElement;
  onMount(() => onCleanup(watchTess(el)));
  return (
    <span
      ref={el}
      class={"tess" + (props.class ? " " + props.class : "")}
      style={{ "--tess": `${props.size ?? 10}px` }}
      role={props.label ? "status" : undefined}
      aria-label={props.label}
      aria-hidden={props.label ? undefined : "true"}
    >
      <i />
      <i />
      <i />
      <i />
    </span>
  );
}

/** Busy state everywhere (buttons, loading lists): the tessellate indicator. */
export function Spinner(props: { size?: number; class?: string }) {
  return <Tessellate size={Math.round((props.size ?? 16) * 0.72)} class={props.class} />;
}

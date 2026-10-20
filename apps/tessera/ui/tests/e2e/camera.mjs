// Attaching from the composer's paperclip.
//   Phones and tablets: a small menu next to the button, "Take photo" and "Photos & files". "Take photo" is a
//   second hidden file input with `capture`, which the Android app answers by opening the camera app (the
//   emulator run covers that part). The photo lands as an image attachment like a picked one: downscaled, and
//   upright whatever the camera's orientation tag says. A second photo works, and so does a cancelled one.
//   Computers: one button that opens the picker directly, no menu, no camera input.
// File choosers are intercepted over CDP (Page.fileChooserOpened) so the suite sees which <input> was opened and
// can hand it a photo from disk.
import fs from "node:fs";
import path from "node:path";
import { SCRATCH, SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";
import { photoBytes } from "../fixtures/photo-bytes.mjs";

const SID = "ses_2b7e91c04d5a";
const C = ".pane.focused .composer";
const TMP = path.join(SCRATCH, "camera");
fs.mkdirSync(TMP, { recursive: true });

// Helpers that run in the page: a camera-style JPEG (stored sideways when it has an orientation tag), and
// what an image looks like once decoded.
const PAGE_LIB = `
window.__photos = {};
window.__withExif = (jpeg, o) => {
  // APP1 "Exif" with one IFD entry: Orientation (0x0112) = o. Placed right after the start-of-image marker.
  const tiff = [0x49, 0x49, 0x2a, 0x00, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, o, 0, 0, 0, 0, 0, 0, 0];
  const body = [0x45, 0x78, 0x69, 0x66, 0, 0, ...tiff];
  const len = body.length + 2;
  const out = new Uint8Array(jpeg.length + body.length + 4);
  out.set(jpeg.subarray(0, 2), 0);
  out.set([0xff, 0xe1, len >> 8, len & 255, ...body], 2);
  out.set(jpeg.subarray(2), 2 + body.length + 4);
  return out;
};
window.__makePhoto = async (name, w, h, orient) => {
  const c = document.createElement('canvas'); c.width = w; c.height = h;
  const x = c.getContext('2d');
  const g = x.createLinearGradient(0, 0, w, h); g.addColorStop(0, '#345'); g.addColorStop(1, '#a86');
  x.fillStyle = g; x.fillRect(0, 0, w, h);
  // Some texture.
  for (let i = 0; i < 4000; i++) { x.fillStyle = 'hsl(' + ((i * 37) % 360) + ' 40% ' + (30 + ((i * 13) % 40)) + '%)'; x.fillRect((i * 7919) % w, (i * 104729) % h, 30 + (i % 90), 30 + ((i * 3) % 90)); }
  // Sensor noise, so the file is as heavy as a real one.
  const px = x.getImageData(0, 0, w, h), d = px.data;
  let r = 1234567;
  for (let i = 0; i < d.length; i += 4) { r = (r * 1103515245 + 12345) & 0x7fffffff; const n = ((r >> 16) & 15) - 8; d[i] += n; d[i + 1] += n; d[i + 2] += n; }
  x.putImageData(px, 0, 0);
  // The marker: the stored picture's top-left corner.
  x.fillStyle = '#0c0'; x.fillRect(0, 0, w / 4, h / 4);
  const blob = await new Promise((r) => c.toBlob(r, 'image/jpeg', 0.92));
  let bytes = new Uint8Array(await blob.arrayBuffer());
  if (orient) bytes = window.__withExif(bytes, orient);
  window.__photos[name] = bytes;
  let bin = '';
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
};
window.__exifOrientation = (bytes) => {
  if (bytes[0] !== 0xff || bytes[1] !== 0xd8) return null;
  let i = 2;
  while (i + 4 < bytes.length && bytes[i] === 0xff) {
    const m = bytes[i + 1], len = (bytes[i + 2] << 8) | bytes[i + 3];
    if (m === 0xe1 && String.fromCharCode(...bytes.subarray(i + 4, i + 8)) === 'Exif') {
      const t = i + 10, le = bytes[t] === 0x49;
      const u16 = (o) => (le ? bytes[t + o] | (bytes[t + o + 1] << 8) : (bytes[t + o] << 8) | bytes[t + o + 1]);
      const u32 = (o) => (le ? (u16(o) | (u16(o + 2) << 16)) >>> 0 : ((u16(o) << 16) | u16(o + 2)) >>> 0);
      const ifd = u32(4), n = u16(ifd);
      for (let k = 0; k < n; k++) { const e = ifd + 2 + k * 12; if (u16(e) === 0x0112) return u16(e + 8); }
      return 1;
    }
    if (m === 0xda) break;
    i += 2 + len;
  }
  return 1;
};
// What the model would get: the size, which corner is green, and the orientation tag still in the file (1 = none).
window.__look = async (b64, mime) => {
  const bin = atob(b64); const bytes = new Uint8Array(bin.length); for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  const img = new Image(); img.src = 'data:' + mime + ';base64,' + b64; await img.decode();
  const c = document.createElement('canvas'); c.width = 100; c.height = 100;
  const x = c.getContext('2d'); x.drawImage(img, 0, 0, 100, 100);
  const at = (px, py) => { const d = x.getImageData(px, py, 1, 1).data; return d[1] > 140 && d[0] < 90 && d[2] < 90; };
  const hits = { tl: at(10, 10), tr: at(90, 10), bl: at(10, 90), br: at(90, 90) };
  return { w: img.naturalWidth, h: img.naturalHeight, bytes: bytes.length, green: Object.keys(hits).filter((k) => hits[k]).join(','), tag: window.__exifOrientation(bytes) };
};
`;

/** A photo on disk (for the file chooser) and in the page (window.__photos[name]). */
async function photoFile(p, name, w, h, orient) {
  const length = await p.eval(`window.__makePhoto(${JSON.stringify(name)}, ${w}, ${h}, ${orient ?? 0}).then(() => window.__photos[${JSON.stringify(name)}].length)`);
  const bytes = await photoBytes(p, name, length);
  const file = path.join(TMP, name);
  fs.writeFileSync(file, bytes);
  return file;
}
const pngFile = async (p, name) => {
  const b64 = await p.eval(`(() => { const c = document.createElement('canvas'); c.width = 120; c.height = 80; const x = c.getContext('2d'); x.fillStyle = '#36c'; x.fillRect(0, 0, 120, 80); const u = c.toDataURL('image/png'); return u.slice(u.indexOf(',') + 1); })()`);
  const file = path.join(TMP, name);
  fs.writeFileSync(file, Buffer.from(b64, "base64"));
  return file;
};

/** Run `action` and return the file chooser it opens: its mode and the <input>'s attributes. */
async function chooserAfter(p, action, ms = 4000) {
  let off;
  const got = new Promise((res) => {
    const t = setTimeout(() => res(null), ms);
    off = p.on("Page.fileChooserOpened", (e) => {
      clearTimeout(t);
      res(e);
    });
  });
  await action();
  const ev = await got;
  off();
  if (!ev) return null;
  const { node } = await p.send("DOM.describeNode", { backendNodeId: ev.backendNodeId });
  const attrs = {};
  for (let i = 0; i < (node.attributes ?? []).length; i += 2) attrs[node.attributes[i]] = node.attributes[i + 1];
  return { mode: ev.mode, attrs, id: ev.backendNodeId };
}
const feed = (p, chooser, files) => p.send("DOM.setFileInputFiles", { files, backendNodeId: chooser.id });

const tapSel = async (p, sel, mobile) => {
  const b = await p.box(sel);
  if (!b) throw new Error("no element " + sel);
  if (mobile) await p.tap(b.cx, b.cy);
  else await p.mouse(b.cx, b.cy);
  await sleep(250);
};
const attachBtn = `${C} .comp-btn[aria-label^="Attach"]`;
const menu = (p) =>
  p.eval(`(() => {
    const m = document.querySelector('.popover.attach-pop');
    if (!m) return null;
    const r = m.getBoundingClientRect(), b = document.querySelector(${JSON.stringify(attachBtn)}).getBoundingClientRect();
    return {
      items: [...m.querySelectorAll('.menu-item')].map((i) => i.textContent.trim()),
      rowH: [...m.querySelectorAll('.menu-item')].map((i) => Math.round(i.getBoundingClientRect().height)),
      icons: [...m.querySelectorAll('.menu-item svg')].length,
      sheet: m.classList.contains('as-sheet') || !!document.querySelector('.sheet-backdrop'),
      w: Math.round(r.width), h: Math.round(r.height), left: Math.round(r.left), right: Math.round(r.right), top: Math.round(r.top), bottom: Math.round(r.bottom),
      btnTop: Math.round(b.top), btnLeft: Math.round(b.left), vw: innerWidth, vh: innerHeight,
    };
  })()`);
const state = (p) =>
  p.eval(`(() => {
    const c = document.querySelector(${JSON.stringify(C)});
    const imgs = [...c.querySelectorAll('.attachment img')];
    return {
      images: imgs.length,
      names: imgs.map((i) => i.alt),
      dims: imgs.map((i) => [i.naturalWidth, i.naturalHeight]),
      chips: [...c.querySelectorAll('.file-chip-name')].map((e) => e.textContent),
      menuOpen: !!document.querySelector('.popover'),
      inputs: [...c.querySelectorAll('input[type=file]')].map((i) => ({ accept: i.accept, capture: i.getAttribute('capture'), multiple: i.multiple, hidden: i.hidden })),
      buttons: c.querySelectorAll('.comp-btn[aria-label^="Attach"]').length,
      popup: c.querySelector('.comp-btn[aria-label^="Attach"]')?.getAttribute('aria-haspopup') ?? null,
      label: c.querySelector('.comp-btn[aria-label^="Attach"]')?.getAttribute('aria-label'),
    };
  })()`);
const clearAtts = (p) => p.eval(`(() => { for (const b of document.querySelectorAll(${JSON.stringify(C)} + ' .attachment-x, ' + ${JSON.stringify(C)} + ' .file-chip-x')) b.click(); return 1; })()`);
const themes = async (p, label, fn) => {
  for (const t of ["dark", "light"]) {
    await p.theme(t);
    await sleep(200);
    await fn(t);
  }
  await p.theme("dark");
};

async function open(label, w, h, mobile, setup) {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const p = await device();
  await p.viewport(w, h, mobile, mobile ? 2 : 1);
  await signIn(p, `Camera ${label}`);
  await p.theme("dark");
  await p.eval(PAGE_LIB);
  await p.send("Page.setInterceptFileChooserDialog", { enabled: true });
  await setup?.(p);
  await p.waitFor(`document.querySelector('.composer textarea')`, 8000);
  await sleep(500);
  return p;
}
const openChat = (p) => p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);

run(async () => {
  // ── a phone ──
  {
    const label = "phone";
    const p = await open(label, 375, 812, true, async (p) => {
      await openChat(p);
      await sleep(1200);
    });
    const photo = await photoFile(p, "IMG_sideways.jpg", 4000, 3000, 6);
    console.log(label, "photo", fs.statSync(photo).size, "bytes");
    check(fs.statSync(photo).size > 1_500_000, `${label}: the test photo is camera-sized (12 MP, ${(fs.statSync(photo).size / 1e6).toFixed(1)} MB)`);

    let s = await state(p);
    check(s.buttons === 1, `${label}: one attach button in the composer`, s);
    check(s.popup === "menu" && s.label === "Attach a photo or file", `${label}: it is labelled as a menu button`, s);
    check(s.inputs.length === 2, `${label}: two hidden file inputs (pick, camera)`, s.inputs);
    check(s.inputs[0]?.multiple && !s.inputs[0].capture && s.inputs[0].hidden, `${label}: the first is today's multi-file picker`, s.inputs[0]);
    check(s.inputs[1]?.accept === "image/*" && s.inputs[1].capture === "environment" && !s.inputs[1].multiple && s.inputs[1].hidden, `${label}: the second takes one image with capture=environment`, s.inputs[1]);

    // The menu: compact, anchored above the button, not a sheet.
    await tapSel(p, attachBtn, true);
    let m = await menu(p);
    check(!!m, `${label}: tapping the paperclip opens the menu`);
    check(m && m.items.join("|") === "Take photo|Photos & files", `${label}: it offers Take photo and Photos & files, nothing else`, m?.items);
    check(m && !m.sheet, `${label}: it is a small popover, not a bottom sheet`);
    check(m && m.h <= 120 && m.w <= 260, `${label}: and compact (${m?.w}×${m?.h}px)`);
    check(m && m.rowH.every((h) => h >= 44) && m.icons === 2, `${label}: rows are finger-sized (${m?.rowH.join(",")}px) with an icon each`);
    check(m && m.bottom <= m.btnTop && m.left >= 8 && m.right <= m.vw - 8, `${label}: it opens above the button and stays on screen`, m);
    await themes(p, label, (t) => p.shot(`${SHOTS}/camera-menu-${label}-${t}.png`));
    // A tap away closes it and opens nothing.
    let ch = await chooserAfter(p, () => p.tap(187, 300), 600);
    check(!ch && !(await state(p)).menuOpen, `${label}: a tap elsewhere closes the menu and opens no picker`);

    // The back gesture closes it, and the app stays where it was.
    await tapSel(p, attachBtn, true);
    check(!!(await menu(p)), `${label}: the menu opens again`);
    await p.eval(`history.back()`);
    await sleep(400);
    check(!(await menu(p)) && (await p.eval(`!!document.querySelector('.workspace')`)), `${label}: the back gesture closes the menu and nothing else`);

    // Take photo → the camera input's chooser (the Android app launches the camera app here).
    await tapSel(p, attachBtn, true);
    ch = await chooserAfter(p, () => tapSel(p, ".popover.attach-pop .menu-item:nth-child(1)", true));
    check(!!ch, `${label}: "Take photo" opens a file chooser`);
    check(ch?.attrs.capture === "environment" && ch.attrs.accept === "image/*" && ch.mode === "selectSingle", `${label}: …the camera input (capture=environment, one image)`, ch);
    check(!(await state(p)).menuOpen, `${label}: the menu closed behind it`);
    // Cancelled (nothing handed over): no attachment, nothing stuck.
    await sleep(300);
    s = await state(p);
    check(s.images === 0 && !s.chips.length && !s.menuOpen, `${label}: cancelling leaves the composer as it was`, s);
    // Again, and this time a photo comes back.
    await tapSel(p, attachBtn, true);
    ch = await chooserAfter(p, () => tapSel(p, ".popover.attach-pop .menu-item:nth-child(1)", true));
    check(ch?.attrs.capture === "environment", `${label}: after a cancel, "Take photo" opens the camera input again`, ch);
    await feed(p, ch, [photo]);
    await p.waitFor(`document.querySelectorAll(${JSON.stringify(C + " .attachment img")}).length === 1`, 15000);
    s = await state(p);
    check(s.images === 1 && s.names[0] === "IMG_sideways.jpg", `${label}: the photo is an image attachment`, s);
    const [pw, ph] = s.dims[0];
    check(Math.max(pw, ph) === 2048 && ph > pw && Math.abs(pw / ph - 0.75) < 0.01, `${label}: 4000×3000 sideways → upright portrait, longer side 2048 (${pw}×${ph})`);
    await themes(p, label, (t) => p.shot(`${SHOTS}/camera-photo-${label}-${t}.png`));

    // A second photo, with the very same file (the input's value was cleared, so it fires change again).
    await tapSel(p, attachBtn, true);
    ch = await chooserAfter(p, () => tapSel(p, ".popover.attach-pop .menu-item:nth-child(1)", true));
    await feed(p, ch, [photo]);
    await p.waitFor(`document.querySelectorAll(${JSON.stringify(C + " .attachment img")}).length === 2`, 15000).catch(() => {});
    s = await state(p);
    check(s.images === 2, `${label}: a second photo (same file) attaches too`, s);

    // Photos & files → the usual picker: several files, images and others.
    const png = await pngFile(p, "shot.png");
    const txt = path.join(TMP, "notes.txt");
    fs.writeFileSync(txt, "hello from a file\n");
    await tapSel(p, attachBtn, true);
    ch = await chooserAfter(p, () => tapSel(p, ".popover.attach-pop .menu-item:nth-child(2)", true));
    check(ch && !("capture" in ch.attrs) && ch.mode === "selectMultiple", `${label}: "Photos & files" opens the plain multi-file picker (no capture)`, ch);
    await feed(p, ch, [png, txt]);
    await p.waitFor(`document.querySelectorAll(${JSON.stringify(C + " .attachment img")}).length === 3`, 8000).catch(() => {});
    s = await state(p);
    check(s.images === 3 && s.chips.includes("notes.txt"), `${label}: picked files arrive as before (an image and a text file)`, s);

    // Sent: every image is small enough, upright, and the tag is gone.
    await p.eval(`document.querySelector(${JSON.stringify(C + " textarea")}).focus()`);
    await p.type("What is this?");
    await tapSel(p, `${C} .send-btn`, true);
    await p.eval(`__mod('lib/store.ts').then((m) => { window.__st = m.state; return 1; })`);
    await p.waitFor(`window.__st.sessions['${SID}'].entries.some((e) => e.kind === 'user' && JSON.stringify(e.content).includes('What is this?'))`, 8000);
    const sent = await p.eval(`(async () => {
      const e = window.__st.sessions['${SID}'].entries.findLast((e) => e.kind === 'user');
      const out = [];
      for (const c of e.content) if (c.type === 'image') out.push({ mime: c.mime, ...(await window.__look(c.data, c.mime)) });
      return out;
    })()`);
    const photos = sent.filter((i) => i.mime === "image/jpeg");
    check(photos.length === 2, `${label}: the message carries both photos as images`, sent);
    check(photos.every((i) => i.w === 1536 && i.h === 2048), `${label}: …each 1536×2048`, photos);
    check(photos.every((i) => i.green === "tr"), `${label}: …upright (the sideways picture's corner is at the top right)`, photos.map((i) => i.green));
    check(photos.every((i) => i.tag === 1 && i.bytes < 1_500_000), `${label}: …no orientation tag left, ${photos.map((i) => (i.bytes / 1e3).toFixed(0) + " KB").join(" and ")} (from ${(fs.statSync(photo).size / 1e6).toFixed(1)} MB)`, photos);
    await sleep(700);
    await themes(p, label, (t) => p.shot(`${SHOTS}/camera-sent-${label}-${t}.png`));
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
  }

  // ── a private chat on a phone: a photo is an image, so it's allowed ──
  {
    const label = "private";
    const p = await open(label, 375, 812, true, async (p) => {
      await p.eval(`__mod('lib/actions.ts').then((m) => m.newPrivateChat())`);
      await sleep(1000);
    });
    const photo = await photoFile(p, "IMG_private.jpg", 3264, 2448, 6);
    let s = await state(p);
    check(s.inputs[0]?.accept.startsWith("image/*,text/*") && s.inputs[1]?.accept === "image/*" && s.inputs[1].capture === "environment", `${label}: the picker is restricted, the camera input still takes images`, s.inputs);
    await tapSel(p, attachBtn, true);
    const m = await menu(p);
    check(m && m.items.join("|") === "Take photo|Photos & files", `${label}: same menu`, m?.items);
    const ch = await chooserAfter(p, () => tapSel(p, ".popover.attach-pop .menu-item:nth-child(1)", true));
    check(ch?.attrs.capture === "environment", `${label}: Take photo opens the camera input`, ch);
    await feed(p, ch, [photo]);
    await p.waitFor(`document.querySelectorAll(${JSON.stringify(C + " .attachment img")}).length === 1`, 15000).catch(() => {});
    s = await state(p);
    check(s.images === 1 && !s.chips.length, `${label}: the photo is attached as an image (no refusal toast)`, s);
    check(s.dims[0] && s.dims[0][0] < s.dims[0][1] && Math.max(...s.dims[0]) === 2048, `${label}: …upright and downscaled (${s.dims[0]})`, s.dims);
    await p.shot(`${SHOTS}/camera-photo-private.png`);
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
  }

  // ── the first screen of a new chat on a phone (the composer's buttons sit below the box) ──
  {
    const label = "hero";
    const p = await open(label, 375, 812, true);
    const sel = ".composer-wrap.in-hero .comp-btn[aria-label^='Attach']";
    check(await p.eval(`!!document.querySelector(${JSON.stringify(sel)})`), `${label}: the new-chat screen has the attach button`);
    await tapSel(p, sel, true);
    const m = await p.eval(`(() => { const m = document.querySelector('.popover.attach-pop'); if (!m) return null; const r = m.getBoundingClientRect(); return { items: m.querySelectorAll('.menu-item').length, h: Math.round(r.height), w: Math.round(r.width), l: Math.round(r.left), r: Math.round(r.right), t: Math.round(r.top), b: Math.round(r.bottom), sheet: m.classList.contains('as-sheet') }; })()`);
    check(m && m.items === 2 && !m.sheet && m.h <= 120 && m.l >= 8 && m.r <= 367 && m.t >= 8 && m.b <= 804, `${label}: the menu from the new-chat screen is compact and on screen`, m);
    await themes(p, label, (t) => p.shot(`${SHOTS}/camera-menu-hero-${t}.png`));
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
  }

  // ── a tablet / unfolded Fold ──
  {
    const label = "fold";
    const p = await open(label, 841, 701, true, async (p) => {
      await openChat(p);
      await sleep(1200);
    });
    await tapSel(p, attachBtn, true);
    const m = await menu(p);
    check(m && m.items.length === 2 && !m.sheet && m.h <= 120 && m.bottom <= m.btnTop, `${label}: the same compact menu`, m);
    await themes(p, label, (t) => p.shot(`${SHOTS}/camera-menu-${label}-${t}.png`));
    const ch = await chooserAfter(p, () => tapSel(p, ".popover.attach-pop .menu-item:nth-child(1)", true));
    check(ch?.attrs.capture === "environment", `${label}: Take photo opens the camera input`, ch);
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
  }

  // ── a computer: unchanged ──
  {
    const label = "desktop";
    const p = await open(label, 1440, 900, false, async (p) => {
      await openChat(p);
      await sleep(1200);
    });
    let s = await state(p);
    check(s.buttons === 1 && s.popup === null && s.label === "Attach files", `${label}: one plain "Attach files" button`, s);
    check(s.inputs.length === 1 && !s.inputs[0].capture && s.inputs[0].multiple, `${label}: only the picker input exists (no camera input)`, s.inputs);
    const ch = await chooserAfter(p, () => tapSel(p, attachBtn, false));
    check(!!ch && !("capture" in ch.attrs) && ch.mode === "selectMultiple", `${label}: one click opens the picker straight away`, ch);
    check(!(await state(p)).menuOpen, `${label}: …with no menu`);
    const png = await pngFile(p, "desktop.png");
    await feed(p, ch, [png]);
    await p.waitFor(`document.querySelectorAll(${JSON.stringify(C + " .attachment img")}).length === 1`, 8000).catch(() => {});
    s = await state(p);
    check(s.images === 1, `${label}: a picked image still attaches`, s);
    await themes(p, label, (t) => p.shot(`${SHOTS}/camera-desktop-${t}.png`));
    check(!p.errors.length, `${label}: no page errors`, p.errors);

    // ── how photos of different shapes come out (the same code the composer runs) ──
    const cases = [
      ["12 MP, sideways (tag 6)", 4000, 3000, 6, { w: 1536, h: 2048, green: "tr" }],
      ["12 MP, upside down (tag 3)", 4000, 3000, 3, { w: 2048, h: 1536, green: "br" }],
      ["12 MP, no tag", 4000, 3000, 0, { w: 2048, h: 1536, green: "tl" }],
      ["small, sideways (tag 6)", 800, 600, 6, { w: 600, h: 800, green: "tr" }],
      ["small, turned left (tag 8)", 800, 600, 8, { w: 600, h: 800, green: "bl" }],
      ["small, no tag (sent as it is)", 800, 600, 0, { w: 800, h: 600, green: "tl", same: true }],
    ];
    for (const [what, w, h, o, want] of cases) {
      const name = `case-${w}-${o}.jpg`;
      await p.eval(`window.__makePhoto(${JSON.stringify(name)}, ${w}, ${h}, ${o}).then(() => 1)`);
      const t0 = Date.now();
      const got = await p.eval(`(async () => {
        const m = await __mod('lib/images.ts');
        const f = new File([window.__photos[${JSON.stringify(name)}]], ${JSON.stringify(name)}, { type: 'image/jpeg' });
        const t = performance.now();
        const a = await m.fileToAttachment(f);
        const ms = Math.round(performance.now() - t);
        return { ms, att: [a.width, a.height], mime: a.mime, ...(await window.__look(a.data, a.mime)) };
      })()`);
      console.log(`  ${what}: ${JSON.stringify(got)} (${Date.now() - t0} ms incl. making it)`);
      check(got.w === want.w && got.h === want.h && got.green === want.green && got.tag === 1, `${what} → ${want.w}×${want.h}, upright, no tag left`, got);
      if (want.same) check(got.bytes === (await p.eval(`window.__photos[${JSON.stringify(name)}].length`)), `${what}: the file itself is attached, untouched`, got);
    }
    await p.close();
  }
  finish();
});

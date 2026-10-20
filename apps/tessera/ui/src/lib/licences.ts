// The licence texts every build of the interface carries, so each app built from it (the desktop
// app, the Android app, the web app) has them inside, as the licences ask: Tessera's own licence and
// NOTICE, and the third-party licence texts and copyright notices (scripts/licenses.mjs writes those).
// Loaded only when someone opens them (Settings → Help & tour → Open-source licences).
import license from "../../../../../LICENSE?raw";
import notice from "../../../../../NOTICE?raw";
import thirdParty from "../../../../../THIRD_PARTY_LICENSES.txt?raw";

export const LICENCE_TEXTS: { title: string; text: string }[] = [
  { title: "Tessera's licence (Apache License 2.0)", text: license },
  { title: "NOTICE", text: notice },
  { title: "Third-party licences and copyright notices", text: thirdParty },
];

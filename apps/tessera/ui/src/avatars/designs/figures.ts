// Figures with the look of classical deities and mythic beings. Never named: ids describe the look.
import type { Design } from "../engine";
import { bolt, bust, crescent, dove, faceFill, features, flame, grapes, lyre, rays, rose, star, vineLeaf, waves, wheatEar, wreath } from "./kit";

export const FIGURES: Design[] = [
  {
    // Bearded elder, laurel crown, thunderbolt.
    id: "laurel-beard",
    bg: "l",
    draw: (c) => {
      bust(c, "W", { rim: "q" });
      c.poly([28, 102, 41, 76, 51, 75.5, 40, 102], "p");
      c.ellipse(50, 42, 19.5, 20.5, "x", 0, { rim: "X", t: 3 });
      faceFill(c);
      c.blob([37.5, 47, 37.5, 58, 41.5, 69, 50, 78, 58.5, 69, 62.5, 58, 62.5, 47, 57.5, 55, 50, 57.5, 42.5, 55], "x", { rim: "X", t: 2.6 });
      c.curve([44, 63, 46, 70, 49.5, 76], 1.3, "X");
      c.curve([56, 63, 54, 70, 50.5, 76], 1.3, "X");
      c.curve([43.2, 55, 46.8, 52.6, 50, 53.4, 53.2, 52.6, 56.8, 55], 2.4, "X");
      features(c, { brow: "X", mouth: "R" });
      wreath(c, 50, 45, 17.2, 196, 344, { leaf: "v", leaf2: "V", step: 12 });
      bolt(c, 81, 35, 1.05, 12);
    },
  },
  {
    // Helmeted wise woman: crested helm with cheek guards, a little owl perched on top.
    id: "owl-helm",
    bg: "O",
    draw: (c) => {
      c.sym(() => c.blob([38, 42, 33, 54, 32, 68, 36, 78, 43, 76, 42, 62, 41, 50], "h", { rim: "H", t: 2 }));
      bust(c, "W", { rim: "q", y: 79 });
      c.path("M28 102C31 91 39 85 50 85C61 85 69 91 72 102Z", "g", { rim: "o", t: 2.4 });
      c.circle(50, 94.5, 4.5, "k", { rim: "R", t: 1.4 });
      faceFill(c, { y: 51, rx: 12, ry: 15 });
      features(c, { y: 51, rx: 12, ry: 15, brow: "H", brows: "flat" });
      // helm: dome, brow ridge, cheek guards
      c.path("M31.5 45C30.5 33 38 26 50 26C62 26 69.5 33 68.5 45C63 41 57 39.5 50 39.5C43 39.5 37 41 31.5 45Z", "g", { rim: "o", t: 2.2 });
      c.sym(() => c.path("M31.5 44C33.5 42 36 41 38.5 41L38.5 57C36.5 59 34.5 60 32.5 59C31.5 54 31 49 31.5 44Z", "g", { rim: "o", t: 1.8 }));
      c.line([36, 37, 50, 35, 64, 37], 1.6, "o");
      // owl perched on the crest
      c.blob([39, 12, 42, 7, 50, 5.5, 58, 7, 61, 12, 61.5, 20, 57.5, 26, 50, 28, 42.5, 26, 38.5, 20], "b", { rim: "H", t: 1.6, out: 1 });
      c.sym(() => {
        c.poly([39.5, 11, 39, 3.5, 44.5, 7], "b", { t: 1.2 });
        c.circle(45.2, 14, 4.2, "w", { t: 1.2, rim: "o" });
        c.dot(45.4, 14.1, 2.6, "k");
        c.dot(44, 21.5, 1.6, "w", 45);
      });
      c.poly([48.6, 16.5, 51.4, 16.5, 50, 20.5], "G");
      c.dot(50, 24, 1.6, "w", 45);
    },
  },
  {
    // Youth in a winged travelling cap, with a herald's staff.
    id: "winged-cap",
    bg: "t",
    draw: (c) => {
      c.ellipse(50, 46, 16.5, 16.5, "H", 0, { rim: "k", t: 2.4 });
      bust(c, "l", { rim: "n" });
      c.poly([50, 76, 72, 80, 88, 102, 62, 102], "n");
      c.circle(64, 80, 3, "G", { t: 1.4 });
      faceFill(c, { y: 48 });
      c.sym(() => {
        for (const [x, y] of [
          [36.2, 42],
          [35.4, 49],
          [36.5, 56],
        ])
          c.dot(x, y, 3.2, "k", 30);
      });
      features(c, { y: 48, cheeks: "r" });
      // wings sweeping up from the cap
      c.sym(() => {
        c.path("M39 32C33 31 25 27 19 20C15 15 13 10 13 6C16 9 18 10 20 10C20 13 22 14 24 14C24 17 26 18 28 18C29 21 31 22 33 22C34 25 36 26 39 26Z", "W", { t: 1.8, out: 1 });
        c.curve([37, 29, 29, 26, 22, 20, 17, 13], 1, "q");
        c.curve([36, 26, 30, 22, 26, 17], 0.9, "q");
      });
      // cap: brim and crown
      c.ellipse(50, 34, 21, 5, "o", 0, { rim: "b", t: 2 });
      c.path("M39 34C39 24 44 20 50 20C56 20 61 24 61 34Z", "o", { rim: "b", t: 2 });
      // herald's staff
      c.line([14, 97, 14, 62], 2.4, "G", { halo: 1 });
      c.ring(14, 82, 4.2, 1.6, "v");
      c.ring(14, 72, 3.6, 1.6, "v");
      c.poly([14, 61, 6, 56, 9, 62], "W");
      c.poly([14, 61, 22, 56, 19, 62], "W");
    },
  },
  {
    // Sea king with a trident, rising from the waves.
    id: "trident-crown",
    bg: "v",
    draw: (c) => {
      c.ellipse(50, 43, 19, 20, "n", 0, { rim: "k", t: 2.6 });
      faceFill(c);
      c.blob([37.5, 47, 36, 60, 40, 72, 50, 84, 60, 72, 64, 60, 62.5, 47, 57.5, 55, 50, 57.5, 42.5, 55], "n", { rim: "k", t: 2.4 });
      c.curve([43.2, 55, 46.8, 52.6, 50, 53.4, 53.2, 52.6, 56.8, 55], 2.4, "k");
      c.curve([45, 62, 46, 70, 49.5, 78], 1.2, "l");
      c.curve([55, 62, 54, 70, 50.5, 78], 1.2, "l");
      features(c, { brow: "k", brows: "frown", mouth: "R" });
      // crown with points
      c.poly([35, 34, 36, 22, 40.5, 29, 45, 18, 50, 27, 55, 18, 59.5, 29, 64, 22, 65, 34, 50, 31.5], "G", { rim: "o", t: 1.8 });
      // trident
      c.line([80, 98, 80, 26], 2.8, "G", { halo: 1 });
      c.line([72, 30, 88, 30], 2.4, "G");
      c.line([72, 30, 72, 16], 2.3, "G");
      c.line([88, 30, 88, 16], 2.3, "G");
      c.line([80, 26, 80, 9], 2.5, "G");
      c.poly([70, 17, 72, 11, 74, 17], "G");
      c.poly([86, 17, 88, 11, 90, 17], "G");
      c.poly([77.5, 10, 80, 3.5, 82.5, 10], "G");
      waves(c, 76, { water: "l", crest: "W", n: 3, h: 16 });
    },
  },
  {
    // Radiant youth crowned with sun rays, with a lyre.
    id: "sun-crown",
    bg: "R",
    draw: (c) => {
      rays(c, 50, 43, 20, 47, 16, "G", { w: 6, alt: 0.62, col2: "g" });
      c.circle(50, 43, 22.5, "g", { rim: "o", t: 2.2, out: 1 });
      c.ellipse(50, 44, 16.5, 17.5, "o", 0, { rim: "b", t: 2.4 });
      c.sym(() => {
        for (const [x, y] of [
          [37, 38],
          [36, 45],
          [37, 52],
        ])
          c.dot(x, y, 3.2, "b", 20);
      });
      bust(c, "W", { rim: "q" });
      c.poly([50, 76, 70, 80, 86, 102, 60, 102], "g");
      faceFill(c, { y: 47, rx: 12, ry: 15 });
      c.path("M38 38C40 31 45 29 50 29C55 29 60 31 62 38C58 34 54 34 50 35C46 34 42 34 38 38Z", "o", { t: 2 });
      features(c, { y: 47, rx: 12, ry: 15, cheeks: "r", brow: "b" });
      lyre(c, 78, 84, 0.78, { body: "b", arm: "G", str: "W" });
    },
  },
  {
    // Huntress with a crescent moon on her brow, bow and quiver, under the stars.
    id: "crescent-bow",
    bg: "n",
    draw: (c) => {
      star(c, 17, 20, 0.9, "W");
      star(c, 84, 14, 0.7, "G");
      star(c, 12, 50, 0.55, "G");
      // bow behind
      c.arc(54, 58, 36, -58, 58, 3.2, "o");
      c.line([73, 27.5, 73, 88.5], 0.9, "W");
      // quiver
      c.poly([20, 66, 29, 61, 38, 94, 29, 97], "b", { rim: "H", t: 1.6 });
      for (const x of [22.5, 27]) c.line([x, 65, x - 3.5, 55], 2.2, "W");
      c.ellipse(50, 46, 16, 17, "h", 0, { rim: "H", t: 2.4 });
      bust(c, "W", { rim: "q" });
      c.line([37, 79, 60, 101], 2.6, "b");
      faceFill(c, { y: 48, rx: 12, ry: 15 });
      c.path("M38 41C39 34 44 31 50 31C56 31 61 34 62 41C57 37 54 36.5 50 37C46 36.5 43 37 38 41Z", "h", { t: 2 });
      features(c, { y: 48, rx: 12, ry: 15, brow: "H", cheeks: "r" });
      crescent(c, 50, 25, 10, -90, "W", { rim: "x", t: 1.6, out: 1, cut: 0.5, inner: 0.84 });
    },
  },
  {
    // Reveler crowned with vine leaves and grapes.
    id: "vine-crown",
    bg: "g",
    draw: (c) => {
      c.ellipse(50, 46, 17, 18, "H", 0, { rim: "k", t: 2.4 });
      bust(c, "W", { rim: "q" });
      c.path("M20 102C24 88 34 80 44 78L64 102Z", "b", { rim: "H", t: 2 });
      for (const [x, y] of [
        [30, 94],
        [38, 88],
        [44, 97],
        [34, 100],
      ])
        c.dot(x, y, 2.4, "k", 20);
      faceFill(c, { y: 48 });
      features(c, { y: 48, cheeks: "r", brow: "H", mouth: "R" });
      vineLeaf(c, 50, 29, 1.25, 0);
      vineLeaf(c, 38, 33, 1.15, -50);
      vineLeaf(c, 62, 33, 1.15, 50);
      vineLeaf(c, 32, 43, 1, -95, "v");
      vineLeaf(c, 68, 43, 1, 95, "v");
      grapes(c, 32, 50, 1);
      grapes(c, 68, 50, 1);
    },
  },
  {
    // Smith in a conical felt cap, with hammer and forge flame.
    id: "hammer-cap",
    bg: "k",
    draw: (c) => {
      // forge flames
      flame(c, 84, 99, 1.3);
      flame(c, 70, 100, 0.85);
      c.ellipse(50, 45, 17, 17, "h", 0, { rim: "H", t: 2.4 });
      bust(c, "q", { rim: "Q" });
      c.path("M50 76L28 80L22 102L40 102Z", "f", { rim: "F", t: 2 });
      faceFill(c, { y: 47 });
      c.blob([37.5, 49, 37.5, 60, 41.5, 70, 50, 77, 58.5, 70, 62.5, 60, 62.5, 49, 57.5, 57, 50, 59.5, 42.5, 57], "h", { rim: "H", t: 2.4 });
      c.curve([43.2, 57, 46.8, 54.6, 50, 55.4, 53.2, 54.6, 56.8, 57], 2.4, "H");
      features(c, { y: 47, brow: "H", brows: "frown", mouth: "R" });
      // pilos cap
      c.path("M35 39C36 26 42 13 50 10C58 13 64 26 65 39C58 36 42 36 35 39Z", "t", { rim: "R", t: 2 });
      c.line([36, 38, 50, 35.6, 64, 38], 2, "R");
      // hammer
      c.line([14, 90, 26, 48], 2.8, "b", { halo: 1 });
      c.poly([16, 44, 34, 49, 36, 42, 18, 37], "X", { rim: "K", t: 1.6 });
    },
  },
  {
    // Warrior in a bronze helm with a great red crest.
    id: "crest-helm",
    bg: "w",
    draw: (c) => {
      // transverse crest
      c.path("M14 38C14 18 30 5 50 5C70 5 86 18 86 38L72 38C71 27 62 20 50 20C38 20 29 27 28 38Z", "R", { rim: "k", t: 2 });
      for (let a = 190; a <= 350; a += 20) {
        const r = (a * Math.PI) / 180;
        c.line([50 + Math.cos(r) * 17, 38 + Math.sin(r) * 17, 50 + Math.cos(r) * 31, 38 + Math.sin(r) * 31], 1, "t");
      }
      c.path("M8 102C12 86 28 78 50 77C72 78 88 86 92 102Z", "o", { rim: "b", t: 2.4 });
      c.path("M8 102C10 90 16 82 26 79L34 102Z", "R", { rim: "k", t: 1.8 });
      c.path("M92 102C90 90 84 82 74 79L66 102Z", "R", { rim: "k", t: 1.8 });
      c.line([38, 86, 50, 90, 62, 86], 1.5, "b");
      c.line([40, 94, 50, 97, 60, 94], 1.5, "b");
      // helm: bowl with cheek pieces, T-shaped opening
      c.path("M31 44C31 30 39 22 50 22C61 22 69 30 69 44L69 60C66 66 60 70 55 71L55 58C55 56 57 50 58 48L42 48C43 50 45 56 45 58L45 71C40 70 34 66 31 60Z", "o", { rim: "b", t: 2 });
      c.path("M37 44L63 44L63 49L57 49L55 57L55 70L45 70L45 57L43 49L37 49Z", "k", { out: 0, t: 1.4 });
      c.line([50, 41, 50, 57], 3, "o");
      c.sym(() => {
        c.tile(43.4, 46.2, 3.8, 2, "W");
        c.dot(44, 46.3, 1.8, "k");
      });
      c.line([33, 38, 50, 35, 67, 38], 1.6, "G");
      c.blob([46, 62, 50, 60.5, 54, 62, 54, 70, 50, 71, 46, 70], "f", { t: 1.4, out: 0 });
      c.line([47.5, 65.5, 52.5, 65.5], 1.2, "R");
    },
  },
  {
    // Goddess crowned with roses, a dove at her side.
    id: "rose-dove",
    bg: "L",
    draw: (c) => {
      c.blob([33, 36, 29, 54, 30, 72, 38, 80, 62, 80, 70, 72, 71, 54, 67, 36, 50, 27], "g", { rim: "o", t: 2.4 });
      bust(c, "r", { rim: "t" });
      faceFill(c, { y: 47 });
      c.path("M37 42C38 34 44 30 50 30C56 30 62 34 63 42C58 37 54 36 50 37C46 36 42 37 37 42Z", "g", { t: 2 });
      c.curve([34, 50, 36, 60, 33, 70], 1.2, "o");
      c.curve([66, 50, 64, 60, 67, 70], 1.2, "o");
      features(c, { y: 47, cheeks: "r", brow: "o", mouth: "R" });
      for (const [x, y] of [
        [42, 76],
        [46, 78],
        [50, 78.6],
        [54, 78],
        [58, 76],
      ])
        c.dot(x, y, 2.2, "W", 45);
      rose(c, 50, 29, 4.2);
      rose(c, 39, 32, 3.8);
      rose(c, 61, 32, 3.8);
      c.sym(() => {
        c.tile(44.5, 29, 3, 1.8, "v", -30);
        c.tile(34.5, 36, 3, 1.8, "v", 50);
      });
      dove(c, 80, 20, 0.62);
      dove(c, 20, 62, 0.52, { flip: true });
    },
  },
  {
    // Harvest queen, veiled, crowned with ears of wheat.
    id: "wheat-crown",
    bg: "V",
    draw: (c) => {
      for (const [a, l] of [
        [-50, 16],
        [-25, 19],
        [0, 20],
        [25, 19],
        [50, 16],
      ])
        wheatEar(c, 50 + Math.sin((a * Math.PI) / 180) * 14, 34 - Math.cos((a * Math.PI) / 180) * 12, l, a);
      c.path("M30 44C30 30 38 24 50 24C62 24 70 30 70 44C72 58 74 70 80 82L20 82C26 70 28 58 30 44Z", "w", { rim: "q", t: 2.4 });
      bust(c, "t", { rim: "R" });
      c.path("M22 102C24 90 28 84 34 80L40 102Z", "w", { rim: "q", t: 2 });
      c.path("M78 102C76 90 72 84 66 80L60 102Z", "w", { rim: "q", t: 2 });
      faceFill(c, { y: 48 });
      c.path("M37 43C38 36 44 33 50 33C56 33 62 36 63 43C58 39 54 38 50 38.5C46 38 42 39 37 43Z", "h", { t: 2 });
      features(c, { y: 48, brow: "h", cheeks: "r" });
      c.line([33, 33, 50, 29.5, 67, 33], 2.4, "G");
    },
  },
  {
    // Dark-crowned king of the underworld, with a two-pronged staff.
    id: "dark-crown",
    bg: "p",
    draw: (c) => {
      c.line([82, 98, 82, 22], 2.6, "q", { halo: 1 });
      c.line([76, 24, 88, 24], 2.2, "q");
      c.line([76, 24, 76, 8], 2.2, "q");
      c.line([88, 24, 88, 8], 2.2, "q");
      c.ellipse(50, 43, 19, 20, "k", 0, { rim: "K", t: 2.4 });
      bust(c, "k", { rim: "K" });
      c.line([36, 79, 50, 84, 64, 79], 2, "g");
      faceFill(c);
      c.blob([37.5, 47, 36.5, 60, 41, 71, 50, 80, 59, 71, 63.5, 60, 62.5, 47, 57.5, 55, 50, 57.5, 42.5, 55], "k", { rim: "K", t: 2.4 });
      c.curve([43.2, 55, 46.8, 52.6, 50, 53.4, 53.2, 52.6, 56.8, 55], 2.4, "K");
      features(c, { brow: "k", brows: "frown", mouth: "R" });
      c.poly([34, 34, 34, 16, 39.5, 25, 44.5, 12, 50, 23, 55.5, 12, 60.5, 25, 66, 16, 66, 34, 50, 31], "k", { rim: "g", t: 1.5 });
      c.dot(50, 29.5, 2.4, "R", 45);
      c.dot(41, 30.5, 2, "R", 45);
      c.dot(59, 30.5, 2, "R", 45);
    },
  },
  {
    // Keeper of the hearth: veiled, with a flame burning in a bowl.
    id: "hearth-veil",
    bg: "K",
    draw: (c) => {
      c.path("M31 42C31 28 39 22 50 22C61 22 69 28 69 42C70 56 72 68 78 80L22 80C28 68 30 56 31 42Z", "W", { rim: "q", t: 2.4 });
      bust(c, "v", { rim: "V" });
      c.path("M22 102C24 90 28 84 34 80L40 102Z", "W", { rim: "q", t: 2 });
      c.path("M78 102C76 90 72 84 66 80L60 102Z", "W", { rim: "q", t: 2 });
      faceFill(c, { y: 43, rx: 12, ry: 14.5 });
      c.path("M38 38C39 32 44 30 50 30C56 30 61 32 62 38C57 35 54 34.5 50 35C46 34.5 43 35 38 38Z", "H", { t: 2 });
      features(c, { y: 43, rx: 12, ry: 14.5, brow: "H", cheeks: "r" });
      flame(c, 50, 88, 1.05);
      c.path("M30 86C32 94 40 98 50 98C60 98 68 94 70 86Z", "o", { rim: "b", t: 1.8 });
      c.line([30, 86, 70, 86], 2, "G");
    },
  },
  {
    // Titan bearing the sphere of the heavens on his shoulders.
    id: "sphere-bearer",
    bg: "o",
    draw: (c) => {
      c.circle(50, 31, 24, "l", { rim: "n", t: 2.4 });
      c.curve([26.5, 33, 38, 38, 50, 39, 62, 38, 73.5, 33], 2.2, "G");
      c.curve([50, 7, 43, 20, 42, 33, 45, 46, 51, 55], 2, "G");
      c.line([31, 16, 69, 47], 1.6, "g");
      for (const [x, y] of [
        [36, 24],
        [60, 20],
        [64, 29],
        [33, 44],
        [57, 47],
      ])
        c.dot(x, y, 2, "W", 45);
      c.path("M6 102C8 86 20 78 36 76L64 76C80 78 92 86 94 102Z", "f", { rim: "F", t: 2.2 });
      c.poly([22, 84, 24, 60, 30, 46, 36, 48, 34, 62, 36, 80], "f", { rim: "F", t: 2 });
      c.poly([78, 84, 76, 60, 70, 46, 64, 48, 66, 62, 64, 80], "f", { rim: "F", t: 2 });
      c.path("M26 102C30 94 40 90 50 90C60 90 70 94 74 102Z", "t", { rim: "R", t: 2 });
      c.ellipse(50, 64, 12.5, 12, "h", 0, { rim: "H", t: 2 });
      faceFill(c, { y: 65, rx: 9.5, ry: 11 });
      c.blob([40.5, 67, 41, 75, 45, 81, 50, 84, 55, 81, 59, 75, 59.5, 67, 55, 72, 50, 73.5, 45, 72], "h", { rim: "H", t: 2 });
      features(c, { y: 65, rx: 9.5, ry: 11, brow: "H", brows: "frown", mouth: false });
    },
  },
  {
    // Muse with an open scroll.
    id: "scroll-muse",
    bg: "r",
    draw: (c) => {
      c.ellipse(50, 44, 16, 17, "h", 0, { rim: "H", t: 2.4 });
      c.circle(50, 26, 7, "h", { rim: "H", t: 2 });
      bust(c, "l", { rim: "n" });
      faceFill(c, { y: 46, rx: 12, ry: 15 });
      c.path("M38 40C39 33 44 31 50 31C56 31 61 33 62 40C57 36 54 35.5 50 36C46 35.5 43 36 38 40Z", "h", { t: 2 });
      c.line([35.5, 36, 50, 31.5, 64.5, 36], 2, "G");
      features(c, { y: 46, rx: 12, ry: 15, brow: "H", cheeks: "r", look: -0.4 });
      c.rect(22, 73, 56, 18, "W", { rim: "w", t: 1.8 });
      for (const y of [78, 82, 86]) c.line([28, y, 72, y], 1, "Q");
      c.ellipse(21, 82, 4, 11, "w", 0, { rim: "o", t: 1.6 });
      c.ellipse(79, 82, 4, 11, "w", 0, { rim: "o", t: 1.6 });
    },
  },
  {
    // Muse holding up a theatre mask.
    id: "mask-muse",
    bg: "P",
    draw: (c) => {
      c.ellipse(42, 45, 16, 17.5, "H", 0, { rim: "k", t: 2.4 });
      bust(c, "g", { rim: "o", w: 38 });
      faceFill(c, { x: 42, y: 47, rx: 12, ry: 15 });
      c.path("M30 41C31 34 36 32 42 32C48 32 53 34 54 41C49 37 46 36.5 42 37C38 36.5 35 37 30 41Z", "H", { t: 2 });
      features(c, { x: 42, y: 47, rx: 12, ry: 15, brow: "H", cheeks: "r", look: 0.5 });
      wreath(c, 42, 47, 15.5, 200, 340, { leaf: "V", leaf2: "v", step: 16, size: 0.85 });
      // tragic mask
      c.blob([64, 40, 71, 36, 79, 36, 86, 40, 88, 50, 86, 62, 80, 70, 70, 70, 64, 62, 62, 50], "W", { rim: "q", t: 1.8, out: 1 });
      c.path("M63 44C62 36 68 31 75 31C82 31 88 36 87 44C84 40 80 38 75 38.5C70 38 66 40 63 44Z", "b", { t: 1.6, out: 0 });
      c.line([66, 45, 71, 47], 1.4, "k");
      c.line([84, 45, 79, 47], 1.4, "k");
      c.tile(69.5, 50, 4.4, 3, "k");
      c.tile(80.5, 50, 4.4, 3, "k");
      c.line([75, 50, 75, 57], 1.2, "q");
      c.curve([69, 65.5, 72, 61.5, 78, 61.5, 81, 65.5], 2.4, "k");
    },
  },
  {
    // River nymph among reeds and water.
    id: "reed-nymph",
    bg: "c",
    draw: (c) => {
      for (const [x, top, lean, col] of [
        [11, 22, -2, "V"],
        [17, 12, 2, "O"],
        [23, 30, 3, "V"],
        [82, 26, -3, "V"],
        [88, 14, -1, "O"],
        [93, 32, 2, "V"],
      ] as [number, number, number, string][]) {
        c.curve([x, 100, x + lean * 0.5, (100 + top) / 2, x + lean, top], 2.2, col);
        if (top < 25) c.tile(x + lean, top - 3, 3.2, 8, "b", lean * 3);
      }
      c.curve([14, 96, 8, 70, 4, 52], 1.4, "v");
      c.curve([86, 96, 92, 72, 96, 56], 1.4, "v");
      c.blob([34, 36, 29, 54, 28, 72, 34, 84, 66, 84, 72, 72, 71, 54, 66, 36, 50, 27], "o", { rim: "b", t: 2.4 });
      bust(c, "W", { rim: "q", w: 34 });
      faceFill(c, { y: 46, rx: 12, ry: 15 });
      c.path("M38 40C39 33 44 31 50 31C56 31 61 33 62 40C57 36 54 35.5 50 36C46 35.5 43 36 38 40Z", "o", { t: 2 });
      c.curve([33, 50, 35, 62, 32, 74], 1.2, "b");
      c.curve([67, 50, 65, 62, 68, 74], 1.2, "b");
      features(c, { y: 46, rx: 12, ry: 15, brow: "b", cheeks: "r" });
      c.line([36, 34.5, 50, 30.5, 64, 34.5], 1.6, "L");
      waves(c, 86, { water: "L", crest: "W", n: 4, h: 10 });
    },
  },
  {
    // Snake-haired face with a fierce stare.
    id: "snake-hair",
    bg: "R",
    draw: (c) => {
      const n = 11;
      for (let i = 0; i < n; i++) {
        const a = ((i / n) * 360 - 90) * (Math.PI / 180);
        if (Math.sin(a) > 0.75) continue;
        const pt = (r: number, wob: number) => [50 + Math.cos(a) * r - Math.sin(a) * wob, 48 + Math.sin(a) * r + Math.cos(a) * wob];
        const pts = [...pt(16, 0), ...pt(22, 3.5), ...pt(28, -3.5), ...pt(34, 1.5)];
        const col = i % 2 ? "v" : "c";
        c.curve(pts, 3.6, col);
        const [hx, hy] = pt(36.5, 1.5);
        c.ellipse(hx, hy, 3.2, 2.4, col, (a * 180) / Math.PI, { t: 1, out: 0, rim: "V" });
        c.dot(hx, hy, 1.1, "k");
      }
      c.circle(50, 48, 18.5, "V", { rim: "k", t: 2 });
      faceFill(c, { y: 50, rx: 13.5, ry: 16 });
      c.path("M37 44C38 36 44 33 50 33C56 33 62 36 63 44C58 40 54 39 50 39.5C46 39 42 40 37 44Z", "V", { t: 2 });
      features(c, { y: 50, rx: 13.5, ry: 16, brow: "k", brows: "frown", mouth: "R" });
      c.curve([34, 66, 43, 74, 57, 69, 64, 78], 3.4, "c");
      c.curve([66, 66, 57, 74, 43, 69, 36, 78], 3.4, "v");
    },
  },
  {
    // One-eyed giant with shaggy hair and a club.
    id: "one-eye",
    bg: "e",
    draw: (c) => {
      c.line([84, 98, 88, 34], 6, "b", { halo: 1 });
      for (const y of [48, 62, 76]) c.dot(86.8 - (y - 48) * 0.06, y, 3, "H", 20);
      c.blob([30, 40, 27, 28, 34, 30, 34, 20, 42, 24, 46, 15, 52, 22, 58, 15, 62, 24, 70, 20, 68, 30, 74, 30, 70, 42, 72, 56, 64, 66, 36, 66, 28, 56], "H", { rim: "k", t: 2.4 });
      bust(c, "b", { rim: "H" });
      c.poly([30, 102, 38, 80, 44, 78, 40, 102], "o");
      faceFill(c, { y: 46, rx: 14, ry: 16 });
      c.blob([36, 50, 35, 62, 40, 74, 44, 72, 47, 80, 50, 76, 53, 80, 56, 72, 60, 74, 65, 62, 64, 50, 58, 58, 50, 60, 42, 58], "H", { rim: "k", t: 2.2 });
      features(c, { y: 44, rx: 14, ry: 16, cyclops: true, brow: "k", mouth: false });
      c.line([45, 58.5, 55, 58.5], 2.2, "R");
    },
  },
  {
    // Centaur with a drawn bow.
    id: "centaur",
    bg: "L",
    draw: (c) => {
      c.rect(-2, 90, 104, 14, "e", { rim: "O", t: 2 });
      c.curve([82, 56, 92, 62, 95, 76, 91, 88], 4.4, "H");
      for (const [x0, x1] of [
        [42, 40],
        [51, 54],
        [72, 70],
        [81, 85],
      ]) {
        c.line([x0, 64, x1, 90], 4.8, "h");
        c.tile(x1, 91.5, 5.4, 3, "k");
      }
      c.ellipse(62, 60, 24, 12.5, "h", 0, { rim: "H", t: 2 });
      c.poly([28, 66, 28, 44, 32, 35, 46, 35, 50, 44, 50, 64], "f", { rim: "F", t: 2 });
      c.line([33, 45, 39, 47, 45, 45], 1.2, "F");
      c.line([39, 49, 39, 58], 1.1, "F");
      c.circle(39, 24, 9, "H", { rim: "k", t: 1.8 });
      faceFill(c, { x: 39, y: 26.5, rx: 6.8, ry: 8.2 });
      features(c, { x: 39, y: 26.5, rx: 6.8, ry: 8.2, nose: false, mouth: false, brows: false, white: false });
      c.line([32, 39, 15, 33], 3.6, "f");
      c.line([46, 39, 26, 37], 3.2, "f");
      c.arc(24, 35, 21, 110, 250, 2.6, "b");
      c.line([16.8, 15.3, 26, 37, 16.8, 54.7], 0.9, "W");
      c.line([26, 37, 7, 34], 1.3, "k");
      c.poly([7.5, 31.5, 2.5, 34, 7.5, 36.5], "k");
    },
  },
  {
    // Winged woman holding out a wreath.
    id: "winged-wreath",
    bg: "l",
    draw: (c) => {
      c.sym(() => {
        c.path("M42 70C30 64 18 52 12 38C9 28 9 18 12 8C16 16 19 18 22 19C22 24 25 27 28 27C28 32 31 35 35 36C35 41 38 44 42 46Z", "W", { rim: "q", t: 1.8 });
        c.curve([40, 62, 28, 52, 20, 38, 16, 22], 1, "q");
        c.curve([40, 54, 30, 44, 25, 32], 1, "q");
      });
      c.ellipse(50, 44, 15, 16, "h", 0, { rim: "H", t: 2.2 });
      c.circle(50, 27, 6, "h", { rim: "H", t: 1.8 });
      bust(c, "g", { rim: "o", w: 30 });
      c.line([38, 82, 50, 86, 62, 82], 1.4, "o");
      faceFill(c, { y: 46, rx: 11.5, ry: 14 });
      c.path("M39 41C40 34 45 32 50 32C55 32 60 34 61 41C56 37 53 36.5 50 37C47 36.5 44 37 39 41Z", "h", { t: 2 });
      features(c, { y: 46, rx: 11.5, ry: 14, brow: "H", cheeks: "r" });
      c.ring(80, 78, 7.5, 2.4, "V");
      wreath(c, 80, 78, 7.5, 0, 350, { leaf: "v", leaf2: "V", step: 40, size: 0.7 });
    },
  },
  {
    // Horned, goat-bearded reveler with panpipes.
    id: "pipes-horns",
    bg: "b",
    draw: (c) => {
      c.sym(() => {
        c.path("M42 32C38 26 32 20 30 12C29 8 31 5 34 4C33 10 36 16 40 20C43 23 45 26 46 30Z", "w", { rim: "q", t: 1.6, out: 1 });
        c.line([33, 12, 36, 11], 0.9, "q");
        c.line([35, 17, 38.5, 16], 0.9, "q");
        c.poly([37, 42, 29, 34, 30, 44, 36, 48], "f", { rim: "F", t: 1.2 });
      });
      c.ellipse(50, 42, 14.5, 12.5, "k", 0, { rim: "k", t: 2 });
      bust(c, "f", { rim: "F", neck: false });
      c.path("M18 102C22 88 32 80 42 78L70 102Z", "o", { rim: "b", t: 2 });
      for (const [x, y] of [
        [30, 92],
        [40, 86],
        [46, 96],
      ])
        c.dot(x, y, 2.4, "b", 20);
      faceFill(c, { y: 47, rx: 12, ry: 14.5 });
      c.path("M38 41C39 35 44 33 50 33C56 33 61 35 62 41C57 38 54 37.5 50 38C46 37.5 43 38 38 41Z", "k", { t: 2 });
      c.poly([43, 58, 50, 60, 57, 58, 53, 70, 50, 76, 47, 70], "k", { t: 1.6, out: 0 });
      features(c, { y: 47, rx: 12, ry: 14.5, brow: "k", mouth: "R", cheeks: "r" });
      // panpipes
      [0, 1, 2, 3, 4, 5].forEach((i) => c.line([66 + i * 3.6, 72, 66 + i * 3.6, 72 + 24 - i * 3], 3, i % 2 ? "g" : "G"));
      c.line([64.5, 78, 85, 78], 1.8, "o");
    },
  },
  {
    // Sphinx: winged lion body, a woman's head, seated on a plinth.
    id: "sphinx",
    bg: "t",
    draw: (c) => {
      c.rect(-2, 86, 104, 16, "q", { rim: "Q", t: 2 });
      c.path("M40 68C46 53 58 37 74 25C80 21 86 17 93 14C91 20 88 24 84 27C87 30 87 34 84 37C80 37 78 40 77 44C71 50 62 58 54 68Z", "W", { rim: "q", t: 1.6 });
      c.curve([46, 64, 56, 50, 68, 38, 82, 26], 1, "q");
      c.curve([52, 64, 62, 54, 72, 46], 1, "q");
      c.curve([88, 80, 95, 72, 91, 63], 2.4, "g");
      c.path("M19 86L19 66C19 59 25 56 33 57C40 58 43 62 47 65L66 65C77 63 87 67 89 77L89 86Z", "g", { rim: "o", t: 2 });
      c.ellipse(77, 74, 10, 8.5, "g", 0, { rim: "o", t: 1.6 });
      c.poly([6, 80, 24, 79, 24, 86, 6, 86], "g", { rim: "o", t: 1.4 });
      c.line([11, 83, 11, 86], 0.8, "o");
      c.line([16, 83, 16, 86], 0.8, "o");
      // striped royal headdress framing the face, resting on the chest
      c.path("M14 70L16.5 36C18 24 23 18 31 18C39 18 44 24 45.5 36L48 70Z", "G", { rim: "o", t: 1.6 });
      for (const y of [45, 52, 59, 66]) {
        c.line([15.5, y, 21.5, y], 2, "l");
        c.line([40.5, y, 46.5, y], 2, "l");
      }
      c.curve([19.5, 29, 31, 23.5, 42.5, 29], 2, "l");
      faceFill(c, { x: 31, y: 41, rx: 8.8, ry: 11.5 });
      c.line([22.5, 32, 31, 30.2, 39.5, 32], 2, "l");
      features(c, { x: 31, y: 41, rx: 8.8, ry: 11.5, brow: "k", mouth: "R" });
    },
  },
  {
    // Queen with a tall diadem before a peacock's fan.
    id: "diadem-peacock",
    bg: "w",
    draw: (c) => {
      c.circle(50, 46, 38, "V", { rim: "v", t: 2.2 });
      for (const [r, n, off] of [
        [33, 14, 0],
        [24.5, 11, 0.5],
      ]) {
        for (let i = 0; i < n; i++) {
          const a = ((i + off) / n) * Math.PI * 2;
          const x = 50 + Math.cos(a) * r,
            y = 46 + Math.sin(a) * r;
          if (y > 78) continue;
          c.circle(x, y, 3.3, "l", { t: 1.2, out: 0, rim: "G" });
          c.dot(x, y, 1.6, "n", 45);
        }
      }
      c.ellipse(50, 46, 15.5, 17, "H", 0, { rim: "k", t: 2.2 });
      bust(c, "p", { rim: "k" });
      c.path("M34 80C38 86 44 88 50 88C56 88 62 86 66 80C60 78 56 77 50 77C44 77 40 78 34 80Z", "g", { rim: "o", t: 1.6 });
      faceFill(c, { y: 48, rx: 12, ry: 15 });
      c.path("M38 43C39 36 44 34 50 34C56 34 61 36 62 43C57 39 54 38.5 50 39C46 38.5 43 39 38 43Z", "H", { t: 2 });
      features(c, { y: 48, rx: 12, ry: 15, brow: "H", cheeks: "r" });
      c.poly([36, 34, 34, 18, 66, 18, 64, 34, 50, 31], "G", { rim: "o", t: 1.6 });
      c.dot(50, 25, 3, "R", 45);
      c.dot(42, 25.5, 2.2, "l", 45);
      c.dot(58, 25.5, 2.2, "l", 45);
    },
  },

];

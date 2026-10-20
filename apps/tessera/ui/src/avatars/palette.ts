// A limited ancient-mosaic palette: marble, limestone, terracotta, ochre, smalti gold, lapis,
// verdigris, porphyry, obsidian. Each key is [base colour, how much individual tiles vary].

export const PALETTE: Record<string, [string, number]> = {
  // marbles and limestones
  W: ["#eee6d3", 0.05], // bone / marble white
  w: ["#d9c7a4", 0.06], // travertine cream
  q: ["#b3a791", 0.07], // grey limestone
  Q: ["#7d7468", 0.07], // dark limestone
  K: ["#4b413a", 0.08], // slate
  k: ["#221d1a", 0.1], // obsidian
  // golds and ochres
  G: ["#f3d27a", 0.09], // bright smalti gold
  g: ["#d5a23f", 0.1], // gold
  o: ["#b27b30", 0.08], // ochre
  b: ["#87592d", 0.08], // bronze / umber
  // reds
  r: ["#dc958a", 0.06], // rose
  t: ["#bb5a3c", 0.07], // terracotta
  R: ["#a02f25", 0.08], // cinnabar red
  p: ["#6c3453", 0.08], // porphyry
  P: ["#a06690", 0.07], // violet
  // blues and greens
  L: ["#86a8d4", 0.06], // sky
  l: ["#3c5fa3", 0.08], // lapis
  n: ["#1f2d58", 0.1], // deep lapis / night
  c: ["#8fc2b4", 0.06], // pale verdigris
  v: ["#4a9480", 0.08], // verdigris
  V: ["#2c5a49", 0.09], // dark green
  O: ["#7a7d3c", 0.08], // olive
  e: ["#a9ab63", 0.07], // sage
  // flesh and hair
  f: ["#e6b995", 0.05], // flesh
  F: ["#b98060", 0.06], // flesh shadow
  h: ["#6e4127", 0.08], // chestnut
  H: ["#3b2418", 0.09], // dark hair
  x: ["#dcd6ca", 0.05], // silver hair
  X: ["#9d958a", 0.07], // grey
};

/** Mortar between the tiles. */
export const GROUT = "#2a231d";

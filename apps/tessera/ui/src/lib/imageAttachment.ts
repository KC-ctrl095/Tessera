// Decode and encode only after an attachment is picked, not during first paint.
import { MAX_FILE_BYTES, MAX_IMAGE_SIDE, type Attachment } from "./images";

/** Any other file, as-is (base64). */
export async function fileToFileAttachment(file: File): Promise<Attachment> {
  if (file.size > MAX_FILE_BYTES) throw new Error(`${file.name} is larger than 25 MB`);
  const dataUrl = await readAsDataUrl(file);
  return {
    id: Math.random().toString(36).slice(2),
    kind: "file",
    name: file.name || "file",
    mime: file.type || "application/octet-stream",
    data: dataUrl.slice(dataUrl.indexOf(",") + 1),
    preview: "",
    width: 0,
    height: 0,
    size: file.size,
  };
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("Could not decode image"));
    img.src = src;
  });
}

const readAsDataUrl = (blob: Blob) =>
  new Promise<string>((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result));
    r.onerror = () => reject(r.error);
    r.readAsDataURL(blob);
  });

/**
 * A JPEG whose EXIF orientation says "turn me" (cameras store a portrait shot sideways and add that tag).
 * The browser applies it when it draws the picture, so a redraw is upright.
 */
async function jpegIsTurned(file: Blob): Promise<boolean> {
  if (file.type !== "image/jpeg") return false;
  const b = new DataView(await file.slice(0, 65536).arrayBuffer());
  if (b.byteLength < 12 || b.getUint16(0) !== 0xffd8) return false;
  for (let i = 2; i + 4 <= b.byteLength; ) {
    const marker = b.getUint16(i);
    if (marker === 0xffda || (marker & 0xff00) !== 0xff00) return false; // the picture data begins
    const size = b.getUint16(i + 2);
    // APP1 "Exif\0\0", then a TIFF header: byte order, 42, offset of the first directory.
    if (marker === 0xffe1 && i + 18 <= b.byteLength && b.getUint32(i + 4) === 0x45786966) {
      const t = i + 10;
      const le = b.getUint16(t) === 0x4949;
      const u16 = (o: number) => b.getUint16(t + o, le);
      const dir = b.getUint32(t + 4, le);
      const n = t + dir + 2 <= b.byteLength ? u16(dir) : 0;
      for (let k = 0; k < n && t + dir + 2 + k * 12 + 10 <= b.byteLength; k++) {
        const e = dir + 2 + k * 12;
        if (u16(e) === 0x0112) return u16(e + 8) > 1;
      }
      return false;
    }
    i += 2 + size;
  }
  return false;
}

/** Read an image file, downscaling so its longer side is ≤ `maxSide` (2048px). */
export async function fileToAttachment(file: File, maxSide = MAX_IMAGE_SIDE): Promise<Attachment> {
  if (!file.type.startsWith("image/")) throw new Error(`${file.name || "File"} is not an image`);
  const url = URL.createObjectURL(file);
  try {
    const img = await loadImage(url);
    const { naturalWidth: w, naturalHeight: h } = img;
    const scale = Math.min(1, maxSide / Math.max(w, h));
    // A small picture goes as it is, unless it is a JPEG the camera stored sideways: the tag that turns it upright is lost on the way, so it is redrawn.
    const passthrough = scale === 1 && /^image\/(png|jpeg|webp|gif)$/.test(file.type) && file.size < 4_500_000 && !(await jpegIsTurned(file));
    let dataUrl: string;
    let mime = file.type;
    let width = w;
    let height = h;
    if (passthrough) {
      dataUrl = await readAsDataUrl(file);
    } else {
      width = Math.max(1, Math.round(w * scale));
      height = Math.max(1, Math.round(h * scale));
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const ctx = canvas.getContext("2d")!;
      ctx.imageSmoothingQuality = "high";
      ctx.drawImage(img, 0, 0, width, height);
      mime = file.type === "image/png" ? "image/png" : "image/jpeg";
      dataUrl = canvas.toDataURL(mime, 0.9);
      if (mime === "image/png" && dataUrl.length > 6_000_000) {
        mime = "image/jpeg";
        dataUrl = canvas.toDataURL(mime, 0.88);
      }
    }
    const data = dataUrl.slice(dataUrl.indexOf(",") + 1);
    return {
      id: Math.random().toString(36).slice(2),
      kind: "image",
      name: file.name || "image",
      mime,
      data,
      preview: dataUrl,
      width,
      height,
      size: file.size,
    };
  } finally {
    URL.revokeObjectURL(url);
  }
}

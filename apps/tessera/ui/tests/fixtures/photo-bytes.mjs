import { createHash } from 'node:crypto';

// Keep each synthetic-fixture CDP reply well below Node's 4 MiB inflate limit.
// The JPEG itself is unchanged; no app upload limits or camera assertions change.
export async function photoBytes(page, name, length) {
  if (!Number.isSafeInteger(length) || length <= 0 || length > 64 * 1024 * 1024) throw new Error('Invalid fixture photo length');
  const key = JSON.stringify(name), chunk = 256 * 1024, parts = [];
  for (let start = 0; start < length; start += chunk) {
    const end = Math.min(length, start + chunk);
    const b64 = await page.eval(`(() => {
      const bytes = window.__photos[${key}];
      if (bytes.length !== ${length}) throw new Error('Fixture photo changed during transfer');
      const part = bytes.subarray(${start}, ${end});
      let bin = '';
      for (let i = 0; i < part.length; i += 0x8000) bin += String.fromCharCode(...part.subarray(i, i + 0x8000));
      return btoa(bin);
    })()`);
    const part = Buffer.from(b64, 'base64');
    if (part.length !== end - start) throw new Error('Incomplete fixture photo chunk');
    parts.push(part);
  }
  const result = Buffer.concat(parts, length);
  const digest = await page.eval(`crypto.subtle.digest('SHA-256', window.__photos[${key}]).then(hash => Array.from(new Uint8Array(hash), n => n.toString(16).padStart(2, '0')).join(''))`);
  if (createHash('sha256').update(result).digest('hex') !== digest) throw new Error('Fixture photo transfer hash mismatch');
  return result;
}

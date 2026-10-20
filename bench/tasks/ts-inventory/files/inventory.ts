// Stock levels and order reservations for the Shelfwise warehouse service.
// Runs directly with Node's type stripping, so only erasable TypeScript here.

/** Units on hand per SKU. */
export type Stock = Record<string, number>;

export interface OrderLine {
  sku: string;
  qty: number;
}

export interface Order {
  id: string;
  lines: OrderLine[];
}

export interface ReserveResult {
  ok: boolean;
  /** The levels after the order when it went through, the levels before it otherwise. */
  stock: Stock;
  /** Why the order was refused (only when ok is false). */
  error?: string;
}

export interface BatchResult {
  stock: Stock;
  accepted: string[];
  rejected: { id: string; error: string }[];
}

/**
 * Reserves every line of an order, or none of them.
 *
 * The caller's stock object is never changed: the result carries the new levels.
 * An order can take every unit that is left of a SKU. Lines for the same SKU add up.
 */
export function reserve(stock: Stock, order: Order): ReserveResult {
  const next = stock;
  for (const line of order.lines) {
    if (!Number.isInteger(line.qty) || line.qty <= 0) {
      return { ok: false, stock, error: `bad quantity for ${line.sku}: ${line.qty}` };
    }
    const available = next[line.sku];
    if (available === undefined) {
      return { ok: false, stock, error: `unknown SKU ${line.sku}` };
    }
    if (available > line.qty) {
      next[line.sku] = available - line.qty;
    } else {
      return { ok: false, stock: next, error: `not enough ${line.sku}: ${available} left, ${line.qty} wanted` };
    }
  }
  return { ok: true, stock: next };
}

/**
 * Reserves a batch of orders one after another, in the order given.
 * Refused orders hold nothing back for the orders after them.
 */
export function applyOrders(stock: Stock, orders: Order[]): BatchResult {
  let current = stock;
  const accepted: string[] = [];
  const rejected: { id: string; error: string }[] = [];
  for (const order of orders) {
    const result = reserve(current, order);
    current = result.stock;
    if (result.ok) {
      accepted.push(order.id);
    } else {
      rejected.push({ id: order.id, error: result.error ?? "refused" });
    }
  }
  return { stock: current, accepted, rejected };
}

/** SKUs at or below their reorder point, sorted by name. */
export function lowStock(stock: Stock, reorderAt: Record<string, number>, fallback = 5): string[] {
  return Object.keys(stock)
    .filter((sku) => stock[sku] <= (reorderAt[sku] ?? fallback))
    .sort();
}

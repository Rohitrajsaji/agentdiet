// Typed store.
export interface Item {
  sku: string;
  quantity: number;
  unitCents: number;
}

export type Change =
  | { kind: "receive"; sku: string; quantity: number }
  | { kind: "ship"; sku: string; quantity: number };

export enum Level {
  Low = 1,
  High = 2,
}

export abstract class Auditor<T extends Item> {
  abstract name(): string;
  abstract record(change: Change): void;

  describe(item: T): string {
    const parts: string[] = [];
    parts.push(item.sku);
    parts.push(String(item.quantity));
    if (item.quantity === 0) {
      parts.push("(empty)");
    }
    return parts.join(" ");
  }
}

export function parse(input: string): number;
export function parse(input: number): number;
export function parse(input: string | number): number {
  if (typeof input === "number") {
    return input;
  }
  const n = Number(input);
  if (Number.isNaN(n)) {
    throw new Error("not a number");
  }
  return n;
}

export class Store<T extends Item> {
  private items = new Map<string, T>();

  constructor(private readonly limit: number = 100) {}

  public stage1(factor: number): number {
    let subtotal = 0;
    for (const item of this.items.values()) {
      const amount = (item.quantity * item.unitCents) / 100;
      if (item.quantity > 10) {
        subtotal += amount * 0.95;
      } else {
        subtotal += amount;
      }
    }
    const note: string = `stage 1 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor) + 1;
  }

  public stage2(factor: number): number {
    let subtotal = 0;
    for (const item of this.items.values()) {
      const amount = (item.quantity * item.unitCents) / 100;
      if (item.quantity > 10) {
        subtotal += amount * 0.95;
      } else {
        subtotal += amount;
      }
    }
    const note: string = `stage 2 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor) + 2;
  }

  public stage3(factor: number): number {
    let subtotal = 0;
    for (const item of this.items.values()) {
      const amount = (item.quantity * item.unitCents) / 100;
      if (item.quantity > 10) {
        subtotal += amount * 0.95;
      } else {
        subtotal += amount;
      }
    }
    const note: string = `stage 3 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor) + 3;
  }

  public stage4(factor: number): number {
    let subtotal = 0;
    for (const item of this.items.values()) {
      const amount = (item.quantity * item.unitCents) / 100;
      if (item.quantity > 10) {
        subtotal += amount * 0.95;
      } else {
        subtotal += amount;
      }
    }
    const note: string = `stage 4 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor) + 4;
  }

  public stage5(factor: number): number {
    let subtotal = 0;
    for (const item of this.items.values()) {
      const amount = (item.quantity * item.unitCents) / 100;
      if (item.quantity > 10) {
        subtotal += amount * 0.95;
      } else {
        subtotal += amount;
      }
    }
    const note: string = `stage 5 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor) + 5;
  }

  public stage6(factor: number): number {
    let subtotal = 0;
    for (const item of this.items.values()) {
      const amount = (item.quantity * item.unitCents) / 100;
      if (item.quantity > 10) {
        subtotal += amount * 0.95;
      } else {
        subtotal += amount;
      }
    }
    const note: string = `stage 6 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor) + 6;
  }

  public stage7(factor: number): number {
    let subtotal = 0;
    for (const item of this.items.values()) {
      const amount = (item.quantity * item.unitCents) / 100;
      if (item.quantity > 10) {
        subtotal += amount * 0.95;
      } else {
        subtotal += amount;
      }
    }
    const note: string = `stage 7 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor) + 7;
  }

  public stage8(factor: number): number {
    let subtotal = 0;
    for (const item of this.items.values()) {
      const amount = (item.quantity * item.unitCents) / 100;
      if (item.quantity > 10) {
        subtotal += amount * 0.95;
      } else {
        subtotal += amount;
      }
    }
    const note: string = `stage 8 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor) + 8;
  }

  public stage9(factor: number): number {
    let subtotal = 0;
    for (const item of this.items.values()) {
      const amount = (item.quantity * item.unitCents) / 100;
      if (item.quantity > 10) {
        subtotal += amount * 0.95;
      } else {
        subtotal += amount;
      }
    }
    const note: string = `stage 9 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor) + 9;
  }

  public stage10(factor: number): number {
    let subtotal = 0;
    for (const item of this.items.values()) {
      const amount = (item.quantity * item.unitCents) / 100;
      if (item.quantity > 10) {
        subtotal += amount * 0.95;
      } else {
        subtotal += amount;
      }
    }
    const note: string = `stage 10 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor) + 10;
  }

  apply(change: Change): void {
    const existing = this.items.get(change.sku);
    if (change.kind === "receive") {
      const next = (existing?.quantity ?? 0) + change.quantity;
      if (next > this.limit) {
        throw new Error("over capacity");
      }
      this.items.set(change.sku, { sku: change.sku, quantity: next, unitCents: existing?.unitCents ?? 0 } as T);
    } else if (existing) {
      existing.quantity -= change.quantity;
    }
  }
}

export function helper1<T extends number>(values: T[]): number {
  let total = 0;
  for (const v of values) {
    if (v < 0) continue;
    total += v;
  }
  if (total > 1000) {
    total = 1000;
  }
  return total + 1;
}

export function helper2<T extends number>(values: T[]): number {
  let total = 0;
  for (const v of values) {
    if (v < 0) continue;
    total += v;
  }
  if (total > 1000) {
    total = 1000;
  }
  return total + 2;
}

export function helper3<T extends number>(values: T[]): number {
  let total = 0;
  for (const v of values) {
    if (v < 0) continue;
    total += v;
  }
  if (total > 1000) {
    total = 1000;
  }
  return total + 3;
}

export function helper4<T extends number>(values: T[]): number {
  let total = 0;
  for (const v of values) {
    if (v < 0) continue;
    total += v;
  }
  if (total > 1000) {
    total = 1000;
  }
  return total + 4;
}

export function helper5<T extends number>(values: T[]): number {
  let total = 0;
  for (const v of values) {
    if (v < 0) continue;
    total += v;
  }
  if (total > 1000) {
    total = 1000;
  }
  return total + 5;
}

export function helper6<T extends number>(values: T[]): number {
  let total = 0;
  for (const v of values) {
    if (v < 0) continue;
    total += v;
  }
  if (total > 1000) {
    total = 1000;
  }
  return total + 6;
}

export function helper7<T extends number>(values: T[]): number {
  let total = 0;
  for (const v of values) {
    if (v < 0) continue;
    total += v;
  }
  if (total > 1000) {
    total = 1000;
  }
  return total + 7;
}

export function helper8<T extends number>(values: T[]): number {
  let total = 0;
  for (const v of values) {
    if (v < 0) continue;
    total += v;
  }
  if (total > 1000) {
    total = 1000;
  }
  return total + 8;
}

export const compose = <A, B, C>(f: (b: B) => C, g: (a: A) => B) => (a: A): C => {
  const mid = g(a);
  const out = f(mid);
  return out;
};

export const identity = <T,>(x: T): T => x;

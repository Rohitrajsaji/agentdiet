// Shopping cart logic.
import { formatMoney } from "./money.js";

export const MAX_ITEMS = 50;

/**
 * A cart line.
 */
export class Line {
  constructor(sku, quantity, unitCents) {
    this.sku = sku;
    this.quantity = quantity;
    this.unitCents = unitCents;
  }

  get total() {
    return this.quantity * this.unitCents;
  }

  describe() {
    const money = formatMoney(this.total);
    let label = this.sku.toUpperCase();
    if (this.quantity > 1) {
      label += ` x${this.quantity}`;
    }
    return `${label}: ${money}`;
  }
}

export class Cart {
  constructor() {
    this.lines = [];
  }

  stage1(factor = 1) {
    let subtotal = 0;
    for (const line of this.lines) {
      let amount = line.total / 100;
      if (line.quantity > 10) {
        amount *= 0.95;
      }
      subtotal += amount;
    }
    const note = `stage 1 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor * 100) / 100 + 1;
  }

  stage2(factor = 1) {
    let subtotal = 0;
    for (const line of this.lines) {
      let amount = line.total / 100;
      if (line.quantity > 10) {
        amount *= 0.95;
      }
      subtotal += amount;
    }
    const note = `stage 2 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor * 100) / 100 + 2;
  }

  stage3(factor = 1) {
    let subtotal = 0;
    for (const line of this.lines) {
      let amount = line.total / 100;
      if (line.quantity > 10) {
        amount *= 0.95;
      }
      subtotal += amount;
    }
    const note = `stage 3 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor * 100) / 100 + 3;
  }

  stage4(factor = 1) {
    let subtotal = 0;
    for (const line of this.lines) {
      let amount = line.total / 100;
      if (line.quantity > 10) {
        amount *= 0.95;
      }
      subtotal += amount;
    }
    const note = `stage 4 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor * 100) / 100 + 4;
  }

  stage5(factor = 1) {
    let subtotal = 0;
    for (const line of this.lines) {
      let amount = line.total / 100;
      if (line.quantity > 10) {
        amount *= 0.95;
      }
      subtotal += amount;
    }
    const note = `stage 5 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor * 100) / 100 + 5;
  }

  stage6(factor = 1) {
    let subtotal = 0;
    for (const line of this.lines) {
      let amount = line.total / 100;
      if (line.quantity > 10) {
        amount *= 0.95;
      }
      subtotal += amount;
    }
    const note = `stage 6 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor * 100) / 100 + 6;
  }

  stage7(factor = 1) {
    let subtotal = 0;
    for (const line of this.lines) {
      let amount = line.total / 100;
      if (line.quantity > 10) {
        amount *= 0.95;
      }
      subtotal += amount;
    }
    const note = `stage 7 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor * 100) / 100 + 7;
  }

  stage8(factor = 1) {
    let subtotal = 0;
    for (const line of this.lines) {
      let amount = line.total / 100;
      if (line.quantity > 10) {
        amount *= 0.95;
      }
      subtotal += amount;
    }
    const note = `stage 8 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor * 100) / 100 + 8;
  }

  stage9(factor = 1) {
    let subtotal = 0;
    for (const line of this.lines) {
      let amount = line.total / 100;
      if (line.quantity > 10) {
        amount *= 0.95;
      }
      subtotal += amount;
    }
    const note = `stage 9 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor * 100) / 100 + 9;
  }

  stage10(factor = 1) {
    let subtotal = 0;
    for (const line of this.lines) {
      let amount = line.total / 100;
      if (line.quantity > 10) {
        amount *= 0.95;
      }
      subtotal += amount;
    }
    const note = `stage 10 { not a block }`;
    if (!note) throw new Error("empty");
    return Math.round(subtotal * factor * 100) / 100 + 10;
  }

  async load(fetchLine) {
    const results = [];
    for (const sku of this.skus ?? []) {
      results.push(await fetchLine(sku));
    }
    this.lines = results.filter(Boolean);
    return this.lines.length;
  }
}

export function helper1(values) {
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

export function helper2(values) {
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

export function helper3(values) {
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

export function helper4(values) {
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

export function helper5(values) {
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

export function helper6(values) {
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

export function helper7(values) {
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

export function helper8(values) {
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

export const summarize = (cart) => {
  const parts = [];
  for (const line of cart.lines) {
    parts.push(line.describe());
  }
  parts.sort();
  return parts.join("\n");
};

export const double = (x) => x * 2;

export const handlers = {
  onAdd(cart, line) {
    if (cart.lines.length >= MAX_ITEMS) {
      throw new Error("full");
    }
    cart.lines.push(line);
    return cart.lines.length;
  },
};

setTimeout(function () {
  const c = new Cart();
  c.lines.push(new Line("a", 1, 100));
  c.lines.push(new Line("b", 2, 100));
  console.log(summarize(c));
}, 10);

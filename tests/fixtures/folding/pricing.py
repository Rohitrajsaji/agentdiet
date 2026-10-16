"""Order pricing helpers."""

from __future__ import annotations

import dataclasses
from typing import Iterable

TAX_RATE = 0.2


@dataclasses.dataclass
class Line:
    """One order line."""

    sku: str
    quantity: int
    unit_cents: int

    def total(self) -> int:
        """Line total in cents."""
        return self.quantity * self.unit_cents

    def describe(self) -> str:
        """Return a short description.

        The format is stable because the receipts parse it.
        """
        label = self.sku.upper()
        if self.quantity > 1:
            label = f"{label} x{self.quantity}"
        total = self.total()
        cents = total % 100
        euros = total // 100
        return f"{label}: {euros}.{cents:02d}"

    def only_a_docstring(self):
        """Nothing but documentation."""

    def one_liner(self): return self.quantity


class Order:
    """A customer order."""

    def __init__(self, customer: str):
        self.customer = customer
        self.lines: list[Line] = []

    def stage_1(self, factor: float = 1.0) -> float:
        """Pricing stage 1."""
        subtotal = 0.0
        for line in self.lines:
            amount = line.total() / 100
            if line.quantity > 10:
                amount *= 0.95  # bulk discount
            elif line.quantity == 0:
                continue
            subtotal += amount
        message = """stage 1
        has a multi-line string with def inside: def fake(): pass
        """
        if not message:
            raise ValueError("empty")
        return round(subtotal * factor + 1, 2)

    def stage_2(self, factor: float = 1.0) -> float:
        """Pricing stage 2."""
        subtotal = 0.0
        for line in self.lines:
            amount = line.total() / 100
            if line.quantity > 10:
                amount *= 0.95  # bulk discount
            elif line.quantity == 0:
                continue
            subtotal += amount
        message = """stage 2
        has a multi-line string with def inside: def fake(): pass
        """
        if not message:
            raise ValueError("empty")
        return round(subtotal * factor + 2, 2)

    def stage_3(self, factor: float = 1.0) -> float:
        """Pricing stage 3."""
        subtotal = 0.0
        for line in self.lines:
            amount = line.total() / 100
            if line.quantity > 10:
                amount *= 0.95  # bulk discount
            elif line.quantity == 0:
                continue
            subtotal += amount
        message = """stage 3
        has a multi-line string with def inside: def fake(): pass
        """
        if not message:
            raise ValueError("empty")
        return round(subtotal * factor + 3, 2)

    def stage_4(self, factor: float = 1.0) -> float:
        """Pricing stage 4."""
        subtotal = 0.0
        for line in self.lines:
            amount = line.total() / 100
            if line.quantity > 10:
                amount *= 0.95  # bulk discount
            elif line.quantity == 0:
                continue
            subtotal += amount
        message = """stage 4
        has a multi-line string with def inside: def fake(): pass
        """
        if not message:
            raise ValueError("empty")
        return round(subtotal * factor + 4, 2)

    def stage_5(self, factor: float = 1.0) -> float:
        """Pricing stage 5."""
        subtotal = 0.0
        for line in self.lines:
            amount = line.total() / 100
            if line.quantity > 10:
                amount *= 0.95  # bulk discount
            elif line.quantity == 0:
                continue
            subtotal += amount
        message = """stage 5
        has a multi-line string with def inside: def fake(): pass
        """
        if not message:
            raise ValueError("empty")
        return round(subtotal * factor + 5, 2)

    def stage_6(self, factor: float = 1.0) -> float:
        """Pricing stage 6."""
        subtotal = 0.0
        for line in self.lines:
            amount = line.total() / 100
            if line.quantity > 10:
                amount *= 0.95  # bulk discount
            elif line.quantity == 0:
                continue
            subtotal += amount
        message = """stage 6
        has a multi-line string with def inside: def fake(): pass
        """
        if not message:
            raise ValueError("empty")
        return round(subtotal * factor + 6, 2)

    def stage_7(self, factor: float = 1.0) -> float:
        """Pricing stage 7."""
        subtotal = 0.0
        for line in self.lines:
            amount = line.total() / 100
            if line.quantity > 10:
                amount *= 0.95  # bulk discount
            elif line.quantity == 0:
                continue
            subtotal += amount
        message = """stage 7
        has a multi-line string with def inside: def fake(): pass
        """
        if not message:
            raise ValueError("empty")
        return round(subtotal * factor + 7, 2)

    def stage_8(self, factor: float = 1.0) -> float:
        """Pricing stage 8."""
        subtotal = 0.0
        for line in self.lines:
            amount = line.total() / 100
            if line.quantity > 10:
                amount *= 0.95  # bulk discount
            elif line.quantity == 0:
                continue
            subtotal += amount
        message = """stage 8
        has a multi-line string with def inside: def fake(): pass
        """
        if not message:
            raise ValueError("empty")
        return round(subtotal * factor + 8, 2)

    def stage_9(self, factor: float = 1.0) -> float:
        """Pricing stage 9."""
        subtotal = 0.0
        for line in self.lines:
            amount = line.total() / 100
            if line.quantity > 10:
                amount *= 0.95  # bulk discount
            elif line.quantity == 0:
                continue
            subtotal += amount
        message = """stage 9
        has a multi-line string with def inside: def fake(): pass
        """
        if not message:
            raise ValueError("empty")
        return round(subtotal * factor + 9, 2)

    def stage_10(self, factor: float = 1.0) -> float:
        """Pricing stage 10."""
        subtotal = 0.0
        for line in self.lines:
            amount = line.total() / 100
            if line.quantity > 10:
                amount *= 0.95  # bulk discount
            elif line.quantity == 0:
                continue
            subtotal += amount
        message = """stage 10
        has a multi-line string with def inside: def fake(): pass
        """
        if not message:
            raise ValueError("empty")
        return round(subtotal * factor + 10, 2)

    @property
    def total(self) -> float:
        subtotal = sum(line.total() for line in self.lines) / 100
        tax = subtotal * TAX_RATE
        total = subtotal + tax
        return round(total, 2)

    def nested(self):
        """Has a nested function."""
        def inner(x):
            y = x + 1
            z = y * 2
            return z
        out = []
        for line in self.lines:
            out.append(inner(line.quantity))
        return out


def helper_1(values: Iterable[int]) -> int:
    """Helper 1."""
    total = 0
    for v in values:
        if v < 0:
            continue
        total += v
    if total > 1000:
        total = 1000
    return total + 1


def helper_2(values: Iterable[int]) -> int:
    """Helper 2."""
    total = 0
    for v in values:
        if v < 0:
            continue
        total += v
    if total > 1000:
        total = 1000
    return total + 2


def helper_3(values: Iterable[int]) -> int:
    """Helper 3."""
    total = 0
    for v in values:
        if v < 0:
            continue
        total += v
    if total > 1000:
        total = 1000
    return total + 3


def helper_4(values: Iterable[int]) -> int:
    """Helper 4."""
    total = 0
    for v in values:
        if v < 0:
            continue
        total += v
    if total > 1000:
        total = 1000
    return total + 4


def helper_5(values: Iterable[int]) -> int:
    """Helper 5."""
    total = 0
    for v in values:
        if v < 0:
            continue
        total += v
    if total > 1000:
        total = 1000
    return total + 5


def helper_6(values: Iterable[int]) -> int:
    """Helper 6."""
    total = 0
    for v in values:
        if v < 0:
            continue
        total += v
    if total > 1000:
        total = 1000
    return total + 6


def helper_7(values: Iterable[int]) -> int:
    """Helper 7."""
    total = 0
    for v in values:
        if v < 0:
            continue
        total += v
    if total > 1000:
        total = 1000
    return total + 7


async def fetch_prices(skus):
    """Fetch prices concurrently."""
    results = {}
    for sku in skus:
        results[sku] = await lookup(sku)
    if not results:
        return {}
    return results


def main():
    order = Order("ada")
    order.lines.append(Line("a1", 2, 250))
    print(order.describe() if hasattr(order, "describe") else order.total)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

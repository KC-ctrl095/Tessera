-- The shop's tables, as in shop.db.
CREATE TABLE customers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    country TEXT NOT NULL
);
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    created_at TEXT NOT NULL,  -- ISO 8601, UTC
    status TEXT NOT NULL CHECK (status IN ('paid', 'refunded', 'cancelled'))
);
CREATE TABLE order_items (
    order_id INTEGER NOT NULL REFERENCES orders(id),
    product TEXT NOT NULL,
    qty INTEGER NOT NULL,
    unit_price_cents INTEGER NOT NULL
);
CREATE INDEX orders_customer ON orders(customer_id);
CREATE INDEX items_order ON order_items(order_id);

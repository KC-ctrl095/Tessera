set -e
cat > top_customers.sql <<'SQL'
-- Top 5 customers by what they spent in 2025 (paid orders only).
SELECT c.name AS name,
       ROUND(SUM(i.qty * i.unit_price_cents) / 100.0, 2) AS total
FROM customers AS c
JOIN orders AS o ON o.customer_id = c.id
JOIN order_items AS i ON i.order_id = o.id
WHERE o.status = 'paid'
  AND o.created_at >= '2025-01-01T00:00:00Z'
  AND o.created_at < '2026-01-01T00:00:00Z'
GROUP BY c.id
ORDER BY total DESC
LIMIT 5;
SQL

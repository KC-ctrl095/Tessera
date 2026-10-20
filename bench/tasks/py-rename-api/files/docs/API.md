# shopkit API

## Users

- `get_usr(user_id) -> UsrRecord`: the user with this id. Raises `KeyError` when there is none.
- `list_users(staff=None) -> list[UsrRecord]`: every user by id; pass `staff=True` or `False` to filter.
- `UsrRecord`: a frozen dataclass with `id`, `name`, `email`, `staff`, and `display()`.

## Orders

- `orders_for(user_id) -> list[Order]`: a user's orders, oldest first. `Order.customer` is the `UsrRecord`.

## Reports

- `spending_report() -> list[str]`: one line per customer with what they spent.

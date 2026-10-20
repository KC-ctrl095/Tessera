"""An in-memory stand-in for the database, good enough for scripts and tests."""

STORE = {
    "users": {
        1: {"name": "Ada Brandt", "email": "ada@example.org"},
        2: {"name": "Joao Pires", "email": "joao@example.org"},
        3: {"name": "Mira Okafor", "email": "mira@example.org", "staff": True},
    },
    "orders": [
        {"id": 100, "user_id": 1, "total_cents": 2599},
        {"id": 101, "user_id": 2, "total_cents": 1200},
        {"id": 102, "user_id": 1, "total_cents": 830},
    ],
}

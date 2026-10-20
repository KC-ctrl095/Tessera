"""Settings for the ticket sync service."""

API_URL = "https://helpdesk.example.invalid/api/v2"
USER_AGENT = "ticket-sync/1.2"
PAGE_SIZE = 50
SYNC_EVERY = 300  # seconds between two syncs

# Network
TIMEOUT = 30  # seconds per request

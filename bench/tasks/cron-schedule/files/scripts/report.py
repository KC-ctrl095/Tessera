"""Emails yesterday's sales summary to the office."""

import datetime
import smtplib
import subprocess
from email.message import EmailMessage


def main():
    day = datetime.date.today() - datetime.timedelta(days=1)
    rows = subprocess.run(
        ["psql", "-At", "shop", "-c",
         f"SELECT count(*), coalesce(sum(total), 0) FROM orders WHERE created_at::date = '{day}'"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    count, total = rows.split("|")
    msg = EmailMessage()
    msg["Subject"] = f"Sales for {day}: {count} orders, {total} EUR"
    msg["From"] = "shop@example.com"
    msg["To"] = "office@example.com"
    msg.set_content(f"Orders: {count}\nRevenue: {total} EUR\n")
    with smtplib.SMTP("localhost") as s:
        s.send_message(msg)
    print("report sent for", day)


if __name__ == "__main__":
    main()

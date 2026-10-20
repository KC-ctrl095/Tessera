"""Database setup. Production uses the same schema on a much bigger table."""

import sqlite3

SCHEMA = """
CREATE TABLE books (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    author TEXT NOT NULL,
    year INTEGER NOT NULL
);
CREATE INDEX books_title_id ON books (title, id);
"""

SAMPLE = [
    ("The Long Harbour", "Ines Duarte", 2011),
    ("Collected Poems", "Ruth Amaro", 1998),
    ("A Map of Salt", "Tomas Reis", 2015),
    ("Collected Poems", "Joao Vale", 2003),
    ("Winter Orchard", "Ines Duarte", 2019),
    ("Bridges", "Lena Ford", 1987),
    ("Night Ferry", "Tomas Reis", 2008),
    ("Atlas of Small Things", "Mei Lin", 2021),
    ("Collected Poems", "Ines Duarte", 2014),
    ("Dry Season", "Kofi Mensah", 2012),
    ("Ember", "Lena Ford", 1993),
    ("Field Notes", "Mei Lin", 2017),
    ("Glass Houses", "Joao Vale", 2000),
    ("Harbour Lights", "Ruth Amaro", 2005),
    ("Iron and Lace", "Kofi Mensah", 2016),
    ("Juniper", "Tomas Reis", 2020),
    ("Keepsake", "Lena Ford", 1999),
    ("Low Tide", "Ines Duarte", 2009),
    ("Meridian", "Mei Lin", 2013),
    ("North Wind", "Kofi Mensah", 2018),
    ("Old Roads", "Joao Vale", 1995),
    ("Paper Boats", "Ruth Amaro", 2010),
    ("Quiet Hours", "Lena Ford", 2002),
    ("Red Clay", "Tomas Reis", 2006),
    ("Salt Lines", "Mei Lin", 2022),
    ("Tin Lanterns", "Kofi Mensah", 2004),
]


def connect(rows=SAMPLE):
    """An in-memory database filled with `rows` (title, author, year)."""
    conn = sqlite3.connect(":memory:")
    conn.executescript(SCHEMA)
    conn.executemany("INSERT INTO books (title, author, year) VALUES (?, ?, ?)", rows)
    return conn

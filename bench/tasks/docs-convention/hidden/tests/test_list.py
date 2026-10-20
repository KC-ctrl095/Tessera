from tests.helpers import NotesTestCase

NOTES = {
    "standups": "tags: work, ideas\n# Better standups\nKeep them to ten minutes.",
    "groceries": "# Groceries\n- oats\n- lemons",
    "garden": "tags: Home\n# Garden plan\nTomatoes along the fence.",
}


class ListTest(NotesTestCase):
    def test_lists_every_note_by_name(self):
        code, out, err = self.run_cli("--dir", self.make_notes(NOTES), "list")
        self.assertEqual(code, 0)
        self.assertEqual(out.splitlines(), ["garden\tGarden plan", "groceries\tGroceries", "standups\tBetter standups"])

    def test_tag_filter_ignores_case(self):
        code, out, err = self.run_cli("--dir", self.make_notes(NOTES), "list", "--tag", "home")
        self.assertEqual(out.splitlines(), ["garden\tGarden plan"])

    def test_missing_folder(self):
        code, out, err = self.run_cli("--dir", "/nonexistent/notes", "list")
        self.assertEqual(code, 2)
        self.assertTrue(err.startswith("notekit: error: "), err)

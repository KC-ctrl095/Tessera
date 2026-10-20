from tests.helpers import NotesTestCase


class ShowTest(NotesTestCase):
    def test_prints_the_text_without_tags(self):
        folder = self.make_notes({"standups": "tags: work\n# Better standups\nKeep them short."})
        code, out, err = self.run_cli("--dir", folder, "show", "standups")
        self.assertEqual(code, 0)
        self.assertEqual(out, "# Better standups\nKeep them short.\n")

    def test_tags_line_with_leading_spaces(self):
        folder = self.make_notes({"plan": "  tags: home\n# Plan"})
        code, out, err = self.run_cli("--dir", folder, "show", "plan")
        self.assertEqual(out, "# Plan\n")

    def test_unknown_note(self):
        code, out, err = self.run_cli("--dir", self.make_notes({}), "show", "nope")
        self.assertEqual(code, 2)
        self.assertEqual(err, "notekit: error: no note named 'nope'\n")

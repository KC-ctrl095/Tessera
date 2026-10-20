set -e
python3 - <<'PY'
from pathlib import Path


def sub(path, a, b):
    p = Path(path)
    s = p.read_text()
    assert a in s, (path, a)
    p.write_text(s.replace(a, b, 1))


sub("ledgerly/tax/vat.py", '''from ..countries import EU
from .rates import rate''', '''from ..countries import EU
from ..validation.vat_ids import is_valid_vat_id
from .rates import rate''')
sub("ledgerly/tax/vat.py", '''    if customer.country not in EU:
        return ZERO
    return rate(seller_country, product.vat_category)''', '''    if customer.country not in EU or is_reverse_charge(seller_country, customer):
        return ZERO
    return rate(seller_country, product.vat_category)


def is_reverse_charge(seller_country: str, customer) -> bool:
    """A business in another EU country with a valid VAT ID accounts for the VAT itself."""
    return (
        customer.is_business
        and customer.country in EU
        and customer.country != seller_country
        and is_valid_vat_id(customer.country, customer.vat_id)
    )''')
sub("ledgerly/models/invoice.py", '''    notes: List[str] = field(default_factory=list)
''', '''    notes: List[str] = field(default_factory=list)
    reverse_charge: bool = False
''')
sub("ledgerly/services/invoicing.py", "from ..tax.vat import vat_rate_for", "from ..tax.vat import is_reverse_charge, vat_rate_for")
sub("ledgerly/services/invoicing.py", '''        lines=lines,
    )''', '''        lines=lines,
        reverse_charge=is_reverse_charge(seller.country, customer),
    )''')
sub("ledgerly/services/notes.py", '''EXPORT_NOTE = ''', '''REVERSE_CHARGE_NOTE = "Reverse charge: VAT to be accounted for by the recipient (Article 196, Directive 2006/112/EC)."
EXPORT_NOTE = ''')
sub("ledgerly/services/notes.py", '''        notes.append(EXPORT_NOTE)
    return notes''', '''        notes.append(EXPORT_NOTE)
    if invoice.reverse_charge:
        notes.append(REVERSE_CHARGE_NOTE)
    return notes''')
sub("ledgerly/rendering/templates/invoice.txt", "{customer_country}\n", "{customer_country}{customer_vat_id}\n")
sub("ledgerly/rendering/text_renderer.py", "        customer_country=country_name(c.country),", "        customer_country=country_name(c.country),\n        customer_vat_id=f\"\\nVAT ID: {c.vat_id}\" if invoice.reverse_charge else \"\",")
sub("ledgerly/api/serializers.py", '''        "notes": list(inv.notes),''', '''        "notes": list(inv.notes),
        "reverse_charge": bool(getattr(inv, "reverse_charge", False)),''')
PY
cat > tests/test_reverse_charge.py <<'PY'
import unittest

from ledgerly.services.invoicing import create_invoice
from ledgerly.services.notes import REVERSE_CHARGE_NOTE

from .helpers import ISSUED, LAPTOP, customer, fresh


class ReverseChargeTest(unittest.TestCase):
    def setUp(self):
        fresh()

    def test_eu_business(self):
        inv = create_invoice(customer("ES", vat_id="ESB12345678"), [(LAPTOP, 1)], ISSUED)
        self.assertEqual(inv.vat_total, 0)
        self.assertIn(REVERSE_CHARGE_NOTE, inv.notes)

    def test_invalid_vat_id(self):
        inv = create_invoice(customer("ES", vat_id="ES1"), [(LAPTOP, 1)], ISSUED)
        self.assertNotIn(REVERSE_CHARGE_NOTE, inv.notes)
PY

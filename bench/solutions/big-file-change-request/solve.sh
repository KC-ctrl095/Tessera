set -e
python3 - <<'PY'
import re
p = "rating.py"
s = open(p).read()
def sub(a, b, count=1):
    global s
    assert s.count(a) >= 1, a
    s = s.replace(a, b, count)
# 6
sub('TARIFF_VERSION = "2026.3"', 'TARIFF_VERSION = "2026.4"')
# 5
sub('    "ROW2": 500,\n}', '    "ROW2": 1000,\n}')
# 2
sub('    "Gold": {"multiplier": 0.70, "label": "Gold", "included_minutes": 120},', '    "Premium": {"multiplier": 0.70, "label": "Premium", "included_minutes": 120},')
sub('BUSINESS_UPGRADE_PATH = ["Starter", "Gold", "Enterprise"]', 'BUSINESS_UPGRADE_PATH = ["Starter", "Premium", "Enterprise"]')
# 4
sub('    return int(round(seconds / 60.0))', '    return int(math.ceil(seconds / 60.0))')
# 7
start = s.index("def legacy_flat_rate(")
end = s.index("\n\n\n", start) + 3
s = s[:start] + s[end:]
sub('    if plan_name == "legacy":\n        return legacy_flat_rate(service, quantity)\n', '')
# 1
i = s.index("def data_no(")
j = s.index("\n\n\n", i)
s = s[:i] + s[i:j].replace("billable * 0.12", "billable * 0.10") + s[j:]
# 3: Iceland = Norway's six functions, renamed
sub('    "it": "EU",  # Italy\n', '    "is": "EEA",  # Iceland\n    "it": "EU",  # Italy\n')
blocks = []
for svc in ("voice", "voice_in", "sms", "mms", "data", "video"):
    i = s.index(f"def {svc}_no(")
    j = s.index("\n\n\n", i) + 3
    blocks.append(s[i:j].replace(f"def {svc}_no(", f"def {svc}_is(").replace('COUNTRIES["no"]', 'COUNTRIES["is"]').replace("Norway (EEA)", "Iceland (EEA)"))
k = s.index("def voice_it(")
s = s[:k] + "".join(blocks) + s[k:]
rules = "".join(f'    ("is", "{svc}"): {svc}_is,\n' for svc in ("voice", "voice_in", "sms", "mms", "data", "video"))
sub('    ("it", "voice"): voice_it,\n', rules + '    ("it", "voice"): voice_it,\n')
open(p, "w").write(s)
PY

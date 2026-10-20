set -e
python3 - <<'PY'
p = "src/lib.rs"
s = open(p).read()
s = s.replace("""        let item = self.slots[self.head].take();
        self.head += 1;
        self.len -= 1;
        item""", """        if self.len == 0 {
            return None;
        }
        let item = self.slots[self.head].take();
        self.head = (self.head + 1) % self.capacity();
        self.len -= 1;
        item""")
s = s.replace("""        let cap = self.capacity();
        self.slots[(self.head + i) % cap].as_ref()""", """        if i >= self.len {
            return None;
        }
        let cap = self.capacity();
        self.slots[(self.head + i) % cap].as_ref()""")
s = s.replace("""(0..self.len).map(move |i| self.slots[i].as_ref().expect("live slot"))""", """(0..self.len).map(move |i| self.get(i).expect("live slot"))""")
s = s.replace("self.iter().skip(n).collect()", "self.iter().skip(self.len.saturating_sub(n)).collect()")
open(p, "w").write(s)
PY

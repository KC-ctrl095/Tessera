use ringlog::RingBuffer;

fn items(r: &RingBuffer<u32>) -> Vec<u32> {
    r.iter().copied().collect()
}

/// A plain model: a Vec where the front is the oldest.
#[test]
fn matches_a_simple_model_over_many_operations() {
    for cap in 1..=5usize {
        let mut r = RingBuffer::new(cap);
        let mut model: Vec<u32> = Vec::new();
        let mut x: u32 = 7;
        for step in 0..400u32 {
            x = x.wrapping_mul(1103515245).wrapping_add(12345) % 1000;
            match x % 5 {
                0 | 1 | 2 => {
                    let dropped = r.push(step);
                    let expected = if model.len() == cap { Some(model.remove(0)) } else { None };
                    model.push(step);
                    assert_eq!(dropped, expected, "push {step} (cap {cap})");
                }
                3 => {
                    let expected = if model.is_empty() { None } else { Some(model.remove(0)) };
                    assert_eq!(r.pop_front(), expected, "pop at step {step} (cap {cap})");
                }
                _ => {
                    let n = (x as usize) % (cap + 2);
                    let want: Vec<u32> = model[model.len().saturating_sub(n)..].to_vec();
                    let got: Vec<u32> = r.last_n(n).into_iter().copied().collect();
                    assert_eq!(got, want, "last_n({n}) at step {step} (cap {cap})");
                }
            }
            assert_eq!(r.len(), model.len());
            assert_eq!(items(&r), model);
            for i in 0..cap + 2 {
                assert_eq!(r.get(i).copied(), model.get(i).copied(), "get({i}) at step {step} (cap {cap})");
            }
        }
    }
}

#[test]
fn pop_on_empty_is_none() {
    let mut r: RingBuffer<u32> = RingBuffer::new(3);
    assert_eq!(r.pop_front(), None);
    assert_eq!(r.pop_front(), None);
    r.push(1);
    assert_eq!(r.pop_front(), Some(1));
    assert_eq!(r.pop_front(), None);
    assert_eq!(r.len(), 0);
}

#[test]
fn get_past_the_end_is_none_when_full() {
    let mut r = RingBuffer::new(3);
    for i in 1..=4 {
        r.push(i);
    }
    assert_eq!(r.get(0), Some(&2));
    assert_eq!(r.get(2), Some(&4));
    assert_eq!(r.get(3), None);
    assert_eq!(r.get(100), None);
}

#[test]
fn last_n_larger_than_len() {
    let mut r = RingBuffer::new(5);
    r.push(1);
    r.push(2);
    let all: Vec<u32> = r.last_n(10).into_iter().copied().collect();
    assert_eq!(all, [1, 2]);
    assert!(r.last_n(0).is_empty());
}

#[test]
fn clear_then_reuse() {
    let mut r = RingBuffer::new(2);
    for i in 1..=3 {
        r.push(i);
    }
    r.clear();
    assert!(r.is_empty());
    r.push(9);
    assert_eq!(items(&r), [9]);
    assert_eq!(r.capacity(), 2);
}

#[test]
fn works_with_owned_strings() {
    let mut r = RingBuffer::new(2);
    r.push(String::from("a"));
    r.push(String::from("b"));
    assert_eq!(r.push(String::from("c")).as_deref(), Some("a"));
    let v: Vec<&str> = r.iter().map(|s| s.as_str()).collect();
    assert_eq!(v, ["b", "c"]);
}

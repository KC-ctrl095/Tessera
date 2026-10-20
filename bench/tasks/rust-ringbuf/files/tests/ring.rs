use ringlog::RingBuffer;

fn items(r: &RingBuffer<u32>) -> Vec<u32> {
    r.iter().copied().collect()
}

#[test]
fn keeps_order_before_wrapping() {
    let mut r = RingBuffer::new(3);
    r.push(1);
    r.push(2);
    assert_eq!(items(&r), [1, 2]);
    assert_eq!(r.len(), 2);
}

#[test]
fn drops_the_oldest_when_full() {
    let mut r = RingBuffer::new(3);
    for i in 1..=3 {
        assert_eq!(r.push(i), None);
    }
    assert_eq!(r.push(4), Some(1));
    assert_eq!(r.push(5), Some(2));
    assert_eq!(items(&r), [3, 4, 5]);
}

#[test]
fn pop_after_wrapping() {
    let mut r = RingBuffer::new(2);
    for i in 1..=5 {
        r.push(i);
    }
    assert_eq!(r.pop_front(), Some(4));
    assert_eq!(r.pop_front(), Some(5));
    assert_eq!(r.pop_front(), None);
    assert!(r.is_empty());
}

#[test]
fn last_n_gives_the_newest() {
    let mut r = RingBuffer::new(4);
    for i in 1..=6 {
        r.push(i);
    }
    let last: Vec<u32> = r.last_n(2).into_iter().copied().collect();
    assert_eq!(last, [5, 6]);
}

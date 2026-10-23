use super::*;

fn reply(from_client: bool) -> CommandReply {
    CommandReply {
        time: 1,
        number: 1,
        from_client,
        success: true,
        body: vec![],
    }
}

#[test]
fn replies_pop_in_order_and_server_blocks_pop_nothing() {
    let mut q = CommandQueue::new();
    q.push("a");
    q.push("b");
    assert_eq!(q.complete(&reply(false)), None);
    assert_eq!(q.complete(&reply(true)), Some("a"));
    assert_eq!(q.complete(&reply(true)), Some("b"));
    assert_eq!(q.complete(&reply(true)), None);
}

#[test]
fn pop_front_while_stops_at_the_first_mismatch() {
    let mut q = CommandQueue::new();
    for tag in [1, 1, 2, 1] {
        q.push(tag);
    }
    assert_eq!(q.pop_front_while(|&tag| tag == 1), vec![1, 1]);
    assert_eq!(q.pop_front_while(|&tag| tag == 1), Vec::<i32>::new());
    assert_eq!(q.drain().collect::<Vec<_>>(), vec![2, 1]);
}

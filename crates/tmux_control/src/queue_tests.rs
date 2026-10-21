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

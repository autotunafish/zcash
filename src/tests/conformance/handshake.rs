use crate::{
    protocol::{
        message::Message,
        payload::{block::Headers, Addr, Nonce, Version},
    },
    setup::{config::read_config_file, node::Node},
};

use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn handshake_responder_side() {
    // 1. Configure and run node.
    // 2. Send a Version message to the node.
    // 3. Expect a Version back and send Verack.
    // 4. Expect Verack back.

    let (_zig, node_meta) = read_config_file();

    let mut node = Node::new(node_meta);
    node.start().await;

    let mut peer_stream = TcpStream::connect(node.addr()).await.unwrap();

    Message::Version(Version::new(node.addr(), peer_stream.local_addr().unwrap()))
        .write_to_stream(&mut peer_stream)
        .await
        .unwrap();

    let version = Message::read_from_stream(&mut peer_stream).await.unwrap();
    assert!(matches!(version, Message::Version(..)));

    Message::Verack
        .write_to_stream(&mut peer_stream)
        .await
        .unwrap();

    let verack = Message::read_from_stream(&mut peer_stream).await.unwrap();
    assert!(matches!(verack, Message::Verack));

    node.stop().await;
}

#[tokio::test]
async fn handshake_initiator_side() {
    let (zig, node_meta) = read_config_file();

    let listener = TcpListener::bind(zig.new_local_addr()).await.unwrap();

    let mut node = Node::new(node_meta);
    node.initial_peers(vec![listener.local_addr().unwrap().port()])
        .start()
        .await;

    match listener.accept().await {
        Ok((mut peer_stream, addr)) => {
            let version = Message::read_from_stream(&mut peer_stream).await.unwrap();
            assert!(matches!(version, Message::Version(..)));

            Message::Version(Version::new(addr, listener.local_addr().unwrap()))
                .write_to_stream(&mut peer_stream)
                .await
                .unwrap();

            let verack = Message::read_from_stream(&mut peer_stream).await.unwrap();
            assert!(matches!(verack, Message::Verack));

            Message::Verack
                .write_to_stream(&mut peer_stream)
                .await
                .unwrap();
        }
        Err(e) => println!("couldn't get client: {:?}", e),
    }

    node.stop().await;
}

#[tokio::test]
async fn reject_non_version_replies_to_version() {
    // Conformance test 004.
    //
    // The node should reject non-Version messages in response to the initial Version it sent.
    //
    // A node can react in one of the following ways:
    //
    //  a) the message is ignored
    //  b) the connection is terminated
    //  c) responds to our message
    //  d) becomes unersponsive to future communications
    //
    // of which only (a) and (b) are valid responses. This test operates in the following manner:
    //
    // For each non-version message, create a peer node and
    //
    //  1) wait for the incoming `version` message
    //  2) send a non-version message
    //  3) send the version message
    //  4) receive a response
    //
    // We expect the following to occur for each of the possible node reactions:
    //
    //  a) (2) is ignored, therefore (3) should succeed, and (4) should be `verack`
    //  b) Node terminates the connection upon processing the message sent in (2),
    //     so either step (3) or at latest (4) should fail (timing dependent on node)
    //  c) message received in (4) is not `verack`
    //  d) steps (3) or (4) cause time out
    //
    // Due to how we instrument the test node, we need to have the list of peers ready when we start the node.
    // This implies we need each test message to operate on a separate connection concurrently.

    // todo: implement rest of the messages
    let mut test_messages = vec![
        Message::GetAddr,
        Message::MemPool,
        Message::Verack,
        Message::Ping(Nonce::default()),
        Message::Pong(Nonce::default()),
        Message::GetAddr,
        Message::Addr(Addr::empty()),
        Message::Headers(Headers::empty()),
        //Message::GetHeaders(LocatorHashes)),
        // Message::GetBlocks(LocatorHashes)),
        // Message::GetData(Inv));
        // Message::Inv(Inv));
        // Message::NotFound(Inv));
    ];

    let (zig, node_meta) = read_config_file();

    // Create and bind TCP listeners (so we have the ports ready for instantiating the node)
    let mut listeners = Vec::with_capacity(test_messages.len());
    for _ in test_messages.iter() {
        listeners.push(TcpListener::bind(zig.new_local_addr()).await.unwrap());
    }

    let ports = listeners
        .iter()
        .map(|listener| listener.local_addr().unwrap().port())
        .collect();
    let mut node = Node::new(node_meta);
    node.initial_peers(ports);

    let mut handles = Vec::with_capacity(test_messages.len());

    // create and start a future for each test message
    for _ in 0..test_messages.len() {
        let listener = listeners.pop().unwrap();
        let message = test_messages.pop().unwrap();

        handles.push(tokio::spawn(async move {
            let (mut stream, addr) = listener.accept().await.unwrap();

            // (1) receive incoming `version`
            let version = Message::read_from_stream(&mut stream).await.unwrap();
            assert!(matches!(version, Message::Version(..)));

            // (2) send non-version message
            message.write_to_stream(&mut stream).await.unwrap();

            // (3) send `version` to start our end of the handshake
            match Message::Version(Version::new(addr, listener.local_addr().unwrap()))
                .write_to_stream(&mut stream)
                .await
            {
                Ok(_) => {
                    // (4) receive `verack` in response to our `version`
                    match Message::read_from_stream(&mut stream).await {
                        Ok(message) => assert!(matches!(message, Message::Verack)),

                        Err(err) => {
                            use std::io::ErrorKind::*;
                            match err.kind() {
                                // We expect these errors if we tried to receive on a broken connection,
                                // this indicates the connection was terminated (which is a valid response).
                                UnexpectedEof | ConnectionReset | ConnectionAborted => {}
                                _ => panic!("Unexpected error while receiving: {:?}", err),
                            }
                        }
                    }
                }

                Err(err) => {
                    use std::io::ErrorKind::*;
                    match err.kind() {
                        // We expect these errors if we tried to send on a broken connection,
                        // this indicates the connection was terminated (which is a valid response).
                        BrokenPipe | ConnectionReset | ConnectionAborted => {}
                        _ => panic!("Unexpected error while sending: {:?}", err),
                    }
                }
            };
        }));
    }

    node.start().await;

    for handle in handles {
        handle.await.unwrap();
    }

    node.stop().await;
}

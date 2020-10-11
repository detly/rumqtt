use rumqttlog::router::Acks;
use rumqttlog::*;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn acks_are_returned_as_expected_to_the_connection() {
    let connections = Connections::new();
    let (connection_1_id, connection_1_rx) = connections.connection("1", 10);

    // Send data one by one
    for i in 0..1000 {
        connections.data(connection_1_id, "hello/1/world", vec![1, 2, 3], i);
    }

    let mut count = 0;
    loop {
        count += wait_for_acks(&connection_1_rx).unwrap().len();
        if count == 1000 {
            break;
        }
    }

    // Send data in bulk
    connections.datav(connection_1_id, "hello/1/world", vec![1, 2, 3], 1000);
    let mut count = 0;
    loop {
        count += wait_for_acks(&connection_1_rx).unwrap().len();
        if count == 1000 {
            break;
        }
    }
}

#[test]
fn new_connection_data_notifies_interested_connections() {
    let connections = Connections::new();
    let (connection_1_id, _connection_1_rx) = connections.connection("1", 2);
    let (connection_2_id, connection_2_rx) = connections.connection("2", 2);

    connections.subscribe(connection_2_id, "hello/+/world", 1);
    let acks = wait_for_acks(&connection_2_rx).unwrap();
    assert_eq!(acks.len(), 1);

    // Write data. 4 messages, 2 topics. Connection's capacity is 2 items.
    connections.data(connection_1_id, "hello/1/world", vec![1, 2, 3], 1);
    connections.data(connection_1_id, "hello/1/world", vec![4, 5, 6], 2);
    connections.data(connection_1_id, "hello/2/world", vec![13, 14, 15], 4);
    connections.data(connection_1_id, "hello/2/world", vec![16, 17, 18], 5);

    // Pending requests were done before. Readiness has
    // to be manually triggered

    let data = wait_for_data(&connection_2_rx).unwrap();
    assert_eq!(data.payload.len(), 2);
    assert_eq!(data.payload[0].as_ref(), &[1, 2, 3]);
    assert_eq!(data.payload[1].as_ref(), &[4, 5, 6]);

    let data = wait_for_data(&connection_2_rx).unwrap();
    assert_eq!(data.payload.len(), 2);
    assert_eq!(data.payload[0].as_ref(), &[13, 14, 15]);
    assert_eq!(data.payload[1].as_ref(), &[16, 17, 18]);
}

#[test]
fn failed_notifications_are_retried_after_connection_ready() {
    let connections = Connections::new();
    let (connection_id, connection_rx) = connections.connection("1", 3);

    // 1. First data write triggers 1 ack notification first (1st notification)
    // 2. 500 data writes
    // 3. Acks request + notification with 500 acks + acks request registration (2nd notification)
    // 4. First data write triggers 1 ack notification first (3rd notification)
    // 5. 500 data writes
    // 6. Acks request + notification with 498 acks (4th notification fails)
    // 7. Connection unscheduled from ready queue
    // 8. 998 data writes
    for i in 0..2000 {
        connections.data(connection_id, "hello/1/world", vec![1, 2, 3], i);
    }

    let count = wait_for_acks(&connection_rx).unwrap().len();
    assert_eq!(count, 1);
    let count = wait_for_acks(&connection_rx).unwrap().len();
    assert_eq!(count, 500);
    let count = wait_for_acks(&connection_rx).unwrap().len();
    assert_eq!(count, 1);

    assert!(wait_for_acks(&connection_rx).is_none());

    connections.ready(connection_id);
    let count = wait_for_acks(&connection_rx).unwrap().len();
    assert_eq!(count, 500);
    let count = wait_for_acks(&connection_rx).unwrap().len();
    assert_eq!(count, 998);
}

fn wait_for_data(rx: &Receiver<Notification>) -> Option<Data> {
    thread::sleep(Duration::from_secs(1));

    match rx.try_recv() {
        Ok(Notification::Data(reply)) => Some(reply),
        Ok(v) => {
            println!("Error = {:?}", v);
            None
        }
        Err(e) => {
            println!("Error = {:?}", e);
            None
        }
    }
}

fn wait_for_acks(rx: &Receiver<Notification>) -> Option<Acks> {
    thread::sleep(Duration::from_secs(1));

    match rx.try_recv() {
        Ok(Notification::Acks(reply)) => Some(reply),
        Ok(v) => {
            println!("Error = {:?}", v);
            None
        }
        Err(e) => {
            println!("Error = {:?}", e);
            None
        }
    }
}

// Broker is used to test router
pub(crate) struct Connections {
    router_tx: Sender<(ConnectionId, Event)>,
}

impl Connections {
    pub fn new() -> Connections {
        let mut config = Config::default();
        config.id = 0;

        let (router, router_tx) = Router::new(Arc::new(config));
        thread::spawn(move || {
            let mut router = router;
            let _ = router.start();
        });

        Connections { router_tx }
    }

    pub fn connection(&self, id: &str, cap: usize) -> (ConnectionId, Receiver<Notification>) {
        let (connection, link_rx) = rumqttlog::Connection::new_remote(id, cap);
        let message = Event::Connect(connection);
        self.router_tx.send((0, message)).unwrap();

        let connection_id = match link_rx.recv().unwrap() {
            Notification::ConnectionAck(ConnectionAck::Success(id)) => id,
            o => panic!("Unexpected connection ack = {:?}", o),
        };

        (connection_id, link_rx)
    }

    pub fn ready(&self, id: usize) {
        let message = (id, Event::Ready);
        self.router_tx.try_send(message).unwrap();
    }

    pub fn data(&self, id: usize, topic: &str, payload: Vec<u8>, pkid: u16) {
        let mut publish = Publish::new(topic, QoS::AtLeastOnce, payload);
        publish.pkid = pkid;

        let message = Event::Data(vec![Packet::Publish(publish)]);
        let message = (id, message);
        self.router_tx.send(message).unwrap();
    }

    pub fn datav(&self, id: usize, topic: &str, payload: Vec<u8>, count: u16) {
        let mut packets = Vec::new();
        for i in 0..count {
            let mut publish = Publish::new(topic, QoS::AtLeastOnce, payload.clone());
            publish.pkid = i;
            packets.push(Packet::Publish(publish));
        }

        let message = Event::Data(packets);
        let message = (id, message);
        self.router_tx.try_send(message).unwrap();
    }

    pub fn subscribe(&self, id: usize, filter: &str, pkid: u16) {
        let mut subscribe = Subscribe::new(filter, QoS::AtLeastOnce);
        subscribe.pkid = pkid;

        let message = Event::Data(vec![Packet::Subscribe(subscribe)]);
        let message = (id, message);
        self.router_tx.try_send(message).unwrap();
    }
}
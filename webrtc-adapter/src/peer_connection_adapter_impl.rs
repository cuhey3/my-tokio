use serde::Serialize;
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, MutexGuard, mpsc};
use webrtc::api::APIBuilder;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc_if::peer_connection_adapter::{PeerConnectionAdapter, PeerType};

pub struct PeerConnectionAdapterImpl {
    peer_connection: Arc<RTCPeerConnection>,
    data_channel: Option<Arc<RTCDataChannel>>,
    channel_is_ready: bool,
    peer_type: Option<PeerType>,
    offer: Option<String>,
    answer: Option<String>,
    // TODO
    // 疎通用なのでしかるべきところに保持する
    user_id: u64,
    on_open_rx: Option<Receiver<()>>,
    on_message_rx: Option<Arc<Mutex<Receiver<DataChannelMessage>>>>,
    on_data_channel_rx: Option<Receiver<Arc<RTCDataChannel>>>,
}

impl PeerConnectionAdapterImpl {
    pub fn new(user_id: u64, peer_connection: RTCPeerConnection) -> Self {
        PeerConnectionAdapterImpl {
            peer_connection: Arc::new(peer_connection),
            data_channel: None,
            channel_is_ready: false,
            peer_type: None,
            offer: None,
            answer: None,
            user_id,
            on_open_rx: None,
            on_message_rx: None,
            on_data_channel_rx: None,
        }
    }

    fn check_peer_type(&self, expect_peer_type: PeerType) -> Result<(), String> {
        let real_peer_type = &self.peer_type.as_ref().ok_or("peer type not set")?;

        if match expect_peer_type {
            PeerType::Offerer => matches!(real_peer_type, PeerType::Offerer),
            PeerType::Answerer => matches!(real_peer_type, PeerType::Answerer),
        } {
            Ok(())
        } else {
            Err(format!(
                "peer_type not match real: {:?}, expected: {:?}",
                real_peer_type, expect_peer_type
            ))
        }
    }

    pub async fn get_message_receiver(
        &self,
    ) -> Result<MutexGuard<'_, Receiver<DataChannelMessage>>, String> {
        if !self.channel_is_ready {
            return Err("data channel is not ready".into());
        }

        let on_message_rx = &self
            .on_message_rx
            .as_ref()
            .ok_or("message receiver not set")?;

        Ok(on_message_rx.lock().await)
    }
}

impl PeerConnectionAdapter for PeerConnectionAdapterImpl {
    async fn create_offer(&mut self) -> Result<String, String> {
        self.peer_connection
            .on_peer_connection_state_change(Box::new(|s| {
                println!("on_peer_connection_state_change offer {:?}", s);
                Box::pin(async move {})
            }));

        self.peer_connection.on_negotiation_needed(Box::new(|| {
            println!("on_negotiation_needed");
            Box::pin(async move {})
        }));

        self.peer_connection.on_ice_candidate(Box::new(|c| {
            println!("on_ice_candidate {:?}", c);
            Box::pin(async move {})
        }));

        let data_channel = self
            .peer_connection
            .create_data_channel("data", None)
            .await
            .map_err(|err| format!("create data channel failed: {}", err))?;

        let (on_open_tx, on_open_rx) = mpsc::channel::<()>(1);

        let (on_message_tx, on_message_rx) = mpsc::channel::<DataChannelMessage>(1);

        set_default_data_channel_handlers("offerer", &data_channel, on_open_tx, on_message_tx)
            .map_err(|err| format!("set default data channel handlers failed: {}", err))?;

        self.data_channel = Some(data_channel);

        let offer = self
            .peer_connection
            .create_offer(None)
            .await
            .map_err(|err| format!("create offer failed: {}", err))?;

        self.peer_connection
            .set_local_description(offer)
            .await
            .map_err(|err| format!("set local description failed: {}", err))?;

        self.peer_connection
            .gathering_complete_promise()
            .await
            .recv()
            .await;

        let local_desc = self
            .peer_connection
            .local_description()
            .await
            .ok_or("get local description failed")?;

        let offer = serde_json::to_string(&local_desc)
            .map_err(|err| format!("serialize offer failed: {}", err))?;

        self.peer_type = Some(PeerType::Offerer);

        self.offer = Some(offer.clone());

        self.on_open_rx = Some(on_open_rx);

        self.on_message_rx = Some(Arc::new(Mutex::new(on_message_rx)));

        Ok(offer)
    }

    fn get_offer(&self) -> Result<String, String> {
        println!("get offer");
        self.check_peer_type(PeerType::Offerer)?;

        let offer = self.offer.as_ref().ok_or("offer does not created")?.clone();

        Ok(offer)
    }

    fn set_answer(&mut self, answer: &str) -> Result<(), String> {
        println!("set answer");
        self.check_peer_type(PeerType::Offerer)?;

        self.answer = Some(answer.to_owned());

        Ok(())
    }

    async fn load_answer(&mut self) -> Result<(), String> {
        println!("loading answer");
        self.check_peer_type(PeerType::Offerer)?;

        let answer = self.answer.take().ok_or("answer not set")?;

        let answer = serde_json::from_str::<RTCSessionDescription>(&answer)
            .map_err(|err| format!("answer deserialize error: {}", err))?;

        self.peer_connection
            .set_remote_description(answer)
            .await
            .map_err(|err| format!("load answer failed: {}", err))?;

        self.on_open_rx
            .take()
            .ok_or("on_open_rx not set")?
            .recv()
            .await;

        self.channel_is_ready = true;

        Ok(())
    }

    async fn ready_to_open_data_channel(&mut self) -> Result<(), String> {
        self.on_data_channel_rx
            .as_ref()
            .ok_or("on_data_channel_rx is not ready")?;

        self.on_open_rx.as_ref().ok_or("on_open_rx is not ready")?;

        self.data_channel = self.on_data_channel_rx.as_mut().unwrap().recv().await;

        self.data_channel
            .as_ref()
            .ok_or("cannot get data_channel from on_data_channel_rx")?;

        self.on_open_rx.as_mut().unwrap().recv().await;

        self.channel_is_ready = true;

        Ok(())
    }

    async fn create_answer_from_offer(&mut self, offer: &str) -> Result<(), String> {
        let (on_message_tx, on_message_rx) = tokio::sync::mpsc::channel::<DataChannelMessage>(1);
        let (on_open_tx, on_open_rx) = mpsc::channel::<()>(1);
        let (channel_tx, channel_rx) = tokio::sync::mpsc::channel::<Arc<RTCDataChannel>>(1);

        self.peer_connection.on_negotiation_needed(Box::new(|| {
            println!("on_negotiation_needed");
            Box::pin(async move {})
        }));

        self.peer_connection.on_ice_candidate(Box::new(|c| {
            println!("on_ice_candidate {:?}", c);
            Box::pin(async move {})
        }));

        self.peer_connection
            .on_peer_connection_state_change(Box::new(|s| {
                println!("on_peer_connection_state_change answer {:?}", s);
                Box::pin(async move {})
            }));

        self.peer_connection
            .on_data_channel(Box::new(move |data_channel: Arc<RTCDataChannel>| {
                let _ = set_default_data_channel_handlers(
                    "answerer",
                    &data_channel,
                    on_open_tx.clone(),
                    on_message_tx.clone(),
                );
                let _ = channel_tx.try_send(data_channel.clone());
                Box::pin(async move {})
            }));

        let offer = serde_json::from_str::<RTCSessionDescription>(offer)
            .map_err(|err| format!("deserialize offer failed: {}", err))?;

        self.peer_connection
            .set_remote_description(offer)
            .await
            .map_err(|err| format!("set remote description failed: {}", err))?;

        let answer = self
            .peer_connection
            .create_answer(None)
            .await
            .map_err(|err| format!("create answer failed: {}", err))?;

        self.peer_connection
            .set_local_description(answer.clone())
            .await
            .map_err(|err| format!("set local description failed: {}", err))?;

        let answer = serde_json::to_string(&answer)
            .map_err(|err| format!("serialize answer failed: {}", err))?;

        self.peer_type = Some(PeerType::Answerer);
        self.answer = Some(answer);
        self.on_open_rx = Some(on_open_rx);
        self.on_message_rx = Some(Arc::new(Mutex::new(on_message_rx)));
        self.on_data_channel_rx = Some(channel_rx);
        Ok(())
    }

    fn get_answer(&self) -> Result<String, String> {
        self.check_peer_type(PeerType::Answerer)?;

        let answer = self
            .answer
            .as_ref()
            .ok_or("answer does not created")?
            .clone();

        Ok(answer)
    }

    fn is_offerer(&self) -> bool {
        matches!(self.peer_type, Some(PeerType::Offerer))
    }

    async fn send_json(&self, json: &str) -> Result<usize, String> {
        if !self.channel_is_ready {
            return Err("data channel is not ready".into());
        }

        self.data_channel
            .as_ref()
            .unwrap()
            .send_text(json)
            .await
            .map_err(|err| format!("data channel send error: {}", err))
    }

    async fn wait_message_json(&self) -> Result<String, String> {
        let message = self
            .get_message_receiver()
            .await
            .map_err(|err| format!("getting message receiver failed: {err}"))?
            .recv()
            .await
            .ok_or("message receiver closed unexpected")?;
        let result = std::str::from_utf8(&message.data)
            .map_err(|err| format!("message received invalid UTF-8: {err}"))?;
        Ok(result.to_owned())
    }

    async fn create_connection_wrapper(user_id: u64) -> Result<Self, String> {
        let registry = Registry::new();
        let api = APIBuilder::new()
            .with_interceptor_registry(registry)
            .build();

        let rtc_configuration = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let Ok(peer_connection) = api.new_peer_connection(rtc_configuration).await else {
            return Err("create peer connection failed.".into());
        };

        Ok(Self::new(user_id, peer_connection))
    }
    fn get_user_id(&self) -> &u64 {
        &self.user_id
    }

    async fn send_data<T: Serialize + Sync>(&self, data: &T) -> Result<usize, String> {
        if !self.channel_is_ready {
            return Err("data channel is not ready".into());
        }

        self.data_channel
            .as_ref()
            .unwrap()
            .send_text(serde_json::to_string(&data).unwrap())
            .await
            .map_err(|err| format!("data channel send error: {}", err))
    }

    async fn close(&mut self) -> Result<(), String> {
        self.data_channel
            .take()
            .ok_or("data channel not set")?
            .close()
            .await
            .map_err(|err| format!("data channel close error: {}", err))
    }
}

fn set_default_data_channel_handlers(
    channel_id: &str,
    data_channel: &Arc<RTCDataChannel>,
    on_open_tx: Sender<()>,
    on_message_tx: Sender<DataChannelMessage>,
) -> Result<(), String> {
    let channel_id_clone = channel_id.to_owned();
    data_channel.on_open(Box::new(move || {
        println!("{channel_id_clone} on_open create answer");
        let _ = on_open_tx.try_send(());
        Box::pin(async move {})
    }));

    let channel_id_clone = channel_id.to_owned();
    data_channel.on_error(Box::new(move |error| {
        println!("{channel_id_clone} on_error create answer {:?}", error);
        Box::pin(async move {})
    }));

    let channel_id_clone = channel_id.to_owned();
    data_channel.on_close(Box::new(move || {
        println!("{channel_id_clone} on_close create answer");
        Box::pin(async move {})
    }));

    let channel_id_clone = channel_id.to_owned();
    data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
        println!("{channel_id_clone} Message from DataChannel {:?}", msg);
        let _ = on_message_tx.try_send(msg);
        Box::pin(async {})
    }));

    Ok(())
}

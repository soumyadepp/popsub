use tokio::sync::mpsc::UnboundedSender;
use tungstenite::protocol::Message as WsMessage;

#[derive(Debug)]
pub struct Client {
    pub id: String,
    pub sender: UnboundedSender<WsMessage>,
}

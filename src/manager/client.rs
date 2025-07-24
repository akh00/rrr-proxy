use futures::{channel::mpsc, future::select, pin_mut};
use log::debug;
use tokio_tungstenite::connect_async;
use tungstenite::Message;

pub struct ManagerClient {
    url: String,
    timeout: u64,
}

impl ManagerClient {
    fn new(url: String) -> Self {
        ManagerClient { url, timeout: 5000 }
    }
    async fn connect(self, receiver: mpsc::UnboundedReceiver<Message>) -> () {
        use futures::stream::StreamExt;
        match connect_async(&self.url).await {
            Ok((ws_stream, _response)) => {
                let (write, mut read) = ws_stream.split();
                let mpsc_to_ws = receiver.map(Message::from).map(Ok).forward(write);
                let ws_to_mpsc = async {
                    while let Some(message) = read.next().await {
                        debug!("Received {:?}", message);
                    }
                };

                pin_mut!(mpsc_to_ws, ws_to_mpsc);
                select(mpsc_to_ws, ws_to_mpsc).await;
            }
            Err(error) => {
                log::error!("Unhandled WS msg: {:?}", error);
            }
        }
        log::error!("Connection failed");
    }
    fn register(&mut self) -> Result<(), u16> {
        Ok(())
    }
}

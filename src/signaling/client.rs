
use futures::{channel::mpsc, pin_mut, future::select};
use tokio_tungstenite::connect_async;
use tungstenite::Message;
use log::{debug, error, log_enabled, info, Level};


pub struct SignalingClient {
    url: String,
    timeout: u64,
}

impl SignalingClient {
    fn new(url: String) -> Self {
        SignalingClient {url, timeout: 5000}
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
    fn register(&mut self) -> Result<(),u16> {
        Ok(())
    }
}
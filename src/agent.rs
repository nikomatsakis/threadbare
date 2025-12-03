use std::str::FromStr;

use anyhow::Context;
use sacp::{
    Channel, JrConnectionCx, JrHandlerChain,
    schema::{
        ContentBlock, NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse,
        SessionNotification, SessionUpdate,
    },
};
use sacp_conductor::{Conductor, McpBridgeMode};
use sacp_proxy::McpServiceRegistry;
use tokio::sync::mpsc::{self, Receiver, Sender, channel};

use crate::{ast, interpreter::ThinkResponse};

#[derive(Clone)]
pub struct Agent {
    tx: Sender<AcpActorMessage>,
}

impl Agent {
    pub fn spawn() -> Self {
        let (tx, rx) = channel(128);
        tokio::spawn(async move { Self::run(rx).await });
        Self { tx }
    }

    async fn run(mut rx: Receiver<AcpActorMessage>) -> Result<(), sacp::Error> {
        // Create a client talking to this agent...
        let (session_update_tx, mut session_update_rx) = mpsc::unbounded_channel();

        // Create a MCP server that we will offer
        let mcp_registry = McpServiceRegistry::new();

        JrHandlerChain::new()
            // When we receive session/update notifications,
            // we send them to `session_update_tx` so we can pick them off elsewhere.
            // There should be a more convenient way to deal with this.
            .on_receive_notification(async move |notification: SessionNotification, cx| {
                session_update_tx
                    .send(notification)
                    .map_err(sacp::util::internal_error)
            })
            .connect_to(Conductor::new(
                "agent".to_string(),
                vec![sacp_tokio::AcpAgent::from_args(vec![
                    "npx",
                    "-y",
                    "@zed-industries/claude-code-acp",
                ])?],
                McpBridgeMode::Http,
            ))?
            .with_client(async move |cx: JrConnectionCx| {
                while let Some(message) = rx.recv().await {
                    match message {
                        AcpActorMessage::Think { prompt, tx } => {
                            // Start the session
                            let NewSessionResponse {
                                session_id,
                                modes: _,
                                meta: _,
                            } = cx
                                .send_request(NewSessionRequest {
                                    cwd: std::env::current_dir()
                                        .expect("can get current directory"),
                                    mcp_servers: vec![],
                                    meta: None,
                                })
                                .block_task()
                                .await?;

                            // Send the prompt and await the end of the return.
                            let PromptResponse {
                                stop_reason,
                                meta: _,
                            } = cx
                                .send_request(PromptRequest {
                                    session_id,
                                    prompt: vec![prompt.into()],
                                    meta: None,
                                })
                                .block_task()
                                .await?;
                            let sacp::schema::StopReason::EndTurn = stop_reason else {
                                return Err(sacp::util::internal_error(format!(
                                    "unexpected stop reason: {stop_reason:?}"
                                )));
                            };

                            // Read all the updates we got.
                            let mut result = String::new();
                            while let Ok(message) = session_update_rx.try_recv() {
                                assert_eq!(message.session_id, session_id);
                                if let SessionUpdate::AgentMessageChunk(content_chunk) =
                                    message.update
                                {
                                    if let ContentBlock::Text(text_content) = content_chunk.content
                                    {
                                        result.push_str(text_content);
                                    }
                                }
                            }

                            tx.send(ThinkResponse::Complete { message: result })
                                .await
                                .map_err(sacp::util::internal_error)?;
                        }
                    }
                }
                Ok(())
            })
            .await
    }
}

pub enum AcpActorMessage {
    Think {
        prompt: String,
        tx: Sender<ThinkResponse>,
    },
}

use sacp::{
    JrConnectionCx, JrHandlerChain,
    schema::{
        ContentBlock, NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse,
        SessionNotification, SessionUpdate,
    },
};
use sacp_conductor::{Conductor, McpBridgeMode};
use sacp_proxy::{McpServer, McpServiceRegistry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Sender, UnboundedReceiver, UnboundedSender, channel, unbounded_channel};

use crate::interpreter::ThinkResponse;

#[derive(Clone)]
pub struct Agent {
    tx: UnboundedSender<AcpActorMessage>,
}

enum RedirectMessage {
    IncomingMessage(PerSessionMessage),
    PushThinker(Sender<PerSessionMessage>),
    PopThinker,
}

enum PerSessionMessage {
    /// Notification from the agent
    SessionNotification(SessionNotification),

    /// Agent invoked the "do" tool
    DoInvocation(DoArg, tokio::sync::oneshot::Sender<String>),

    /// Agent completed their turn
    PromptResponse(PromptResponse),
}

impl Agent {
    pub fn spawn() -> Self {
        let (tx, rx) = unbounded_channel();
        tokio::spawn(async move { Self::run(rx).await });
        Self { tx }
    }

    pub fn send_prompt(&mut self, message: AcpActorMessage) -> Result<(), sacp::Error> {
        self.tx.send(message).map_err(sacp::util::internal_error)
    }

    async fn run(mut rx: UnboundedReceiver<AcpActorMessage>) -> Result<(), sacp::Error> {
        // OK, I am a horrible monster and I pray for death. At the moment,
        // we cannot add a distinct MCP server that is unique to a *particular* session
        // nor can we easily receive messages targeting a specific session-id.
        // Therefore, we have this horrific hack: we funnel ALL messages to a common
        // place, where they get redirected out based on the session.
        // Create a shared vector where we dump updates
        let (main_loop_tx, main_loop_rx) = unbounded_channel();

        // Create a MCP server that we will offer
        let mcp_registry =
            McpServiceRegistry::new().with_mcp_server("patchwork", McpServerActor::new(main_loop_tx.clone()).server())?;

        JrHandlerChain::new()
            // Handle MCP registry events
            .with_handler(mcp_registry.clone())
            // When we receive session/update notifications,
            // we send them to `session_update_tx` so we can pick them off elsewhere.
            // There should be a more convenient way to deal with this.
            .on_receive_notification({
                let main_loop_tx = main_loop_tx.clone();
                async move |notification: SessionNotification, _cx| {
                    main_loop_tx
                        .send(RedirectMessage::IncomingMessage(
                            PerSessionMessage::SessionNotification(notification),
                        ))
                        .map_err(sacp::util::internal_error)?;
                    Ok(())
                }
            })
            // Talk to Claude Code; the conductor is used to manage MCP-over-ACP messages.
            .connect_to(Conductor::new(
                "agent".to_string(),
                vec![sacp_tokio::AcpAgent::from_args(vec![
                    "npx",
                    "-y",
                    "@zed-industries/claude-code-acp",
                ])?],
                McpBridgeMode::Http,
            ))?
            // Get a client handle
            .with_client(async move |cx| {
                // Task 1: receive messages for the "main"
                cx.spawn(Self::redirect_actor(cx.clone(), main_loop_rx))?;

                // Task 2: receive new "think" requests from the interpreter
                // Receive a request from the interpreter
                while let Some(message) = rx.recv().await {
                    match message {
                        AcpActorMessage::Think { prompt, tx } => {
                            cx.spawn(Self::think_message(
                                cx.clone(),
                                prompt,
                                tx,
                                main_loop_tx.clone(),
                            ))?;
                        }
                    }
                }

                Ok(())
            })
            .await
    }

    /// The redirect actor keeps a stack of active "think" requests.
    /// And sends
    async fn redirect_actor(
        _cx: JrConnectionCx,
        mut main_loop_rx: tokio::sync::mpsc::UnboundedReceiver<RedirectMessage>,
    ) -> Result<(), sacp::Error> {
        let mut stack: Vec<Sender<PerSessionMessage>> = vec![];
        while let Some(redirect_message) = main_loop_rx.recv().await {
            // Store the updat
            match redirect_message {
                RedirectMessage::IncomingMessage(incoming_message) => {
                    if let Some(sender) = stack.last() {
                        sender
                            .send(incoming_message)
                            .await
                            .expect("sender to be alive");
                    }
                }

                RedirectMessage::PushThinker(sender) => {
                    stack.push(sender);
                }

                RedirectMessage::PopThinker => {
                    stack.pop().expect("stack to be non-empty");
                }
            }
        }
        Ok(())
    }

    async fn think_message(
        cx: JrConnectionCx,
        prompt: String,
        tx: std::sync::mpsc::Sender<ThinkResponse>,
        main_loop_tx: tokio::sync::mpsc::UnboundedSender<RedirectMessage>,
    ) -> Result<(), sacp::Error> {
        // Start the session
        let NewSessionResponse {
            session_id,
            modes: _,
            meta: _,
        } = cx
            .send_request(NewSessionRequest {
                cwd: std::env::current_dir().expect("can get current directory"),
                mcp_servers: vec![],
                meta: None,
            })
            .block_task()
            .await?;

        // Tell the main loop that we have a new thinker on the top of the stack
        let (think_tx, mut think_rx) = channel(128);
        main_loop_tx
            .send(RedirectMessage::PushThinker(think_tx))
            .expect("main loop to be alive");

        // Start the prompt. When we get the response, send that to the main loop too.
        cx.send_request(PromptRequest {
            session_id: session_id.clone(),
            prompt: vec![prompt.into()],
            meta: None,
        })
        .await_when_result_received({
            let main_loop_tx = main_loop_tx.clone();
            async move |response| {
                main_loop_tx
                    .send(RedirectMessage::IncomingMessage(
                        PerSessionMessage::PromptResponse(response?),
                    ))
                    .map_err(sacp::util::internal_error)
            }
        })?;

        // Now read all the updates out from `think_rx`
        let mut result = String::new();
        while let Some(message) = think_rx.recv().await {
            match message {
                // Agent sent us some text. Accumulate it.
                PerSessionMessage::SessionNotification(notification) => {
                    // We received a session update.
                    if let SessionUpdate::AgentMessageChunk(content_chunk) = notification.update {
                        if let ContentBlock::Text(text_content) = content_chunk.content {
                            result.push_str(&text_content.text);
                        }
                    }
                }

                // Agent invoked the "do" tool. Tell the interpreter to "do" it,
                // passing along the `do_tx` where the result should go.
                PerSessionMessage::DoInvocation(DoArg { number }, do_tx) => {
                    tx.send(ThinkResponse::Do {
                        uuid: number,
                        do_tx,
                    })
                    .expect("do-er to be listening");
                }

                // Agent finished their turn, huzzah!
                PerSessionMessage::PromptResponse(prompt_response) => {
                    match prompt_response.stop_reason {
                        sacp::schema::StopReason::EndTurn => {
                            break;
                        }
                        reason => {
                            return Err(sacp::util::internal_error(
                                format!("unexpected stop reason from agent: {reason:?}"),
                            ));
                        }
                    }
                }
            }
        }

        tx.send(ThinkResponse::Complete { message: result })
            .map_err(sacp::util::internal_error)?;

        main_loop_tx
            .send(RedirectMessage::PopThinker)
            .expect("main loop to be alive");

        Ok(())
    }
}

pub enum AcpActorMessage {
    Think {
        prompt: String,
        tx: std::sync::mpsc::Sender<ThinkResponse>,
    },
}

pub struct McpServerActor {
    main_loop_tx: tokio::sync::mpsc::UnboundedSender<RedirectMessage>,
}

#[derive(JsonSchema, Deserialize, Serialize)]
struct DoArg {
    number: usize,
}

#[derive(JsonSchema, Deserialize, Serialize)]
struct DoResult {
    text: String,
}

impl McpServerActor {
    fn new(
        main_loop_tx: tokio::sync::mpsc::UnboundedSender<RedirectMessage>,
    ) -> Self {
        Self { main_loop_tx }
    }

    fn server(self) -> McpServer {
        McpServer::new().instructions("foo").tool_fn(
            "do",
            "bar",
            async move |arg: DoArg, _cx| -> Result<DoResult, sacp::Error> {
                let (do_tx, do_rx) = tokio::sync::oneshot::channel();
                self.main_loop_tx.send(RedirectMessage::IncomingMessage(PerSessionMessage::DoInvocation(arg, do_tx))).map_err(sacp::util::internal_error)?;
                Ok(DoResult {
                    text: do_rx.await.map_err(sacp::util::internal_error)?
                })
            },
            |f, a, b| Box::pin(f(a, b)),
        )
    }
}

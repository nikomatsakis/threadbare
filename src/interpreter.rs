use crate::{agent::{AcpActorMessage, Agent}, ast::{Ast, Think}};

pub enum ThinkResponse {
    /// The thinker requested us to "do" something
    Do { 
        /// uuid of code fragment to eval        
        uuid: usize,

        /// where to send the result once we have "done" it
        do_tx: tokio::sync::oneshot::Sender<String>
    },

    Complete { message: String },
}

pub struct Interpreter {
    agent: Agent,
}

impl Interpreter {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent,
        }
    }

    pub fn run(mut self, ast: &Ast) -> Result<String, sacp::Error> {
        self.interpret(ast)
    }

    fn interpret(&mut self, ast: &Ast) -> Result<String, sacp::Error> {
        let mut output = String::new();
        match ast {
            Ast::Print { message } => {
                output.push_str(message);
                output.push_str("\n");
            }
            Ast::Do { children } => {
                for child in children {
                    self.interpret(child)?;
                }
            }
            Ast::Think { think } => {
                // Thinking loop
                let result = self.think(think)?;
                output.push_str(&result);
            }
        }
        Ok(output)
    }

    fn think(&mut self, think: &Think) -> Result<String, sacp::Error> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.agent.send_prompt(AcpActorMessage::Think { prompt: think.prompt.to_string(), tx })?;
        for response in rx {
            match response {
                ThinkResponse::Do { uuid, do_tx } => {
                    println!("Evaluating subroutine with UUID: {}", uuid);
                    let result = self.interpret(&think.children[uuid])?;
                    do_tx.send(result).expect("do_tx to be connected");
                }
                ThinkResponse::Complete { message } => {
                    println!("Thought complete with message: {}", message);
                    return Ok(message);
                }
            }
        }
        panic!("terminated without completion message")
    }
}

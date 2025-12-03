use std::fs::File;

use crate::{ast::Ast, interpreter::Interpreter};

mod ast;

mod interpreter;

mod agent;

#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    let agent = agent::Agent::spawn();
    for arg in std::env::args() {
        let file = File::open(arg)?;
        let program: Ast = serde_json::from_reader(file)?;
        let output = Interpreter::new(agent.clone()).run(&program)?;
        println!("{output}");
    }
    Ok(())
}

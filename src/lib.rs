use std::fs::File;

use crate::{ast::Ast, interpreter::interpret};

mod ast;

mod interpreter;

mod agent;

mod thinker_acp;

#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    for arg in std::env::args() {
        let file = File::open(arg)?;
        let program: Ast = serde_json::from_reader(file)?;
        interpret(&program);
    }
    Ok(())
}

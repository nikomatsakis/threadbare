use crate::ast::Ast;

pub enum ThinkResponse {
    Eval { uuid: usize },

    Complete { message: String },
}

pub fn interpret(
    ast: &Ast,
) {
    match ast {
        Ast::Print { message } => {
            println!("{message}");
        }
        Ast::Do { children } => {
            for child in children {
                interpret(child);
            }
        }
        Ast::Think { prompt, children } => {
            // Thinking loop

            let (tx, rx) =
        }
    }
}
